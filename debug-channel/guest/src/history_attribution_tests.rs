use super::*;
use crate::{ancestry::Lifecycle, connection::Connection, journal::RetentionPolicy};
use serde_json::{json, Value};
use venera_debug_protocol::{matches_filter, Filter, LogRecord, ProcessIdentity, Target};

const BOOT: u64 = 134_393_633_875_000_000;
const TIME: u64 = BOOT + 300;
const SOURCE_TIME: &str = "2026-11-17T04:29:47.5000300Z";
const TEXT: &str = "A: log before PID reuse";

fn identity(pid: u32, offset: u64, key: u64) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        boot_time_filetime: Some(BOOT.to_string()),
        start_time_filetime: Some((BOOT + offset).to_string()),
        start_key: Some(key.to_string()),
        ..Default::default()
    }
}

fn start(identity: &ProcessIdentity, parent: Option<&ProcessIdentity>) -> Lifecycle {
    Lifecycle::Start {
        identity: identity.clone(),
        parent: parent.map_or(0, |p| p.pid),
        parent_key: parent.map(|p| p.start_key.as_ref().unwrap().parse().unwrap()),
        session: None,
    }
}

fn read(
    connection: &mut Connection,
    history: &History,
    after: Option<&str>,
    through: Option<&str>,
) -> Value {
    let reply = connection.receive_negotiated(
        &crate::connection::line(&json!({"jsonrpc":"2.0","id":"page","method":"history.read",
            "params":{"after":after.map(|position| json!({"journal_id":"fixture-journal","position":position})),
                "through":through,"limit":256}})).unwrap(),
        |_| None,
        |request, _, encoding| history.rpc_encoded(request, true, encoding),
    );
    assert_eq!(reply.output.len(), 1);
    assert!(reply.output[0].get("error").is_none(), "{:?}", reply.output);
    reply.output[0]["result"].clone()
}

fn handshake(local_pipe: bool, capable: bool) -> Connection {
    let mut connection = if local_pipe {
        Connection::local(1, true)
    } else {
        Connection::new(1)
    };
    let [endpoint, discover] = connection.connect();
    let mut info = venera_debug_protocol::v1::EndpointInfo::new(
        venera_debug_protocol::v1::EndpointRole::Host,
        "fixture",
    );
    if capable {
        info = info.with_history_attribution();
    }
    let discovery: Value =
        serde_json::from_str(include_str!("../../protocol/v1/host.openrpc.json")).unwrap();
    let reply = connection.receive(
        &crate::connection::line(&json!([
            {"jsonrpc":"2.0","id":endpoint["id"],"result":info},
            {"jsonrpc":"2.0","id":discover["id"],"result":discovery},
            {"jsonrpc":"2.0","id":"guest-info","method":"endpoint.info","params":{}}
        ]))
        .unwrap(),
        |_| None,
    );
    assert_eq!(
        reply.output[0][0]["result"]["capabilities"],
        json!(["history.attribution"])
    );
    connection
}

