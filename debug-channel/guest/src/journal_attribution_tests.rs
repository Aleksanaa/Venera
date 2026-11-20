use crate::ancestry::Lifecycle;
use venera_debug_protocol::ProcessIdentity;

#[test]
fn interrupted_revision_publish_recovers_position_and_keeps_snapshot_with_target() {
    const BOOT: u64 = 134_393_633_875_000_000;
    const TIME: u64 = (BOOT - 116_444_736_000_000_000) / 10_000;
    let directory = Directory::new();
    let policy = RetentionPolicy {
        runtime_ms: 30,
        startup_boots: 1,
        ..Default::default()
    };
    let mut journal = directory.open(policy.clone());
    let source = ImportSource {
        id: "late.etl".into(),
        boot_filetime: BOOT,
    };
    journal.discover_imports(vec![source.clone()]).unwrap();
    journal
        .append(
            &BOOT.to_string(),
            TIME,
            true,
            serde_json::from_value(json!({"type":"log","record":{
                "stream_id":"fixture","seq":"1","observed_at":"2026-11-17T04:29:48Z",
                "source_time":"2026-11-17T04:29:47.5000300Z","source":"etw","component":"guest",
                "level":"info","raw_level":"info","process":{"pid":42},"text":"old process"
            }}))
            .unwrap(),
        )
        .unwrap();
    journal
        .append(&BOOT.to_string(), TIME, false, event("runtime history"))
        .unwrap();
    let changes = vec![Lifecycle::Start {
        identity: ProcessIdentity {
            pid: 42,
            boot_time_filetime: Some(BOOT.to_string()),
            start_time_filetime: Some((BOOT + 200).to_string()),
            start_key: Some("4201".into()),
            ..Default::default()
        },
        parent: 0,
        parent_key: None,
        session: None,
    }];
    // Leave revision bytes outside the published generation.
    INTERRUPT.set(Some("sync"));
    assert_eq!(
        journal
            .checkpoint_processes(&source, changes.clone(), BOOT + 350, TIME)
            .unwrap_err()
            .to_string(),
        "sync"
    );
    drop(journal);

    let mut journal = directory.open(policy.clone());
    assert_eq!(journal.range().unwrap().high_water.0, 3);
    journal
        .checkpoint_processes(&source, changes, BOOT + 350, TIME)
        .unwrap();
    assert_eq!(journal.range().unwrap().high_water.0, 3);
    journal.prune(TIME + 31).unwrap();
    journal.close().unwrap();

    let mut journal = directory.open(policy);
    let request = HistoryRead {
        after: Some(HistoryCursor {
            journal_id: "journal-a".into(),
            position: DecimalU64(1),
        }),
        through: None,
        limit: 256,
    };
    let page = journal
        .read_encoded(request.clone(), 8192, HistoryEncoding::Attribution)
        .unwrap();
    assert_eq!(
        page["missing"],
        json!([{"first":"2","last":"2","reason":"retention_expired"}])
    );
    assert_eq!(
        page["records"][0]["event"]["attribution"]["process"]["start_key"],
        "4201"
    );
    assert_eq!(page["records"][0]["event"]["target"]["position"], "1");
    assert_eq!(page["records"][0]["cursor"]["position"], "3");

    journal
        .append(
            &(BOOT + 1_000_000).to_string(),
            TIME + 100,
            true,
            event("next boot"),
        )
        .unwrap();
    let page = journal
        .read_encoded(
            HistoryRead {
                after: None,
                ..request
            },
            8192,
            HistoryEncoding::Attribution,
        )
        .unwrap();
    assert_eq!(
        page["missing"],
        json!([{"first":"1","last":"3","reason":"retention_expired"}])
    );
    assert_eq!(page["records"][0]["cursor"]["position"], "4");
}
