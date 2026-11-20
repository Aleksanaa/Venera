use crate::store::{ExportRange, Result, export_message, has_raw};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::services::{ServeDir, ServeFile};
use venera_debug_protocol::{Filter, LogRecord, Target, matches_filter, v1::DecimalU64};

#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}
fn bad(error: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, error.to_string())
}

#[derive(Default, Deserialize)]
struct Filters {
    component: Option<String>,
    level: Option<String>,
    program: Option<String>,
    pid: Option<String>,
    start_time_filetime: Option<String>,
    boot_time_filetime: Option<String>,
    start_key: Option<String>,
    include_descendants: Option<String>,
    q: Option<String>,
    from: Option<String>,
    until: Option<String>,
}
impl Filters {
    fn range(&self) -> std::result::Result<ExportRange, ApiError> {
        ExportRange::parse(self.from.as_deref(), self.until.as_deref()).map_err(bad)
    }
    fn instance(&self) -> std::result::Result<Option<Filter>, ApiError> {
        match (&self.pid, &self.start_time_filetime) {
            (Some(pid), Some(start)) => {
                let canonical = |value: &str| {
                    value
                        .parse::<DecimalU64>()
                        .map(|n| n.to_string())
                        .map_err(bad)
                };
                let include_descendants = self
                    .include_descendants
                    .as_deref()
                    .map(str::parse::<bool>)
                    .transpose()
                    .map_err(bad)?
                    .unwrap_or(false);
                if self.start_key.is_some() && self.boot_time_filetime.is_none() {
                    return Err(bad("Process start key requires boot_time_filetime"));
                }
                Ok(Some(Filter {
                    targets: vec![Target::ProcessInstance {
                        pid: pid.parse().map_err(bad)?,
                        start_time_filetime: canonical(start)?,
                        boot_time_filetime: self
                            .boot_time_filetime
                            .as_deref()
                            .map(canonical)
                            .transpose()?,
                        start_key: self.start_key.as_deref().map(canonical).transpose()?,
                        include_descendants,
                    }],
                    ..Filter::default()
                }))
            }
            (None, None)
                if self.boot_time_filetime.is_none()
                    && self.start_key.is_none()
                    && self.include_descendants.is_none() =>
            {
                Ok(None)
            }
            _ => Err(bad(
                "Process instance filters require pid and start_time_filetime",
            )),
        }
    }
    fn matches(&self, message: &Value, instance: Option<&Filter>) -> bool {
        let text = |key: &str| message.get(key).and_then(Value::as_str).unwrap_or("");
        let contains = |haystack: &str, needle: &str| {
            haystack
                .to_ascii_lowercase()
                .contains(&needle.to_ascii_lowercase())
        };
        self.component
            .as_ref()
            .is_none_or(|v| text("component").eq_ignore_ascii_case(v))
            && self
                .level
                .as_ref()
                .is_none_or(|v| text("level").eq_ignore_ascii_case(v))
            && self.program.as_ref().is_none_or(|v| {
                contains(message["process"]["exe_path"].as_str().unwrap_or(""), v)
                    || message["process"]["pid"]
                        .as_u64()
                        .is_some_and(|pid| pid.to_string() == *v)
            })
            && self.q.as_ref().is_none_or(|v| contains(text("text"), v))
            && instance.is_none_or(|filter| {
                message["type"] == "log"
                    && LogRecord::deserialize(message)
                        .is_ok_and(|record| matches_filter(filter, &record, &record.ancestors))
            })
    }
}
#[derive(Deserialize)]
struct EventsQuery {
    limit: Option<usize>,
    before: Option<String>,
    after: Option<String>,
    #[serde(flatten)]
    filters: Filters,
}
fn id(value: &str) -> std::result::Result<i64, ApiError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad("Event ID must be a decimal integer"));
    }
    value.parse().map_err(bad)
}
fn open(path: &std::path::Path) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(2))?;
    Ok(db)
}

fn visible(db: &Connection) -> Result<&'static str> {
    let has_control = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('events') WHERE name='revision_target')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(if has_control {
        "revision_target IS NULL"
    } else {
        "1"
    })
}
fn event(id: i64, received: String, message: String, raw: Option<String>) -> Result<Value> {
    let value: Value = serde_json::from_str(&message)?;
    let raw_message = raw.unwrap_or_else(|| message.clone());
    Ok(
        json!({"id":id.to_string(), "received_at":received, "message":value, "raw_message":raw_message}),
    )
}