#[test]
fn late_pid_attribution_resumes_through_history_read_after_reopen() {
    let a_parent = identity(10, 100, 1001);
    let a = identity(42, 200, 4201);
    let b_parent = identity(11, 400, 1002);
    let b = identity(42, 500, 4202);
    let now_ms = (TIME - UNIX_EPOCH_FILETIME) / 10_000;

    for sealed in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "venera-late-revision-{}-{sealed}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let policy = RetentionPolicy {
            runtime_ms: 30,
            ..Default::default()
        };
        let launch = || {
            let journal = Journal::open(&root, "fixture-journal", policy.clone()).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (history, worker) = History::start(journal, stop.clone()).unwrap();
            (history, stop, worker)
        };
        let (history, stop, worker) = launch();
        let source = ImportSource {
            id: "late-evidence.etl".into(),
            boot_filetime: BOOT,
        };
        history.discover_imports(vec![source.clone()]).unwrap();
        let event: HistoryEvent = serde_json::from_value(json!({"type":"log","record":{
            "stream_id":"fixture","seq":"1","observed_at":"2026-11-17T04:29:48Z",
            "source_time":SOURCE_TIME,"source":"etw","component":"guest","level":"info","raw_level":"info",
            "process":{"pid":42},"text":TEXT
        }})).unwrap();
        if sealed {
            history
                .import_batch(ImportBatch {
                    source: source.clone(),
                    through: 1,
                    complete: false,
                    now_ms,
                    entries: vec![crate::journal::ImportEntry {
                        ordinal: 1,
                        source_filetime: TIME,
                        event,
                    }],
                })
                .unwrap();
        } else {
            history
                .append_durable(JournalInput {
                    boot_id: BOOT.to_string(),
                    recorded_at_ms: now_ms,
                    startup: true,
                    event,
                })
                .unwrap();
        }

        let mut connection = handshake(sealed, true);
        let first = read(&mut connection, &history, None, None);
        let original = first["records"][0].clone();
        assert_eq!(first["next"]["position"], "1");
        assert_eq!(original["event"]["record"]["text"], TEXT);
        assert_eq!(original["event"]["record"]["process"]["pid"], 42);
        assert!(original["event"]["record"]["process"]["start_time_filetime"].is_null());

        history
            .checkpoint_processes(
                source.clone(),
                vec![start(&b_parent, None), start(&b, Some(&b_parent))],
                BOOT + 600,
                now_ms,
            )
            .unwrap();
        assert_eq!(
            read(&mut connection, &history, Some("1"), None)["next"]["position"],
            "1"
        );
        let a_evidence = vec![
            start(&a_parent, None),
            start(&a, Some(&a_parent)),
            Lifecycle::End {
                pid: 42,
                start: BOOT + 200,
                end: BOOT + 350,
                key: Some(4201),
            },
        ];
        history
            .checkpoint_processes(source.clone(), a_evidence.clone(), BOOT + 700, now_ms)
            .unwrap();
        let changed = read(&mut connection, &history, Some("1"), None);
        assert_eq!(changed["records"][0]["cursor"]["position"], "2");
        assert_eq!(
            changed["records"][0]["event"],
            json!({
                "type":"attribution_revision","target":{"journal_id":"fixture-journal","position":"1"},
                "attribution":{"process":a,"ancestors":[a_parent]}
            })
        );
        assert_eq!(changed["next"]["position"], "2");
        finish(vec![], &stop, worker).unwrap();

        let (history, stop, worker) = launch();
        let mut connection = handshake(!sealed, true);
        assert_eq!(read(&mut connection, &history, Some("1"), None), changed);
        assert_eq!(
            read(&mut connection, &history, None, Some("1"))["records"][0],
            original
        );
        history
            .checkpoint_processes(source.clone(), a_evidence, BOOT + 700, now_ms)
            .unwrap();
        assert_eq!(
            read(&mut connection, &history, Some("2"), None)["next"]["position"],
            "2"
        );

        let mut legacy = handshake(!sealed, false);
        let current = read(&mut legacy, &history, None, None);
        assert_eq!(
            current["records"][1]["event"]["gap"]["reason"],
            "attribution_revision"
        );
        assert_eq!(current["records"][1]["event"]["gap"]["count"], "0");
        let reread: LogRecord =
            serde_json::from_value(current["records"][0]["event"]["record"].clone()).unwrap();
        assert_eq!(
            (reread.text.as_str(), reread.source_time.as_deref()),
            (TEXT, Some(SOURCE_TIME))
        );
        let filter = |p: &ProcessIdentity, descendants| Filter {
            targets: vec![Target::ProcessInstance {
                pid: p.pid,
                start_time_filetime: p.start_time_filetime.clone().unwrap(),
                boot_time_filetime: p.boot_time_filetime.clone(),
                start_key: p.start_key.clone(),
                include_descendants: descendants,
            }],
            ..Default::default()
        };
        assert!(matches_filter(
            &filter(&a, false),
            &reread,
            &reread.ancestors
        ));
        assert!(matches_filter(
            &filter(&a_parent, true),
            &reread,
            &reread.ancestors
        ));
        assert!(!matches_filter(
            &filter(&b, false),
            &reread,
            &reread.ancestors
        ));
        assert!(!matches_filter(
            &filter(&b_parent, true),
            &reread,
            &reread.ancestors
        ));
        finish(vec![], &stop, worker).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
