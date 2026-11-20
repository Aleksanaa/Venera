use super::{export, ExportRange, Store};
use rusqlite::Connection;
use serde_json::{json, Value};
use venera_debug_protocol::v1::{HistoryPage, HistoryRecord};

fn record(journal: &str, position: u64, text: &str) -> HistoryRecord {
    serde_json::from_value(json!({
        "cursor":{"journal_id":journal,"position":position.to_string()},
        "boot_id":"boot-1","recorded_at_ms":"1794355200000","startup":true,
        "event":{"type":"log","record":{
            "stream_id":"stream-1","seq":"18446744073709551615",
            "observed_at":"2026-11-11T00:00:00Z","source_time":null,
            "source":"driver","component":"KMD","level":"INFO","raw_level":"info",
            "process":null,"session_id":null,"tid":null,"text":text
        }}
    }))
    .unwrap()
}

fn page(journal: &str, positions: &[u64], missing: &[(u64, u64)], next: u64) -> HistoryPage {
    let mut page: HistoryPage = serde_json::from_value(json!({
        "range":{"journal_id":journal,"oldest":"1","high_water":next.to_string()},
        "records":[],
        "missing":missing.iter().map(|(first,last)| json!({"first":first.to_string(),"last":last.to_string(),"reason":"retention_expired"})).collect::<Vec<_>>(),
        "next":{"journal_id":journal,"position":next.to_string()}
    })).unwrap();
    page.records = positions
        .iter()
        .map(|&position| record(journal, position, "captured"))
        .collect();
    page
}

fn projections(path: &std::path::Path) -> Vec<Value> {
    let db = Connection::open(path).unwrap();
    let rows = db
        .prepare("SELECT message FROM events ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| serde_json::from_str(&row.unwrap()).unwrap())
        .collect();
    rows
}

#[test]
fn process_keys_and_exited_ancestors_survive_sync_reopen_and_export() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut page = page("process-boot", &[1], &[], 1);
    page.records[0].boot_id = "134393633875000000".into();
    let venera_debug_protocol::v1::HistoryEvent::Log { record } = &mut page.records[0].event else {
        panic!("log")
    };
    record.process = Some(serde_json::from_value(json!({"pid":42,"start_time_filetime":"134393633875000200",
        "boot_time_filetime":"134393633875000000","start_key":"18446744073709551614","exe_path":"C:\\child.exe"})).unwrap());
    record.ancestors = vec![serde_json::from_value(json!({"pid":20,"start_time_filetime":"134393633875000100",
        "boot_time_filetime":"134393633875000000","start_key":"9007199254740997","exe_path":"C:\\parent.exe"})).unwrap()];
    let store = Store::open(&path, 262144).unwrap();
    store.append_history_page(&page).unwrap();
    drop(store);
    let store = Store::open(&path, 262144).unwrap();
    assert_eq!(store.append_history_page(&page).unwrap().position.0, 1);
    let rows = projections(&path);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["process"]["start_key"], "18446744073709551614");
    assert_eq!(
        rows[0]["ancestors"][0]["boot_time_filetime"],
        "134393633875000000"
    );
    let mut output = Vec::new();
    export(&path, &mut output, &ExportRange::default()).unwrap();
    let exported: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        exported["message"]["event"]["record"]["ancestors"][0],
        json!({"pid":20,
        "start_time_filetime":"134393633875000100","boot_time_filetime":"134393633875000000",
        "start_key":"9007199254740997","exe_path":"C:\\parent.exe"})
    );
}

#[test]
fn store_history_repetition_and_reopen_preserve_one_original_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let page = page("journal-a", &[1], &[], 1);
    let store = Store::open(&path, 262144).unwrap();
    assert_eq!(store.history_cursor("journal-a").unwrap(), None);
    assert_eq!(store.append_history_page(&page).unwrap().position.0, 1);
    assert_eq!(store.append_history_page(&page).unwrap().position.0, 1);
    drop(store);
    let store = Store::open(&path, 262144).unwrap();
    assert_eq!(store.append_history_page(&page).unwrap().position.0, 1);
    assert_eq!(
        store
            .history_cursor("journal-a")
            .unwrap()
            .unwrap()
            .position
            .0,
        1
    );
    assert_eq!(
        projections(&path),
        vec![json!({
            "major":1,"type":"log","stream_id":"stream-1","seq":"18446744073709551615",
            "observed_at":"2026-11-11T00:00:00Z","source_time":null,
            "source":"driver","component":"KMD","level":"INFO","raw_level":"info",
            "process":null,"session_id":null,"tid":null,"text":"captured"
        })]
    );
    let mut output = Vec::new();
    export(&path, &mut output, &ExportRange::default()).unwrap();
    let exported: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        exported["message"]["cursor"],
        json!({"journal_id":"journal-a","position":"1"})
    );
    assert_eq!(
        exported["message"]["event"]["record"]["seq"],
        "18446744073709551615"
    );
    assert_eq!(
        store
            .db
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "truncate"
    );
    assert_eq!(
        store
            .db
            .query_row("PRAGMA synchronous", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        2
    );
}