async fn events(
    State(path): State<PathBuf>,
    query: std::result::Result<Query<EventsQuery>, QueryRejection>,
) -> std::result::Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(bad)?;
    let limit = query.limit.unwrap_or(200);
    if !(1..=1000).contains(&limit) {
        return Err(bad("Limit must be between 1 and 1000"));
    }
    if query.before.is_some() && query.after.is_some() {
        return Err(bad("Choose one event cursor"));
    }
    let before = query.before.as_deref().map(id).transpose()?;
    let after = query.after.as_deref().map(id).transpose()?;
    let range = query.filters.range()?;
    let instance = query.filters.instance()?;
    tokio::task::spawn_blocking(move || -> std::result::Result<Json<Value>, ApiError> {
        let mut conn = open(&path).map_err(internal)?;
        let db = conn.transaction().map_err(internal)?;
        let raw = if has_raw(&db).map_err(internal)? { "raw_message" } else { "NULL" };
        let visible = visible(&db).map_err(internal)?;
        let (oldest, latest): (Option<i64>, Option<i64>) = db.query_row(&format!("SELECT MIN(id),MAX(id) FROM events WHERE {visible}"), [], |r| Ok((r.get(0)?, r.get(1)?))).map_err(internal)?;
        let upper: i64 = db.query_row("SELECT COALESCE(MAX(id),0) FROM events", [], |r| r.get(0)).map_err(internal)?;
        let sql = if after.is_some() && visible != "1" {
            // Each revision advances the scan at its own ID but delivers the current
            // projection at the original row ID, including rows leaving the filter.
            "SELECT e.id, COALESCE(t.id,e.id), COALESCE(t.received_at,e.received_at),
                    COALESCE(t.message,e.message), CASE WHEN t.id IS NULL THEN e.raw_message ELSE t.raw_message END, t.id IS NOT NULL
             FROM events e LEFT JOIN events t ON t.journal_id=e.journal_id AND t.position=e.revision_target
             WHERE e.id>?1 AND e.id<=?2 AND (e.revision_target IS NULL OR t.id IS NOT NULL) ORDER BY e.id".into()
        } else {
            let (condition, order) = if after.is_some() { ("id>?1", "ASC") } else { ("(?1 IS NULL OR id<?1)", "DESC") };
            format!("SELECT id,id,received_at,message,{raw},0 FROM events WHERE {visible} AND {condition} AND id<=?2 ORDER BY id {order}")
        };
        let mut statement = db.prepare(&sql).map_err(internal)?;
        let cursor = after.or(before);
        let mut rows = statement.query(params![cursor, upper]).map_err(internal)?;
        let mut events = Vec::new();
        let mut changes = Vec::new();
        let mut next = upper.max(after.unwrap_or(0));
        while let Some(row) = rows.next().map_err(internal)? {
            let value = event(row.get(1).map_err(internal)?, row.get(2).map_err(internal)?, row.get(3).map_err(internal)?, row.get(4).map_err(internal)?).map_err(internal)?;
            let matches = range.contains(value["received_at"].as_str().unwrap()).map_err(internal)? && query.filters.matches(&value["message"], instance.as_ref());
            if row.get::<_,bool>(5).map_err(internal)? {
                changes.push(json!({"id":value["id"],"event":matches.then_some(&value)}));
            } else if matches {
                events.push(value);
            }
            if events.len() + changes.len() == limit {
                if after.is_some() { next = row.get(0).map_err(internal)?; }
                break;
            }
        }
        if after.is_none() { events.reverse(); }
        Ok(Json(json!({"events":events,"changes":changes,"next_id":next.to_string(),"oldest_id":oldest.map(|v| v.to_string()),"latest_id":latest.map(|v| v.to_string())})))
    }).await.map_err(internal)?
}
async fn detail(
    State(path): State<PathBuf>,
    Path(value): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let id = id(&value)?;
    tokio::task::spawn_blocking(move || {
        let db = open(&path).map_err(internal)?;
        let raw = if has_raw(&db).map_err(internal)? {
            "raw_message"
        } else {
            "NULL"
        };
        let visible = visible(&db).map_err(internal)?;
        let row: Option<(String, String, Option<String>)> = db
            .query_row(
                &format!("SELECT received_at,message,{raw} FROM events WHERE id=?1 AND {visible}"),
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(internal)?;
        let (received, message, raw) =
            row.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Event not found".into()))?;
        event(id, received, message, raw)
            .map(Json)
            .map_err(internal)
    })
    .await
    .map_err(internal)?
}

async fn export(
    State(path): State<PathBuf>,
    query: std::result::Result<Query<Filters>, QueryRejection>,
) -> std::result::Result<Response, ApiError> {
    let Query(filters) = query.map_err(bad)?;
    let range = filters.range()?;
    let instance = filters.instance()?;
    let mut cursor = tokio::task::spawn_blocking(move || {
        crate::store::ExportCursor::open(&path).map_err(internal)
    })
    .await
    .map_err(internal)??;
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    tokio::task::spawn_blocking(move || {
        let result = (|| -> Result<()> {
            while let Some((received, projection, raw)) = cursor.next_row()? {
                let message: Value = serde_json::from_str(&projection)?;
                if !range.contains(&received)? || !filters.matches(&message, instance.as_ref()) {
                    continue;
                }
                let raw = export_message(&raw)?;
                let line = format!(
                    "{{\"received_at\":{},\"message\":{}}}\n",
                    serde_json::to_string(&received)?,
                    raw
                );
                if sender
                    .blocking_send(Ok::<_, std::io::Error>(Bytes::from(line)))
                    .is_err()
                {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = sender.blocking_send(Err(std::io::Error::other(error.to_string())));
        }
    });
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=events.jsonl",
            ),
        ],
        Body::from_stream(ReceiverStream::new(receiver)),
    )
        .into_response())
}
async fn api_missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "API route not found".into())
}

