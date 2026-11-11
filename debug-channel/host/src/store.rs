use rusqlite::{params, Connection, Error, ErrorCode, OptionalExtension};
use serde_json::{json, Value};
use std::{io::Write, path::Path};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

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
        if max_bytes < 65536 {
            return Err("storage budget must be at least 65536 bytes".into());
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch("PRAGMA page_size=4096; PRAGMA auto_vacuum=FULL; PRAGMA journal_mode=TRUNCATE; CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, received_at TEXT NOT NULL, message TEXT NOT NULL);")?;
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
                match store.db.execute_batch("BEGIN IMMEDIATE; ALTER TABLE events RENAME TO events_previous; CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, received_at TEXT NOT NULL, message TEXT NOT NULL); INSERT INTO events SELECT * FROM events_previous ORDER BY id; DROP TABLE events_previous; COMMIT;") {
                    Ok(()) => break,
                    Err(Error::SqliteFailure(error, _)) if error.code == ErrorCode::DiskFull => {
                        let _ = store.db.execute_batch("ROLLBACK");
                        if !store.evict()? { return Err("storage budget cannot hold the database metadata".into()); }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(store)
    }
    fn evict(&self) -> Result<bool> {
        Ok(self.db.execute("DELETE FROM events WHERE id IN (SELECT id FROM events ORDER BY id LIMIT MAX(1,(SELECT COUNT(*)/10 FROM events)))", [])? > 0)
    }
    pub fn append(&self, message: &Value) -> Result<()> {
        let text = serde_json::to_string(message)?;
        let received = now();
        loop {
            match self.db.execute(
                "INSERT INTO events(received_at,message) VALUES (?1,?2)",
                params![received, text],
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
}

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
    fn contains(&self, received: &str) -> Result<bool> {
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
    let upper: i64 = db.query_row("SELECT COALESCE(MAX(id),0) FROM events", [], |r| r.get(0))?;
    let mut last = 0i64;
    loop {
        let row: Option<(i64, String, String)> = db.query_row(
            "SELECT id,received_at,message FROM events WHERE id>?1 AND id<=?2 ORDER BY id LIMIT 1",
            params![last, upper], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?;
        let Some((id, received, message)) = row else {
            break;
        };
        last = id;
        if !range.contains(&received)? {
            continue;
        }
        let value: Value = serde_json::from_str(&message)?;
        serde_json::to_writer(&mut *out, &json!({"received_at":received,"message":value}))?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