#[test]
fn delayed_page_fills_retention_gap_across_connections_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let first = Store::open(&path, 262144).unwrap();
    let delayed = Store::open(&path, 262144).unwrap();
    for writer in [&first, &delayed] {
        assert_eq!(writer.history_cursor("j").unwrap(), None);
    }
    let expired = page("j", &[], &[(1, 1)], 1);
    let real = page("j", &[1], &[], 1);
    first.append_history_page(&expired).unwrap();
    delayed.append_history_page(&real).unwrap();
    drop((first, delayed));
    let reopened = Store::open(&path, 262144).unwrap();
    for replay in [&expired, &real] {
        assert_eq!(reopened.append_history_page(replay).unwrap().position.0, 1);
    }
    let mut bytes = Vec::new();
    export(&path, &mut bytes, &ExportRange::default()).unwrap();
    let row: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        row["message"],
        serde_json::to_value(&real.records[0]).unwrap()
    );
}

#[test]
fn store_history_conflict_rolls_back_records_and_progress() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let store = Store::open(&path, 262144).unwrap();
    store.append_history_page(&page("a", &[1], &[], 1)).unwrap();
    let mut conflicting = page("a", &[2, 1], &[], 2);
    conflicting.records[1] = record("a", 1, "changed");
    assert!(store.append_history_page(&conflicting).is_err());
    assert_eq!(store.history_cursor("a").unwrap().unwrap().position.0, 1);
    assert_eq!(
        projections(&path)
            .iter()
            .map(|v| v["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["captured"]
    );
    let mut duplicate = page("b", &[1, 1], &[], 1);
    duplicate.records[1] = record("b", 1, "changed");
    assert!(store.append_history_page(&duplicate).is_err());
    assert_eq!(store.history_cursor("b").unwrap(), None);
}

#[test]
fn store_history_explicit_holes_advance_and_old_pages_keep_progress() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let store = Store::open(&path, 262144).unwrap();
    let first = page("a", &[4, 2], &[(1, 1), (3, 3)], 4);
    assert_eq!(store.append_history_page(&first).unwrap().position.0, 4);
    assert_eq!(
        store
            .append_history_page(&page("a", &[5], &[], 5))
            .unwrap()
            .position
            .0,
        5
    );
    assert_eq!(store.append_history_page(&first).unwrap().position.0, 5);
    let rows = projections(&path);
    assert_eq!(rows.len(), 5);
    assert_eq!(
        rows[2],
        json!({"major":1,"type":"gap","stream_id":"a","journal_id":"a","reason":"retention_expired","count":"1","first_seq":null,"last_seq":null,"first_position":"1","last_position":"1"})
    );
    assert_eq!(rows[3]["first_position"], "3");
}

#[test]
fn store_history_uncovered_positions_and_overlap_leave_database_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let store = Store::open(&path, 262144).unwrap();
    assert!(store.append_history_page(&page("a", &[2], &[], 2)).is_err());
    assert_eq!(store.history_cursor("a").unwrap(), None);
    assert!(store
        .append_history_page(&page("a", &[2], &[(1, 2)], 2))
        .is_err());
    assert_eq!(
        store
            .append_history_page(&page("a", &[1, 2], &[], 2))
            .unwrap()
            .position
            .0,
        2
    );
    assert_eq!(projections(&path).len(), 2);
}

#[test]
fn store_history_journals_consume_independently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let store = Store::open(&path, 262144).unwrap();
    store
        .append_history_page(&page("a", &[1, 2], &[], 2))
        .unwrap();
    assert_eq!(store.history_cursor("b").unwrap(), None);
    assert!(store.append_history_page(&page("b", &[3], &[], 3)).is_err());
    store.append_history_page(&page("b", &[1], &[], 1)).unwrap();
    assert_eq!(store.history_cursor("a").unwrap().unwrap().position.0, 2);
    assert_eq!(store.history_cursor("b").unwrap().unwrap().position.0, 1);
    assert_eq!(projections(&path).len(), 3);
}

