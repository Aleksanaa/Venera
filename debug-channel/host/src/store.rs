use rusqlite::{Connection, Error, ErrorCode, OptionalExtension, params};
use serde_json::Value;
use std::{collections::BTreeSet, io::Write, path::Path};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use venera_debug_protocol::v1::{
    AttributionHistoryEvent as Event, AttributionHistoryPage, AttributionHistoryRecord, DecimalU64,
    HistoryAttribution, HistoryCursor, HistoryEvent, HistoryPage,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const MIN_STORAGE_BYTES: u64 = 16384 + 8 * (2 * 4096 + 8);

const EVENTS: &str = "CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, received_at TEXT NOT NULL, message TEXT NOT NULL, raw_message TEXT)";
const DIAGNOSTIC: &str = "INSERT INTO events(received_at,message,raw_message) VALUES (?1,?2,?3)";

#[derive(Debug)]
pub(super) struct DiagnosticCapacity;
impl std::fmt::Display for DiagnosticCapacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("storage budget cannot hold this diagnostic")
    }
}
impl std::error::Error for DiagnosticCapacity {}

#[derive(Debug)]
pub(super) struct HistoryCapacity;
impl std::fmt::Display for HistoryCapacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("storage budget cannot hold this history page")
    }
}
impl std::error::Error for HistoryCapacity {}

pub(super) fn is_busy(error: &(dyn std::error::Error + 'static)) -> bool {
    error.downcast_ref::<Error>().is_some_and(|error| {
        matches!(error, Error::SqliteFailure(e, _) if matches!(e.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked))
    })
}

pub fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("UTC timestamp")
}

