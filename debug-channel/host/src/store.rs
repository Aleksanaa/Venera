use rusqlite::{params, Connection, Error, ErrorCode, OptionalExtension};
use serde_json::Value;
use std::{collections::BTreeSet, io::Write, path::Path};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use venera_debug_protocol::v1::{DecimalU64, HistoryCursor, HistoryEvent, HistoryPage};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const MIN_STORAGE_BYTES: u64 = 16384 + 7 * (2 * 4096 + 8);

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
}
impl Store {
    pub fn open(path: &Path, max_bytes: u64) -> Result<Self> {
        if max_bytes < MIN_STORAGE_BYTES {
            return Err(
                format!("storage budget must be at least {MIN_STORAGE_BYTES} bytes").into(),
            );
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch("PRAGMA page_size=4096; PRAGMA auto_vacuum=FULL; PRAGMA journal_mode=TRUNCATE; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, received_at TEXT NOT NULL, message TEXT NOT NULL, raw_message TEXT);")?;
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
        let store = Self { db };
        let pages = (max_bytes - 16384) / (2 * 4096 + 8);
        while store
            .db
            .query_row("PRAGMA page_count", [], |row| row.get::<_, u64>(0))?
            > pages
        {
            if !store.evict()? {
                return Err("storage budget cannot hold the database metadata".into());
            }
        }
        store.db.pragma_update(None, "max_page_count", pages)?;
        let sql: String = store.db.query_row(
            "SELECT sql FROM sqlite_master WHERE name='events'",
            [],
            |r| r.get(0),
        )?;
        if !sql.to_uppercase().contains("AUTOINCREMENT") {
            loop {
                match store.db.execute_batch("BEGIN IMMEDIATE; ALTER TABLE events RENAME TO events_previous; CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, received_at TEXT NOT NULL, message TEXT NOT NULL, raw_message TEXT); INSERT INTO events(id,received_at,message,raw_message) SELECT id,received_at,message,raw_message FROM events_previous ORDER BY id; DROP TABLE events_previous; COMMIT;") {
                    Ok(()) => break,
                    Err(Error::SqliteFailure(error, _)) if error.code == ErrorCode::DiskFull => {
                        let _ = store.db.execute_batch("ROLLBACK");
                        if !store.evict()? { return Err("storage budget cannot hold the database metadata".into()); }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        loop {
            match store.migrate_history() {
                Ok(()) => break,
                Err(error) if error.downcast_ref::<Error>().is_some_and(|error| {
                    matches!(error, Error::SqliteFailure(e, _) if e.code == ErrorCode::DiskFull)
                }) => {
                    if !store.evict()? {
                        return Err("storage budget cannot hold the database metadata".into());
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(store)
    }

    fn migrate_history(&self) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        for column in ["journal_id", "position"] {
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
        tx.commit()?;
        Ok(())
    }
    fn evict(&self) -> Result<bool> {
        Ok(self.db.execute("DELETE FROM events WHERE id IN (SELECT id FROM events ORDER BY id LIMIT MAX(1,(SELECT COUNT(*)/10 FROM events)))", [])? > 0)
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
        let raw_text = raw.map(str::to_owned);
        let received = now();
        loop {
            match self.db.execute(
                "INSERT INTO events(received_at,message,raw_message) VALUES (?1,?2,?3)",
                params![received, text, raw_text],
            ) {
                Ok(_) => return Ok(()),
                Err(Error::SqliteFailure(e, _)) if e.code == ErrorCode::DiskFull => {
                    if !self.evict()? {
                        return Err("storage budget cannot hold this record".into());
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
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

    pub fn append_history_page(&self, page: &HistoryPage) -> Result<HistoryCursor> {
        validate_history_page(page)?;
        let mut evict_count = 0i64;
        loop {
            let result = self.history_transaction(page, evict_count);
            match result {
                Ok(cursor) => return Ok(cursor),
                Err(error) if error.downcast_ref::<Error>().is_some_and(|error| {
                    matches!(error, Error::SqliteFailure(e, _) if e.code == ErrorCode::DiskFull)
                }) => {
                    let count: i64 = self.db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
                    if evict_count >= count {
                        return Err(HistoryCapacity.into());
                    }
                    evict_count = (evict_count + (count / 10).max(1)).min(count);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn history_transaction(&self, page: &HistoryPage, evict_count: i64) -> Result<HistoryCursor> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
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
        let stale: BTreeSet<_> = page.records.iter().map(|r| r.cursor.position.0)
            .filter(|&p| p <= current).collect();
        let mut recovered = BTreeSet::new();
        let mut replaced = Vec::new();
        let mut remaining = Vec::new();
        if !stale.is_empty() {
            let mut stmt = tx.prepare("SELECT id,received_at,message FROM events WHERE journal_id=?1 AND position IS NULL AND raw_message IS NULL")?;
            let mut gaps = stmt.query([journal])?;
            while let Some(row) = gaps.next()? {
                let message: Value = serde_json::from_str(&row.get::<_, String>(2)?)?;
                if message["type"] != "gap" || message["reason"] != "retention_expired" {
                    continue;
                }
                let bound = |key: &str| -> Result<u64> {
                    Ok(message[key].as_str().ok_or("missing stored gap bound")?.parse()?)
                };
                let (first, last) = (bound("first_position")?, bound("last_position")?);
                if first == 0 || first > last {
                    return Err("invalid stored history gap".into());
                }
                let mut matches = stale.range(first..=last).peekable();
                if matches.peek().is_none() { continue; }
                replaced.push(row.get::<_, i64>(0)?);
                let received: String = row.get(1)?;
                let mut start = first;
                for &p in matches {
                    recovered.insert(p);
                    if start < p { remaining.push((received.clone(), start, p - 1)); }
                    start = p.saturating_add(1);
                }
                if !stale.contains(&last) { remaining.push((received, start, last)); }
            }
        }
        tx.execute(
            "DELETE FROM events WHERE id IN (SELECT id FROM events ORDER BY id LIMIT ?1)",
            [evict_count],
        )?;
        for id in replaced { tx.execute("DELETE FROM events WHERE id=?1", [id])?; }
        for (received, first, last) in remaining { insert_missing(&tx, journal, &received, first, last)?; }
        let received = now();
        for record in &page.records {
            let position = record.cursor.position.to_string();
            // Already consumed entries stay consumed after their display rows expire.
            if record.cursor.position.0 <= current && !recovered.contains(&record.cursor.position.0) {
                continue;
            }
            let (mut projection, kind) = match &record.event {
                HistoryEvent::Log { record } => (serde_json::to_value(record)?, "log"),
                HistoryEvent::Gap { gap } => (serde_json::to_value(gap)?, "gap"),
            };
            projection["major"] = serde_json::json!(1);
            projection["type"] = serde_json::json!(kind);
            tx.execute("INSERT OR IGNORE INTO events(received_at,message,raw_message,journal_id,position) VALUES(?1,?2,?3,?4,?5)",
                params![received, serde_json::to_string(&projection)?, rows[&position], journal, position])?;
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
        tx.commit()?;
        Ok(HistoryCursor {
            journal_id: journal.clone(),
            position: DecimalU64(next),
        })
    }
}

fn insert_missing(db: &Connection, journal: &str, received: &str, first: u64, last: u64) -> Result<()> {
    let projection = serde_json::json!({
        "major":1,"type":"gap","stream_id":journal,"journal_id":journal,
        "reason":"retention_expired","count":(last-first+1).to_string(),
        "first_seq":null,"last_seq":null,
        "first_position":first.to_string(),"last_position":last.to_string()
    });
    db.execute("INSERT INTO events(received_at,message,journal_id) VALUES(?1,?2,?3)",
        params![received, projection.to_string(), journal])?;
    Ok(())
}

fn validate_history_page(page: &HistoryPage) -> Result<()> {
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
            Some("CREATE TABLE events(id INTEGER PRIMARY KEY,received_at TEXT NOT NULL,message TEXT NOT NULL)"),
            Some("CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,received_at TEXT NOT NULL,message TEXT NOT NULL,raw_message TEXT)"),
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
            let store = Store::open(&path, 73784).unwrap();
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
                7
            );
            drop(store);
            let reopened = Store::open(&path, 73784).unwrap();
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
                "storage budget must be at least 73784 bytes"
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