async fn control_query(
    control: PathBuf,
    kind: &'static str,
) -> std::result::Result<Json<Value>, ApiError> {
    tokio::task::spawn_blocking(move || {
        use std::{
            io::{BufRead, BufReader, Read, Write},
            os::unix::net::UnixStream,
        };
        let unavailable = |error: Box<dyn std::error::Error>| {
            ApiError(StatusCode::SERVICE_UNAVAILABLE, error.to_string())
        };
        let query = || -> Result<Value> {
            let mut stream = UnixStream::connect(control)?;
            stream.set_read_timeout(Some(Duration::from_secs(if kind == "system.info.get" {
                50
            } else {
                8
            })))?;
            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
            let mut bytes =
                serde_json::to_vec(&json!({"jsonrpc":"2.0","id":"web","method":kind,"params":{}}))?;
            bytes.push(b'\n');
            stream.write_all(&bytes)?;
            let mut line = String::new();
            BufReader::new(stream)
                .take((venera_debug_protocol::DEFAULT_MAX_LINE_BYTES + 1) as u64)
                .read_line(&mut line)?;
            if !line.ends_with('\n') {
                return Err("Live request ended before a complete response".into());
            }
            let response: Value = serde_json::from_str(&line)?;
            if let Some(result) = response.get("result") {
                let mut result = result.clone();
                if kind == "system.info.get" {
                    result["major"] = json!(1);
                    result["type"] = json!("system.info.result");
                    result["request_id"] = response.get("id").cloned().unwrap_or(json!("web"));
                    result["ok"] = json!(true);
                }
                Ok(result)
            } else if kind == "system.info.get" && response.get("error").is_some() {
                Ok(json!({
                    "major": 1,
                    "type": "system.info.result",
                    "request_id": response.get("id").cloned().unwrap_or(json!("web")),
                    "ok": false,
                    "error": response["error"],
                }))
            } else {
                Ok(response)
            }
        };
        query().map(Json).map_err(unavailable)
    })
    .await
    .map_err(internal)?
}

pub async fn serve(
    database: PathBuf,
    assets: PathBuf,
    listener: tokio::net::TcpListener,
    control: PathBuf,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    let static_files = ServeDir::new(&assets).fallback(ServeFile::new(assets.join("index.html")));
    let system_control = control.clone();
    let connection_control = control;
    let app = Router::new()
        .route(
            "/api/system-info",
            get(move || control_query(system_control.clone(), "system.info.get")),
        )
        .route(
            "/api/connection",
            get(move || control_query(connection_control.clone(), "connection.get")),
        )
        .route(
            "/api/health",
            get(|| async { Json(json!({"status":"ok"})) }),
        )
        .route("/api/events", get(events))
        .route("/api/events/{id}", get(detail))
        .route("/api/export", get(export))
        .route("/api", get(api_missing))
        .route("/api/{*path}", get(api_missing))
        .method_not_allowed_fallback(|| async {
            ApiError(
                StatusCode::METHOD_NOT_ALLOWED,
                "Method not supported".into(),
            )
        })
        .fallback_service(static_files)
        .with_state(database);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown.await;
        })
        .await?;
    Ok(())
}