pub struct Store {
    db: Connection,
    pages: Option<u64>,
}
impl Store {
    pub fn open(path: &Path, max_bytes: impl Into<Option<u64>>) -> Result<Self> {
        let max_bytes = max_bytes.into();
        if max_bytes.is_some_and(|n| n < MIN_STORAGE_BYTES) {
            return Err(
                format!("storage budget must be at least {MIN_STORAGE_BYTES} bytes").into(),
            );
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch("PRAGMA page_size=4096; PRAGMA auto_vacuum=FULL; PRAGMA journal_mode=TRUNCATE; PRAGMA synchronous=FULL;")?;
        db.execute_batch(EVENTS)?;
        let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let synchronous: u32 = db.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
        if mode != "truncate" || synchronous != 2 {
            return Err("database requires TRUNCATE journaling and FULL synchronization".into());
        }
        let has_raw: bool = db
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('events') WHERE name='raw_message'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v != 0)?;
        if !has_raw {
            db.execute("ALTER TABLE events ADD COLUMN raw_message TEXT", [])?;
        }
        let auto: u32 = db.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
        if auto != 1 {
            return Err("database requires FULL auto_vacuum".into());
        }
        let pages = max_bytes.map(|n| ((n - 16384) / (2 * 4096 + 8)).min(0xfffffffe));
        let store = Self { db, pages };
        let tx = rusqlite::Transaction::new_unchecked(
            &store.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let sql: String = tx.query_row(
            "SELECT sql FROM sqlite_master WHERE name='events'",
            [],
            |r| r.get(0),
        )?;
        if !sql.to_uppercase().contains("AUTOINCREMENT") {
            tx.execute_batch("ALTER TABLE events RENAME TO events_previous; CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, received_at TEXT NOT NULL, message TEXT NOT NULL, raw_message TEXT); INSERT INTO events(id,received_at,message,raw_message) SELECT id,received_at,message,raw_message FROM events_previous ORDER BY id; DROP TABLE events_previous;")?;
        }
        Self::migrate_history(&tx)?;
        if let Some(pages) = pages {
            while tx.query_row(
                "SELECT page_count-freelist_count FROM pragma_page_count,pragma_freelist_count",
                [],
                |r| r.get::<_, u64>(0),
            )? > pages
            {
                if trim_detached_gaps(&tx, 0)? == 0 && tx.execute("DELETE FROM events WHERE id IN (SELECT id FROM events ORDER BY id LIMIT MAX(1,(SELECT COUNT(*)/10 FROM events)))", [])? == 0 {
                    return Err("storage budget cannot hold the database metadata".into());
                }
            }
        }
        tx.commit()?;
        if let Some(pages) = pages {
            store.db.pragma_update(None, "max_page_count", pages)?;
        }
        Ok(store)
    }

    fn migrate_history(tx: &Connection) -> Result<()> {
        // The revision target marks archival metadata; the applied position on
        // a log keeps its projection monotonic through delayed gap recovery.
        for column in [
            "journal_id",
            "position",
            "revision_target",
            "attribution_position",
        ] {
            let exists: bool = tx.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('events') WHERE name=?1",
                [column],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )?;
            if !exists {
                tx.execute_batch(&format!("ALTER TABLE events ADD COLUMN {column} TEXT"))?;
            }
        }
        tx.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS events_history_identity ON events(journal_id,position); CREATE TABLE IF NOT EXISTS history_checkpoints(journal_id TEXT PRIMARY KEY, position TEXT NOT NULL);")?;
        if !tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='history_missing')", [], |r| r.get::<_,bool>(0))? {
            tx.execute_batch("CREATE TABLE history_missing(id INTEGER PRIMARY KEY,journal_id TEXT NOT NULL,received_at TEXT NOT NULL,first TEXT NOT NULL,last TEXT NOT NULL);
                INSERT INTO history_missing SELECT id,journal_id,received_at,json_extract(message,'$.first_position'),json_extract(message,'$.last_position') FROM events WHERE journal_id IS NOT NULL AND position IS NULL AND raw_message IS NULL AND json_extract(message,'$.reason')='retention_expired';")?;
        }
        tx.execute_batch("CREATE TRIGGER IF NOT EXISTS record_history_missing AFTER INSERT ON events
            WHEN NEW.journal_id IS NOT NULL AND NEW.position IS NULL AND NEW.raw_message IS NULL AND json_extract(NEW.message,'$.reason')='retention_expired'
            BEGIN INSERT INTO history_missing VALUES(NEW.id,NEW.journal_id,NEW.received_at,json_extract(NEW.message,'$.first_position'),json_extract(NEW.message,'$.last_position')); END;")?;
        Ok(())
    }
    pub(super) fn nonblocking_writes(&self) -> Result<()> {
        self.db.busy_timeout(std::time::Duration::ZERO)?;
        Ok(())
    }
    pub fn append(&self, message: &Value) -> Result<()> {
        self.append_with_raw(message, None)
    }
    pub fn append_with_raw(&self, message: &Value, raw: Option<&str>) -> Result<()> {
        let text = serde_json::to_string(message)?;
        let received = now();
        let result = self.with_eviction(|tx, count| {
            self.evict(tx, count)?;
            tx.execute(DIAGNOSTIC, params![received, text, raw])?;
            Ok(())
        });
        // FULL can also come from the filesystem. Reject only records that cannot
        // fit in an empty archive with the same page limit.
        if let Err(error) = &result {
            if error.is::<HistoryCapacity>() {
                let pages = self.pages.expect("explicit budget");
                if (text.len() + raw.map_or(0, str::len)) as u64 > pages * 4096 {
                    return Err(DiagnosticCapacity.into());
                }
                let probe = Connection::open_in_memory()?;
                probe.execute_batch("PRAGMA page_size=4096; PRAGMA auto_vacuum=FULL;")?;
                probe.execute_batch(EVENTS)?;
                Self::migrate_history(&probe)?;
                probe.pragma_update(None, "max_page_count", pages)?;
                if matches!(probe.execute(DIAGNOSTIC,params![received,text,raw]),Err(Error::SqliteFailure(e,_)) if e.code==ErrorCode::DiskFull)
                {
                    return Err(DiagnosticCapacity.into());
                }
                return Err("archive space unavailable for diagnostic".into());
            }
        }
        result
    }

    pub fn history_cursor(&self, journal_id: &str) -> Result<Option<HistoryCursor>> {
        let position: Option<String> = self
            .db
            .query_row(
                "SELECT position FROM history_checkpoints WHERE journal_id=?1",
                [journal_id],
                |r| r.get(0),
            )
            .optional()?;
        position
            .map(|position| {
                Ok(HistoryCursor {
                    journal_id: journal_id.to_owned(),
                    position: position.parse().map_err(std::io::Error::other)?,
                })
            })
            .transpose()
    }

    #[allow(dead_code)] // Used by the standalone archive probe.
    pub fn append_history_page(&self, page: &HistoryPage) -> Result<HistoryCursor> {
        self.append_attribution_page(&compatibility_page(page.clone()))
    }

