use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_venera-debug-host");

#[test]
fn export_selects_received_time_interval_through_cli() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,received_at TEXT NOT NULL,message TEXT NOT NULL)").unwrap();
    let fixtures = [
        (
            "2026-11-10T08:00:00Z",
            json!({"type":"log","seq":9007199254740993u64,"text":"before"}),
        ),
        (
            "2026-11-10T08:00:01Z",
            json!({"type":"log","seq":18446744073709551615u64,"text":"中文\n🙂"}),
        ),
        (
            "2026-11-10T09:00:01.500+01:00",
            json!({"type":"status","revision":7,"health":{"port":"present"}}),
        ),
        ("2026-11-10T08:00:02Z", json!({"type":"gap","count":2})),
        (
            "2026-11-10T08:00:03Z",
            json!({"type":"log","seq":0,"text":"after"}),
        ),
    ];
    for (received, message) in &fixtures {
        db.execute(
            "INSERT INTO events(received_at,message) VALUES(?1,?2)",
            params![received, message.to_string()],
        )
        .unwrap();
    }
    let run = |args: &[&str]| -> Output {
        Command::new(BIN)
            .arg("export")
            .arg("--database")
            .arg(&path)
            .args(args)
            .output()
            .unwrap()
    };
    let rows = |output: Output| -> Vec<Value> {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    assert_eq!(
        rows(run(&[
            "--from",
            "2026-11-10T09:00:01+01:00",
            "--until",
            "2026-11-10T08:00:02Z"
        ])),
        vec![
            json!({"received_at":"2026-11-10T08:00:01Z","message":{"type":"log","seq":18446744073709551615u64,"text":"中文\n🙂"}}),
            json!({"received_at":"2026-11-10T09:00:01.500+01:00","message":{"type":"status","revision":7,"health":{"port":"present"}}}),
        ]
    );
    let all: Vec<Value> = fixtures
        .iter()
        .map(|(received, message)| json!({"received_at":received,"message":message}))
        .collect();
    assert_eq!(rows(run(&[])), all);
    assert_eq!(
        rows(run(&["--until", "2026-11-10T08:00:01Z"])),
        vec![
            json!({"received_at":"2026-11-10T08:00:00Z","message":{"type":"log","seq":9007199254740993u64,"text":"before"}})
        ]
    );
    assert_eq!(
        rows(run(&["--from", "2026-11-10T08:00:03Z"])),
        vec![
            json!({"received_at":"2026-11-10T08:00:03Z","message":{"type":"log","seq":0,"text":"after"}})
        ]
    );
    assert_eq!(
        rows(run(&["--from", "2026-11-11T00:00:00Z"])),
        Vec::<Value>::new()
    );
    for args in [
        vec!["--from", "yesterday"],
        vec!["--until", "2026-11-10T08:00:02"],
        vec![
            "--from",
            "2026-11-10T08:00:02Z",
            "--until",
            "2026-11-10T08:00:01Z",
        ],
        vec![
            "--from",
            "2026-11-10T08:00:01Z",
            "--until",
            "2026-11-10T08:00:01Z",
        ],
    ] {
        let result = run(&args);
        assert_eq!(result.status.code(), Some(1));
        assert_eq!(result.stdout, b"");
        assert!(!result.stderr.is_empty());
    }
}