#[test]
fn store_history_unsigned_boundary_uses_finite_missing_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let store = Store::open(&path, 262144).unwrap();
    let last = page("a", &[u64::MAX], &[(1, u64::MAX - 1)], u64::MAX);
    assert_eq!(
        store.append_history_page(&last).unwrap().position.0,
        18446744073709551615
    );
    assert_eq!(
        store.append_history_page(&last).unwrap().position.0,
        18446744073709551615
    );
    drop(store);
    let store = Store::open(&path, 262144).unwrap();
    assert_eq!(
        store
            .history_cursor("a")
            .unwrap()
            .unwrap()
            .position
            .to_string(),
        "18446744073709551615"
    );
    assert_eq!(projections(&path)[1]["count"], "18446744073709551614");
}

#[test]
fn store_history_eviction_keeps_consumed_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let store = Store::open(&path, 131072).unwrap();
    store.append_history_page(&page("a", &[1], &[], 1)).unwrap();
    for i in 0..80 {
        store
            .append(&json!({"text":"x".repeat(1800),"seq":i}))
            .unwrap();
    }
    assert_eq!(store.history_cursor("a").unwrap().unwrap().position.0, 1);
    assert_eq!(
        store
            .append_history_page(&page("a", &[1], &[], 1))
            .unwrap()
            .position
            .0,
        1
    );
    assert!(projections(&path).iter().all(|v| v["text"] != "captured"));
    drop(store);
    let store = Store::open(&path, 131072).unwrap();
    assert_eq!(store.history_cursor("a").unwrap().unwrap().position.0, 1);
    assert_eq!(
        store
            .append_history_page(&page("a", &[2], &[], 2))
            .unwrap()
            .position
            .0,
        2
    );
}

#[test]
fn store_history_page_capacity_eviction_commits_with_progress() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let store = Store::open(&path, 131072).unwrap();
    for position in 1..=60 {
        let mut current = page("a", &[position], &[], position);
        current.records[0] = record("a", position, &"x".repeat(1000));
        assert_eq!(
            store.append_history_page(&current).unwrap().position.0,
            position
        );
    }
    drop(store);
    let store = Store::open(&path, 131072).unwrap();
    assert_eq!(store.history_cursor("a").unwrap().unwrap().position.0, 60);
    let db = Connection::open(&path).unwrap();
    let retained: Vec<String> = db
        .prepare("SELECT position FROM events ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(retained.last().unwrap(), "60");
    assert!(!retained.iter().any(|position| position == "1"));
    let physical: u64 = [
        "history.db",
        "history.db-journal",
        "history.db-wal",
        "history.db-shm",
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
fn store_history_capacity_failure_rolls_back_page_and_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let store = Store::open(&path, 131072).unwrap();
    store.append_history_page(&page("a", &[1], &[], 1)).unwrap();
    let mut large = page("a", &[2], &[], 2);
    large.records[0] = record("a", 2, &"x".repeat(100000));
    assert!(store.append_history_page(&large).is_err());
    assert_eq!(store.history_cursor("a").unwrap().unwrap().position.0, 1);
    assert_eq!(projections(&path).len(), 1);
    assert_eq!(projections(&path)[0]["text"], "captured");
}

#[test]
fn store_history_metadata_budget_error_can_be_reopened_with_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let error = Store::open(&path, 65536).err().unwrap();
    assert_eq!(
        error.to_string(),
        "storage budget must be at least 81984 bytes"
    );
    let store = Store::open(&path, 81984).unwrap();
    assert_eq!(
        store
            .append_history_page(&page("a", &[], &[], 0))
            .unwrap()
            .position
            .0,
        0
    );
    assert_eq!(
        store
            .append_history_page(&page("a", &[1], &[], 1))
            .unwrap()
            .position
            .0,
        1
    );
}