    pub fn append_attribution_page(&self, page: &AttributionHistoryPage) -> Result<HistoryCursor> {
        page.validate_revisions().map_err(|error| error.message)?;
        validate_history_page(page)?;
        self.with_eviction(|tx, count| self.history_transaction(tx, page, count))
    }

    fn with_eviction<T>(&self, mut write: impl FnMut(&Connection, i64) -> Result<T>) -> Result<T> {
        let mut count = 0;
        loop {
            let tx = rusqlite::Transaction::new_unchecked(
                &self.db,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let result = write(&tx, count).and_then(|value| {
                tx.commit()?;
                Ok(value)
            });
            match result {
                Err(error)
                    if self.pages.is_some()
                        && matches!(error.downcast_ref::<Error>(),Some(Error::SqliteFailure(e,_)) if e.code==ErrorCode::DiskFull) =>
                {
                    let rows: i64 = self
                        .db
                        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
                    if count == i64::MAX {
                        return Err(HistoryCapacity.into());
                    }
                    count = if count >= rows {
                        i64::MAX
                    } else {
                        (count + (rows / 10).max(1)).min(rows)
                    };
                }
                other => return other,
            }
        }
    }

    fn history_transaction(
        &self,
        tx: &Connection,
        page: &AttributionHistoryPage,
        evict_count: i64,
    ) -> Result<HistoryCursor> {
        let journal = &page.range.journal_id;
        let current = self.history_cursor(journal)?.map_or(0, |c| c.position.0);
        let next = page.next.position.0.max(current);
        let mut coverage: Vec<(u64, u64)> = page
            .records
            .iter()
            .map(|record| (record.cursor.position.0, record.cursor.position.0))
            .chain(
                page.missing
                    .iter()
                    .map(|missing| (missing.first.0, missing.last.0)),
            )
            .collect();
        coverage.sort_unstable();
        let mut covered = current;
        for (first, last) in coverage {
            if first > covered.saturating_add(1) {
                break;
            }
            covered = covered.max(last);
        }
        if covered < next {
            return Err("history page leaves an uncovered position".into());
        }
        // Validate retained identities before capacity eviction, including duplicates in this page.
        let mut rows = std::collections::BTreeMap::new();
        for record in &page.records {
            let position = record.cursor.position.to_string();
            let raw = serde_json::to_string(record)?;
            let saved: Option<String> = tx
                .query_row(
                    "SELECT raw_message FROM events WHERE journal_id=?1 AND position=?2",
                    params![journal, position],
                    |r| r.get(0),
                )
                .optional()?;
            if saved.as_ref().is_some_and(|saved| saved != &raw)
                || rows
                    .insert(position, raw.clone())
                    .is_some_and(|previous| previous != raw)
            {
                return Err("history record conflicts with its stored identity".into());
            }
        }
        // Read the original gaps before capacity eviction can remove their evidence.
        let stale: BTreeSet<_> = page
            .records
            .iter()
            .map(|r| r.cursor.position.0)
            .filter(|&p| p <= current)
            .collect();
        let mut recovered = BTreeSet::new();
        let mut replaced = Vec::new();
        let mut remaining = Vec::new();
        if !stale.is_empty() {
            let mut stmt = tx.prepare(
                "SELECT id,received_at,first,last FROM history_missing WHERE journal_id=?1",
            )?;
            let mut gaps = stmt.query([journal])?;
            while let Some(row) = gaps.next()? {
                let first: u64 = row.get::<_, String>(2)?.parse()?;
                let last: u64 = row.get::<_, String>(3)?.parse()?;
                if first == 0 || first > last {
                    return Err("invalid stored history gap".into());
                }
                let mut matches = stale.range(first..=last).peekable();
                if matches.peek().is_none() {
                    continue;
                }
                replaced.push(row.get::<_, i64>(0)?);
                let received: String = row.get(1)?;
                let mut start = first;
                for &p in matches {
                    recovered.insert(p);
                    if start < p {
                        remaining.push((received.clone(), start, p - 1));
                    }
                    start = p.saturating_add(1);
                }
                if !stale.contains(&last) {
                    remaining.push((received, start, last));
                }
            }
        }
        // A delayed reader may fill a gap after its revision arrived. Take the
        // latest retained attribution from the same view used to recover the gap.
        let mut snapshots = std::collections::BTreeMap::new();
        for record in &page.records {
            if recovered.contains(&record.cursor.position.0)
                && matches!(record.event, Event::Log { .. })
            {
                let raw: Option<String> = tx
                    .query_row(
                        "SELECT raw_message FROM events WHERE journal_id=?1 AND revision_target=?2
                     ORDER BY length(position) DESC, position DESC LIMIT 1",
                        params![journal, record.cursor.position.to_string()],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(raw) = raw {
                    let entry: AttributionHistoryRecord = serde_json::from_str(&raw)?;
                    if let Event::AttributionRevision { attribution, .. } = entry.event {
                        snapshots.insert(
                            record.cursor.position.0,
                            (entry.cursor.position, attribution),
                        );
                    }
                }
            }
        }
        self.evict(tx, evict_count)?;
        for id in replaced {
            tx.execute("DELETE FROM events WHERE id=?1", [id])?;
            tx.execute("DELETE FROM history_missing WHERE id=?1", [id])?;
        }
        for (received, first, last) in remaining {
            insert_missing(&tx, journal, &received, first, last)?;
        }
        let received = now();
        let mut entries: Vec<_> = page.records.iter().collect();
        entries.sort_unstable_by_key(|record| record.cursor.position);
        for record in entries {
            let position = record.cursor.position.to_string();
            // Already consumed entries stay consumed after their display rows expire.
            if record.cursor.position.0 <= current && !recovered.contains(&record.cursor.position.0)
            {
                continue;
            }
            let (mut projection, kind, revision_target) = match &record.event {
                Event::Log { record } => (serde_json::to_value(record)?, "log", None),
                Event::Gap { gap } => (
                    serde_json::to_value(gap)?,
                    "gap",
                    gap.is_history_attribution_marker().then(String::new),
                ),
                Event::AttributionRevision {
                    target,
                    attribution,
                } => {
                    apply_attribution(tx, target, record.cursor.position, attribution)?;
                    (
                        serde_json::json!({}),
                        "attribution_revision",
                        Some(target.position.to_string()),
                    )
                }
            };
            projection["major"] = serde_json::json!(1);
            projection["type"] = serde_json::json!(kind);
            tx.execute("INSERT OR IGNORE INTO events(received_at,message,raw_message,journal_id,position,revision_target) VALUES(?1,?2,?3,?4,?5,?6)",
                params![received, serde_json::to_string(&projection)?, rows[&position], journal, position, revision_target])?;
            if let Some((revision, attribution)) = snapshots.remove(&record.cursor.position.0) {
                apply_attribution(tx, &record.cursor, revision, &attribution)?;
            }
        }
        for missing in &page.missing {
            if missing.last.0 <= current {
                continue;
            }
            let first = missing.first.0.max(current.saturating_add(1));
            insert_missing(&tx, journal, &received, first, missing.last.0)?;
        }
        tx.execute("INSERT INTO history_checkpoints(journal_id,position) VALUES(?1,?2) ON CONFLICT(journal_id) DO UPDATE SET position=excluded.position",
            params![journal, next.to_string()])?;
        Ok(HistoryCursor {
            journal_id: journal.clone(),
            position: DecimalU64(next),
        })
    }

    fn evict(&self, db: &Connection, count: i64) -> Result<()> {
        if count > 0 {
            db.execute(
                "DELETE FROM events WHERE id IN (SELECT id FROM events ORDER BY id LIMIT ?1)",
                [count],
            )?;
            // Keep recent backfill evidence after its display row expires. At the
            // final retry, make the entire data budget available to the new page.
            let keep = if count == i64::MAX {
                0
            } else {
                self.pages.unwrap_or(0) / 8
            };
            trim_detached_gaps(db, keep)?;
        }
        Ok(())
    }
}

fn apply_attribution(
    db: &Connection,
    target: &HistoryCursor,
    revision: DecimalU64,
    attribution: &HistoryAttribution,
) -> Result<()> {
    // Compare canonical decimal strings as unsigned positions, including values
    // outside SQLite's signed integer range.
    db.execute(
        "UPDATE events SET message=json_set(message,'$.process',json(?1),'$.ancestors',json(?2)),
            attribution_position=?3 WHERE journal_id=?4 AND position=?5 AND json_extract(message,'$.type')='log'
         AND (attribution_position IS NULL OR length(attribution_position)<length(?3)
              OR (length(attribution_position)=length(?3) AND attribution_position<?3))",
        params![serde_json::to_string(&attribution.process)?, serde_json::to_string(&attribution.ancestors)?,
            revision.to_string(), target.journal_id, target.position.to_string()],
    )?;
    Ok(())
}

pub(super) fn compatibility_page(page: HistoryPage) -> AttributionHistoryPage {
    AttributionHistoryPage {
        range: page.range,
        records: page
            .records
            .into_iter()
            .map(|entry| AttributionHistoryRecord {
                cursor: entry.cursor,
                boot_id: entry.boot_id,
                recorded_at_ms: entry.recorded_at_ms,
                startup: entry.startup,
                event: match entry.event {
                    HistoryEvent::Log { record } => Event::Log { record },
                    HistoryEvent::Gap { gap } => Event::Gap { gap },
                },
            })
            .collect(),
        missing: page.missing,
        next: page.next,
    }
}

fn trim_detached_gaps(db: &Connection, keep: u64) -> Result<usize> {
    Ok(db.execute(
        "DELETE FROM history_missing WHERE id IN (
        SELECT g.id FROM history_missing g WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.id=g.id)
        ORDER BY g.id DESC LIMIT -1 OFFSET ?1)",
        [keep],
    )?)
}

fn insert_missing(
    db: &Connection,
    journal: &str,
    received: &str,
    first: u64,
    last: u64,
) -> Result<()> {
    let projection = serde_json::json!({
        "major":1,"type":"gap","stream_id":journal,"journal_id":journal,
        "reason":"retention_expired","count":(last-first+1).to_string(),
        "first_seq":null,"last_seq":null,
        "first_position":first.to_string(),"last_position":last.to_string()
    });
    db.execute(
        "INSERT INTO events(received_at,message,journal_id) VALUES(?1,?2,?3)",
        params![received, projection.to_string(), journal],
    )?;
    Ok(())
}

fn validate_history_page(page: &AttributionHistoryPage) -> Result<()> {
    let journal = &page.range.journal_id;
    let high = page.range.high_water.0;
    if journal.is_empty()
        || page.next.journal_id != *journal
        || page.next.position.0 > high
        || page.range.oldest.0 == 0
        || page.range.oldest.0 > high.saturating_add(1)
    {
        return Err("invalid history page range".into());
    }
    let mut intervals = Vec::new();
    for record in &page.records {
        let position = record.cursor.position.0;
        if record.cursor.journal_id != *journal
            || position == 0
            || position < page.range.oldest.0
            || position > page.next.position.0
        {
            return Err("invalid history record cursor".into());
        }
    }
    for missing in &page.missing {
        if missing.first.0 == 0
            || missing.first > missing.last
            || missing.last > page.next.position
            || missing.reason != "retention_expired"
        {
            return Err("invalid history missing interval".into());
        }
        intervals.push((missing.first.0, missing.last.0));
    }
    intervals.sort_unstable();
    if intervals.windows(2).any(|pair| pair[0].1 >= pair[1].0)
        || page.records.iter().any(|record| {
            intervals
                .iter()
                .any(|&(first, last)| (first..=last).contains(&record.cursor.position.0))
        })
    {
        return Err("overlapping history coverage".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "store_history_tests.rs"]
mod history_tests;

#[derive(Default)]
pub struct ExportRange {
    from: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
}
impl ExportRange {
    pub fn parse(from: Option<&str>, until: Option<&str>) -> Result<Self> {
        let parse = |value: &str| -> Result<OffsetDateTime> {
            Ok(OffsetDateTime::parse(value, &Rfc3339)?.to_offset(time::UtcOffset::UTC))
        };
        let range = Self {
            from: from.map(parse).transpose()?,
            until: until.map(parse).transpose()?,
        };
        if matches!((range.from, range.until), (Some(from), Some(until)) if from >= until) {
            return Err("--from must be earlier than --until".into());
        }
        Ok(range)
    }
    pub(super) fn contains(&self, received: &str) -> Result<bool> {
        if self.from.is_none() && self.until.is_none() {
            return Ok(true);
        }
        let instant = OffsetDateTime::parse(received, &Rfc3339)?;
        Ok(self.from.is_none_or(|from| instant >= from)
            && self.until.is_none_or(|until| instant < until))
    }
}

pub fn export(path: &Path, out: &mut impl Write, range: &ExportRange) -> Result<()> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let has_raw = has_raw(&db)?;
    let upper: i64 = db.query_row("SELECT COALESCE(MAX(id),0) FROM events", [], |r| r.get(0))?;
    let mut last = 0i64;
    loop {
        let row: Option<(i64, String, String)> = db.query_row(
            if has_raw {
                "SELECT id,received_at,COALESCE(raw_message,message) FROM events WHERE id>?1 AND id<=?2 ORDER BY id LIMIT 1"
            } else {
                "SELECT id,received_at,message FROM events WHERE id>?1 AND id<=?2 ORDER BY id LIMIT 1"
            },
            params![last, upper], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?;
        let Some((id, received, message)) = row else {
            break;
        };
        last = id;
        if !range.contains(&received)? {
            continue;
        }
        let message = export_message(&message)?;
        writeln!(
            out,
            "{{\"message\":{message},\"received_at\":{}}}",
            serde_json::to_string(&received)?
        )?;
    }
    Ok(())
}

pub(super) fn has_raw(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('events') WHERE name='raw_message'",
        [],
        |r| r.get::<_, i64>(0),
    )? != 0)
}