#[test]
fn store_history_legacy_migration_keeps_content_and_next_event_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA auto_vacuum=FULL; CREATE TABLE events(id INTEGER PRIMARY KEY,received_at TEXT NOT NULL,message TEXT NOT NULL); INSERT INTO events VALUES(7,'2026-11-11T00:00:00Z','{\"text\":\"legacy\"}');").unwrap();
    drop(db);
    let store = Store::open(&path, 262144).unwrap();
    store.append_history_page(&page("a", &[1], &[], 1)).unwrap();
    assert_eq!(projections(&path)[0], json!({"text":"legacy"}));
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT MAX(id) FROM events", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        8
    );
    db.execute("DELETE FROM events", []).unwrap();
    drop(store);
    let store = Store::open(&path, 262144).unwrap();
    store.append(&json!({"text":"current"})).unwrap();
    assert_eq!(
        db.query_row("SELECT id FROM events", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_eq!(store.history_cursor("a").unwrap().unwrap().position.0, 1);
}

#[test]
fn diagnostic_capacity_failure_preserves_fifty_committed_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.db");
    let store = Store::open(&path, 262144).unwrap();
    for n in 1..=50 {
        store.append_history_page(&page("a", &[n], &[], n)).unwrap();
    }
    let mut original = Vec::new();
    export(&path, &mut original, &ExportRange::default()).unwrap();
    assert_eq!(original.split(|b| *b == b'\n').count() - 1, 50);
    assert!(store.append(&json!({"text":"x".repeat(150000)})).is_err());
    drop(store);
    let store = Store::open(&path, 262144).unwrap();
    let mut reopened = Vec::new();
    export(&path, &mut reopened, &ExportRange::default()).unwrap();
    assert_eq!(reopened, original);
    assert_eq!(store.history_cursor("a").unwrap().unwrap().position.0, 50);
}

#[test]
fn backfill_survives_display_eviction_by_another_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.db");
    let store = Store::open(&path, 131072).unwrap();
    store
        .append_history_page(&page("a", &[], &[(1, 2)], 2))
        .unwrap();
    let other = Store::open(&path, 131072).unwrap();
    for n in 0..80 {
        other
            .append(&json!({"n":n,"text":"x".repeat(1500)}))
            .unwrap();
    }
    assert!(projections(&path).iter().all(|v| v["reason"].is_null()));
    store.append_history_page(&page("a", &[2], &[], 2)).unwrap();
    let rows = projections(&path);
    assert!(rows.iter().any(|v| v["text"] == "captured"));
    assert!(rows
        .iter()
        .any(|v| v["first_position"] == "1" && v["last_position"] == "1"));
}

#[test]
#[ignore = "allocates a 1 GiB archive"]
fn reopen_gib_archive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.db");
    let store = Store::open(&path, None).unwrap();
    store.append(&json!({"first":true})).unwrap();
    let payload = json!({"text":"x".repeat(4<<20)});
    for _ in 0..256 {
        store.append(&payload).unwrap();
    }
    drop(store);
    let size = path.metadata().unwrap().len();
    assert!(size > 1 << 30);
    let store = Store::open(&path, None).unwrap();
    let (first, count): (String, u64) = store
        .db
        .query_row(
            "SELECT (SELECT message FROM events ORDER BY id LIMIT 1),count(*) FROM events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(first, r#"{"first":true}"#);
    assert_eq!(count, 257);
    eprintln!("archive bytes={size}, retained={count}, first={first}");
}

#[test]
#[ignore = "requires a private 8 MiB tmpfs at TMPDIR"]
fn resume_after_filesystem_full() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.db");
    let store = Store::open(&path, None).unwrap();
    // Fixed maximum allocation even when run outside the test mount.
    let reserve = dir.path().join("reserve");
    std::fs::write(&reserve, vec![0u8; 2 << 20]).unwrap();
    let mut committed = 0;
    for n in 1..=32 {
        let mut p = page("a", &[n], &[], n);
        p.records[0] = record("a", n, &"x".repeat(256 << 10));
        match store.append_history_page(&p) {
            Ok(_) => committed = n,
            Err(e) => {
                assert!(
                    matches!(e.downcast_ref::<rusqlite::Error>(),Some(rusqlite::Error::SqliteFailure(code,_)) if code.code==rusqlite::ErrorCode::DiskFull)
                );
                break;
            }
        }
    }
    assert!((1..32).contains(&committed));
    drop(store);
    std::fs::remove_file(reserve).unwrap();
    let store = Store::open(&path, None).unwrap();
    assert_eq!(
        store.history_cursor("a").unwrap().unwrap().position.0,
        committed
    );
    let expected = "x".repeat(256 << 10);
    let rows = projections(&path);
    assert_eq!(rows.len(), committed as usize);
    assert!(rows.iter().all(|r| r["text"] == expected));
    let next = committed + 1;
    assert_eq!(
        store
            .append_history_page(&page("a", &[next], &[], next))
            .unwrap()
            .position
            .0,
        next
    );
    eprintln!("FULL after {committed} commits, released 2 MiB, resumed at {next}");
}