pub(super) fn export_message(raw: &str) -> Result<String> {
    let _: Value = serde_json::from_str(raw)?;
    // JSON line breaks occur between tokens; retain the original number tokens.
    Ok(raw.lines().map(str::trim).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn minimum_budget_creates_and_migrates_writable_archives() {
        let schemas = [
            None,
            Some(
                "CREATE TABLE events(id INTEGER PRIMARY KEY,received_at TEXT NOT NULL,message TEXT NOT NULL)",
            ),
            Some(
                "CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,received_at TEXT NOT NULL,message TEXT NOT NULL,raw_message TEXT)",
            ),
        ];
        for legacy in schemas {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("minimum.db");
            if let Some(sql) = legacy {
                let db = Connection::open(&path).unwrap();
                db.execute_batch("PRAGMA page_size=4096; PRAGMA auto_vacuum=FULL;")
                    .unwrap();
                db.execute_batch(sql).unwrap();
                db.execute("INSERT INTO events(id,received_at,message) VALUES(1,'2026-11-10T00:00:00Z','{\"text\":\"legacy\"}')", []).unwrap();
            }
            let store = Store::open(&path, 81984).unwrap();
            store.append(&json!({"text":"current"})).unwrap();
            let mut bytes = Vec::new();
            export(&path, &mut bytes, &ExportRange::default()).unwrap();
            let rows: Vec<Value> = String::from_utf8(bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows.last().unwrap()["message"], json!({"text":"current"}));
            if legacy.is_some() {
                assert_eq!(rows[0]["message"], json!({"text":"legacy"}));
            }
            let page: HistoryPage = serde_json::from_value(json!({
                "range":{"journal_id":"minimum","oldest":"2","high_water":"1"},
                "records":[],"missing":[{"first":"1","last":"1","reason":"retention_expired"}],
                "next":{"journal_id":"minimum","position":"1"}
            }))
            .unwrap();
            assert_eq!(store.append_history_page(&page).unwrap().position.0, 1);
            assert_eq!(
                store
                    .db
                    .query_row("PRAGMA page_count", [], |r| r.get::<_, u64>(0))
                    .unwrap(),
                8
            );
            drop(store);
            let reopened = Store::open(&path, 81984).unwrap();
            assert_eq!(
                reopened
                    .history_cursor("minimum")
                    .unwrap()
                    .unwrap()
                    .position
                    .0,
                1
            );
            reopened.append(&json!({"text":"reopened"})).unwrap();
        }
    }

    #[test]
    fn smaller_budgets_report_the_supported_minimum() {
        let dir = tempfile::tempdir().unwrap();
        for budget in [65536, 73783] {
            let error = Store::open(&dir.path().join("small.db"), budget)
                .err()
                .unwrap();
            assert_eq!(
                error.to_string(),
                "storage budget must be at least 81984 bytes"
            );
        }
    }

    #[test]
    fn legacy_history_conversion_preserves_raw_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA auto_vacuum=FULL; CREATE TABLE events(id INTEGER PRIMARY KEY,received_at TEXT NOT NULL,message TEXT NOT NULL,raw_message TEXT)").unwrap();
        let raw = "{ \"jsonrpc\":\"2.0\", \"method\":\"gap\", \"params\":{\"stream_id\":\"流 🙂\",\"reason\":\"overflow\",\"count\":\"18446744073709551615\",\"first_seq\":null,\"last_seq\":null} }";
        db.execute(
            "INSERT INTO events VALUES(7,'2026-11-10T00:00:00Z','{\"type\":\"gap\"}',?1)",
            [raw],
        )
        .unwrap();
        drop(db);
        let store = Store::open(&path, 131072).unwrap();
        let saved: (String, String) = store
            .db
            .query_row(
                "SELECT message,raw_message FROM events WHERE id=7",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(saved, ("{\"type\":\"gap\"}".into(), raw.into()));
        let mut bytes = Vec::new();
        export(&path, &mut bytes, &ExportRange::default()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"received_at":"2026-11-10T00:00:00Z","message":{"jsonrpc":"2.0","method":"gap","params":{"stream_id":"流 🙂","reason":"overflow","count":"18446744073709551615","first_seq":null,"last_seq":null}}})
        );
    }

    #[test]
    fn legacy_history_conversion_obeys_the_page_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA auto_vacuum=FULL; CREATE TABLE events(id INTEGER PRIMARY KEY,received_at TEXT NOT NULL,message TEXT NOT NULL);").unwrap();
        for seq in 0..100 {
            db.execute(
                "INSERT INTO events(received_at,message) VALUES('2026-11-10T00:00:00Z',?1)",
                [json!({"seq":seq,"text":"x".repeat(1000)}).to_string()],
            )
            .unwrap();
        }
        drop(db);
        let store = Store::open(&path, 131072).unwrap();
        store.append(&json!({"seq":100,"text":"current"})).unwrap();
        let mut bytes = Vec::new();
        export(&path, &mut bytes, &ExportRange::default()).unwrap();
        let rows: Vec<Value> = String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            rows.last().unwrap()["message"],
            json!({"seq":100,"text":"current"})
        );
        assert!(rows.first().unwrap()["message"]["seq"].as_u64().unwrap() > 0);
        let physical: u64 = [
            "legacy.db",
            "legacy.db-journal",
            "legacy.db-wal",
            "legacy.db-shm",
        ]
        .iter()
        .map(|name| {
            std::fs::metadata(dir.path().join(name))
                .map(|m| m.len())
                .unwrap_or(0)
        })
        .sum();
        assert!(physical <= 131072, "physical bytes: {physical}");
    }

    #[test]
    fn old_database_export_keeps_its_cutoff_during_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA auto_vacuum=FULL; CREATE TABLE events(id INTEGER PRIMARY KEY,received_at TEXT NOT NULL,message TEXT NOT NULL); INSERT INTO events VALUES(1,'2026-11-10T00:00:00Z','{\"text\":\"first\"}'),(2,'2026-11-10T00:00:01Z','{\"text\":\"second\"}');").unwrap();
        drop(db);
        let store = Store::open(&path, 131072).unwrap();
        struct Output<'a> {
            store: &'a Store,
            bytes: Vec<u8>,
            changed: bool,
        }
        impl Write for Output<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend(bytes);
                if bytes.contains(&b'\n') && !self.changed {
                    self.store.db.execute("DELETE FROM events", []).unwrap();
                    self.store.append(&json!({"text":"new arrival"})).unwrap();
                    self.changed = true;
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut output = Output {
            store: &store,
            bytes: Vec::new(),
            changed: false,
        };
        export(&path, &mut output, &ExportRange::default()).unwrap();
        assert_eq!(
            String::from_utf8(output.bytes).unwrap(),
            "{\"message\":{\"text\":\"first\"},\"received_at\":\"2026-11-10T00:00:00Z\"}\n"
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT id FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        drop(store);
        let reopened = Store::open(&path, 131072).unwrap();
        reopened.db.execute("DELETE FROM events", []).unwrap();
        reopened.append(&json!({"text":"after restart"})).unwrap();
        assert_eq!(
            reopened
                .db
                .query_row("SELECT id FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            4
        );
    }
}
