#![allow(dead_code)]

mod history_roundtrip;

#[path = "../../debug-channel/guest/src/ancestry.rs"]
mod ancestry;
#[path = "../../debug-channel/guest/src/collector.rs"]
mod collector;
#[path = "../../debug-channel/guest/src/connection.rs"]
mod connection;
include!(concat!(env!("OUT_DIR"), "/guest_modules.rs"));
#[path = "../../debug-channel/guest/src/windows/follow_output.rs"]
mod follow_output;
#[path = "../../debug-channel/guest/src/history.rs"]
mod history;
#[path = "../../debug-channel/host/src/history_sync.rs"]
mod history_sync;
#[path = "../../debug-channel/guest/src/journal.rs"]
mod journal;
#[path = "../../debug-channel/guest/src/kernel_events.rs"]
mod kernel_events;
#[path = "../../debug-channel/guest/src/state.rs"]
mod state;
#[path = "../../debug-channel/host/src/store.rs"]
mod store;
#[path = "../../debug-channel/host/src/transport.rs"]
mod transport;
#[path = "../../debug-channel/guest/src/user_events.rs"]
mod user_events;

use ancestry::{Attribution, Lifecycle};
use connection::{line, Connection};
use follow_output::{FollowPage, PanelOutput};
use history::History;
use journal::{ImportBatch, ImportEntry, ImportSource, Journal, JournalInput, RetentionPolicy};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use store::{ExportRange, Result, Store};
use v1::*;
use venera_debug_protocol::{
    client::{Client, HistoryConsumer, Start},
    matches_filter, v1, Filter, Target,
};

const BOOT: u64 = 134_393_633_875_000_000;
const MS: u64 = (BOOT - 116_444_736_000_000_000) / 10_000;
const ID: &str = "fixture-journal";
const TIMES: [&str; 2] = [
    "2026-11-17T04:29:47.5000300Z",
    "2026-11-17T04:29:47.5000550Z",
];
const TEXTS: [&str; 2] = ["A: before PID reuse", "B: after PID reuse"];

fn cursor(position: u64) -> HistoryCursor {
    HistoryCursor {
        journal_id: ID.into(),
        position: DecimalU64(position),
    }
}

fn identity(pid: u32, offset: u64, key: u64) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        start_time_filetime: Some((BOOT + offset).to_string()),
        boot_time_filetime: Some(BOOT.to_string()),
        start_key: Some(key.to_string()),
        exe_path: Some(format!("C:\\fixture\\{key}.exe")),
    }
}

fn start(p: &ProcessIdentity, parent: Option<&ProcessIdentity>) -> Lifecycle {
    Lifecycle::Start {
        identity: p.clone(),
        parent: parent.map_or(0, |p| p.pid),
        parent_key: parent.map(|p| p.start_key.as_ref().unwrap().parse().unwrap()),
        session: None,
    }
}

fn filter(p: &ProcessIdentity, include_descendants: bool) -> Filter {
    Filter {
        targets: vec![Target::ProcessInstance {
            pid: p.pid,
            start_time_filetime: p.start_time_filetime.clone().unwrap(),
            boot_time_filetime: p.boot_time_filetime.clone(),
            start_key: p.start_key.clone(),
            include_descendants,
        }],
        ..Filter::default()
    }
}

struct Guest(History, Arc<AtomicBool>, thread::JoinHandle<io::Result<()>>);
impl Guest {
    fn open(path: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (history, task) = History::start(
            Journal::open(path, ID, RetentionPolicy::default()).unwrap(),
            stop.clone(),
        )
        .unwrap();
        Self(history, stop, task)
    }
    fn close(self) {
        history::finish(vec![], &self.1, self.2).unwrap();
    }
}

fn respond(conn: &mut Connection, history: &History, bytes: &[u8]) -> Vec<Value> {
    let got = conn.receive_negotiated(
        bytes,
        |_| panic!("unexpected control request"),
        |rpc, _, encoding| history.rpc_encoded(rpc, true, encoding),
    );
    assert_eq!(got.parse_errors, 0, "{}", String::from_utf8_lossy(bytes));
    got.output
}

// Drive the same byte-level negotiation used by the named-pipe client.
struct Local(Client, Connection);
impl Local {
    fn open(history: &History, encoding: HistoryEncoding) -> Self {
        let mut client = if encoding == HistoryEncoding::Attribution {
            Client::with_history_attribution()
        } else {
            Client::default()
        };
        let mut conn = Connection::local(1, false);
        client.connect();
        for request in conn.connect() {
            client.receive(&line(&request).unwrap()).unwrap();
        }
        let mut local = Self(client, conn);
        local.pump(history);
        assert_eq!(local.0.history_encoding(), encoding);
        local
    }
    fn pump(&mut self, history: &History) -> Vec<venera_debug_protocol::client::Reply> {
        let mut bytes = Vec::new();
        self.0.write(&mut bytes).unwrap();
        respond(&mut self.1, history, &bytes)
            .into_iter()
            .flat_map(|r| self.0.receive(&line(&r).unwrap()).unwrap())
            .collect()
    }
    fn call(&mut self, history: &History, method: Method, params: Value) -> Value {
        let id = self.0.request(method, params).unwrap();
        self.pump(history)
            .into_iter()
            .find(|r| r.id == id)
            .unwrap()
            .result
            .unwrap()
    }
    fn read(&mut self, history: &History, after: Option<HistoryCursor>, through: u64) -> Value {
        self.call(
            history,
            Method::HistoryRead,
            json!(HistoryRead {
                after,
                through: Some(DecimalU64(through)),
                limit: 256
            }),
        )
    }
}

struct OneConnection(Option<UnixStream>);
impl transport::Connector for OneConnection {
    fn connect(&mut self, _: Duration) -> io::Result<Box<dyn transport::Stream>> {
        self.0
            .take()
            .map(|s| Box::new(s) as Box<dyn transport::Stream>)
            .ok_or(io::ErrorKind::NotConnected.into())
    }
}

fn projected(db: &Path, position: u64) -> (i64, Value) {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT id,message FROM events WHERE journal_id=?1 AND position=?2",
            (ID, position.to_string()),
            |r| {
                Ok((
                    r.get(0)?,
                    serde_json::from_str(&r.get::<_, String>(1)?).unwrap(),
                ))
            },
        )
        .unwrap()
}

// At the instant the receiver puts an ACK on the wire, a separate DB connection
// can already observe the committed position and attribution projection.
fn sync(db: &Path, history: &History, after: u64, through: u64, expected_key: Option<&str>) {
    let (host, guest) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    guest
        .set_read_timeout(Some(Duration::from_secs(6)))
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(json!(null)));
    let (ack_tx, ack_rx) = mpsc::channel();
    let guest_history = history.clone();
    let db_owned = db.to_owned();
    let server = thread::spawn(move || {
        let mut stream = BufReader::new(guest);
        let mut conn = Connection::new(2);
        for r in conn.connect() {
            stream.get_mut().write_all(&line(&r).unwrap()).unwrap();
        }
        let mut requests = Vec::new();
        loop {
            let mut bytes = Vec::new();
            if stream.read_until(b'\n', &mut bytes).unwrap() == 0 {
                break;
            }
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let responses = conn.receive_negotiated(
                &bytes,
                |_| panic!("unexpected host control request"),
                |rpc, _, encoding| {
                    assert_eq!(encoding, HistoryEncoding::Attribution);
                    guest_history.rpc_encoded(rpc, true, encoding)
                },
            );
            assert_eq!(responses.parse_errors, 0, "{request}");
            if request["method"] == "history.read" {
                requests.push(request["params"]["after"]["position"].clone());
            }
            let mut observation = None;
            if request["method"] == "history.ack" {
                let acknowledge: HistoryAck =
                    serde_json::from_value(request["params"].clone()).unwrap();
                let durable = Store::open(&db_owned, None)
                    .unwrap()
                    .history_cursor(ID)
                    .unwrap()
                    .unwrap();
                assert_eq!(durable, acknowledge.cursor);
                assert_eq!(responses.output[0]["result"], request["params"]);
                let key = projected(&db_owned, 1).1["process"]["start_key"]
                    .as_str()
                    .map(str::to_owned);
                observation = Some((durable, key));
            }
            for r in responses.output {
                stream.get_mut().write_all(&line(&r).unwrap()).unwrap();
            }
            if let Some((durable, key)) = observation {
                ack_tx
                    .send((durable, key, std::mem::take(&mut requests)))
                    .unwrap();
            }
        }
    });
    let worker_stop = stop.clone();
    let worker_status = status.clone();
    let store = Store::open(db, None).unwrap();
    let worker = thread::spawn(move || {
        let (_diagnostics, writer) = history_sync::diagnostics(worker_stop.clone());
        history_sync::run(
            store,
            Arc::new(Mutex::new(OneConnection(Some(host)))),
            worker_stop,
            worker_status,
            writer,
        )
        .unwrap();
    });
    let got = ack_rx.recv_timeout(Duration::from_secs(8));
    if got.is_ok() {
        let deadline = Instant::now() + Duration::from_secs(2);
        while status.lock().unwrap()["state"] != "caught_up" && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
    }
    stop.store(true, Ordering::Release);
    worker.join().unwrap();
    server.join().unwrap();
    let (ack, key, requests) =
        got.unwrap_or_else(|e| panic!("ACK {through}: {e}; status={}", status.lock().unwrap()));
    assert_eq!(requests, [json!(after.to_string())]);
    assert_eq!(ack, cursor(through));
    assert_eq!(key.as_deref(), expected_key);
    assert_eq!(status.lock().unwrap()["state"], "caught_up");
    println!(
        "  receiver resume={after} ack={} process={key:?}",
        ack.position.0
    );
}

fn lines(output: &[u8]) -> Vec<Value> {
    output
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect()
}

fn follow(
    panel: &mut PanelOutput,
    consumer: &mut HistoryConsumer,
    local: &mut Local,
    history: &History,
    f: Filter,
) -> Vec<Value> {
    let range = local.call(history, Method::HistoryRange, json!({}));
    consumer
        .range(serde_json::from_value(range).unwrap(), None)
        .unwrap();
    let payload = local.call(history, Method::HistoryRead, json!(consumer.read_request()));
    let FollowPage::Attribution(page) =
        FollowPage::decode(consumer, local.0.history_encoding(), payload).unwrap()
    else {
        panic!("panel capability")
    };
    let mut out = Vec::new();
    panel.page(consumer, &page, f, &mut out).unwrap();
    lines(&out)
}

fn check_body(row: &LogRecord, index: usize) {
    assert_eq!(row.seq, index as u64 + 1);
    assert_eq!(row.source_time.as_deref(), Some(TIMES[index]));
    assert_eq!(row.text, TEXTS[index]);
}

fn scenario(sealed: bool) {
    println!("{}", if sealed { "sealed" } else { "live" });
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("journal");
    let warm = dir.path().join("warm.db");
    let fresh = dir.path().join("fresh.db");
    let guest = Guest::open(&root);
    let h = &guest.0;
    let source = |id: &str| ImportSource {
        id: id.into(),
        boot_filetime: BOOT,
    };
    let (logs, early, late) = (
        source("sealed-log"),
        source("early-lifecycle"),
        source("late-A-start"),
    );
    h.discover_imports(vec![logs.clone(), early.clone(), late.clone()])
        .unwrap();
    let (a, b, pa, pb) = (
        identity(42, 200, 4201),
        identity(42, 500, 4202),
        identity(10, 100, 1001),
        identity(11, 110, 1101),
    );
    let (fa, fb) = (filter(&pa, true), filter(&pb, true));
    let mut panels = [
        (
            PanelOutput::default(),
            HistoryConsumer::new(Start::Now),
            fa.clone(),
        ),
        (
            PanelOutput::default(),
            HistoryConsumer::new(Start::Now),
            fb.clone(),
        ),
    ];
    let mut local = Local::open(h, HistoryEncoding::Attribution);
    for (panel, consumer, f) in &mut panels {
        assert!(follow(panel, consumer, &mut local, h, f.clone()).is_empty());
        assert_eq!(consumer.cursor(), Some(&cursor(0)));
    }
    let mut state = state::State::new("fixture".into(), true);
    let mut capture = |i: usize, id| {
        state
            .capture_record(
                collector::Capture {
                    pid: Some(42),
                    tid: None,
                    session_id: None,
                    observed_at: "2026-11-17T04:29:48Z".into(),
                    observed_filetime: BOOT + 10_000_000,
                    source_time: Some(TIMES[i].into()),
                    source: "etw".into(),
                    text: TEXTS[i].into(),
                    raw_level: Some("info".into()),
                    process: Some(id),
                    component: Some("guest".into()),
                    structured: None,
                },
                Some(collector::Classification {
                    component: "guest".into(),
                    level: "info".into(),
                    raw_level: "info".into(),
                    tid: None,
                }),
            )
            .unwrap()
    };
    let mut a_row = capture(
        0,
        ProcessIdentity {
            pid: 42,
            ..Default::default()
        },
    );
    let append = |h: &History, record| {
        h.append_durable(JournalInput {
            boot_id: BOOT.to_string(),
            recorded_at_ms: MS,
            startup: true,
            event: HistoryEvent::Log { record },
        })
        .unwrap()
    };
    if !sealed {
        append(h, a_row.clone());
    }
    h.checkpoint_processes(
        early,
        vec![
            start(&pa, None),
            start(&pb, None),
            Lifecycle::End {
                pid: 42,
                start: BOOT + 200,
                end: BOOT + 400,
                key: Some(4201),
            },
            start(&b, Some(&pb)),
        ],
        BOOT + 600,
        MS,
    )
    .unwrap();
    let cp = h.process_checkpoint(BOOT).unwrap();
    assert_eq!(
        cp.attribute_record(&mut a_row, BOOT, BOOT + 300, true),
        Attribution::Missing(42)
    );
    let mut b_row = capture(1, b.clone());
    assert_eq!(
        cp.attribute_record(&mut b_row, BOOT, BOOT + 550, true),
        Attribution::Ready
    );
    if sealed {
        let receipt = h
            .import_batch(ImportBatch {
                source: logs.clone(),
                through: 2,
                complete: true,
                now_ms: MS,
                entries: [a_row, b_row]
                    .into_iter()
                    .enumerate()
                    .map(|(i, record)| ImportEntry {
                        ordinal: i as u64 + 1,
                        source_filetime: BOOT + [300, 550][i],
                        event: HistoryEvent::Log { record },
                    })
                    .collect(),
            })
            .unwrap();
        assert!(receipt.complete && receipt.through == 2);
    } else {
        append(h, b_row);
    }
    assert_eq!(h.range().unwrap().high_water.0, 2);

    let original: AttributionHistoryPage = serde_json::from_value(local.read(h, None, 2)).unwrap();
    assert_eq!(original.records.len(), 2);
    for (i, (panel, consumer, f)) in panels.iter_mut().enumerate() {
        let first = follow(panel, consumer, &mut local, h, f.clone());
        assert_eq!(first.len(), 2);
        for (j, line) in first.iter().enumerate() {
            assert_eq!(line["cursor"], json!(cursor(j as u64 + 1)));
            assert_eq!(line["type"], "log");
            assert_eq!(line["source_time"], TIMES[j]);
            assert_eq!(line["text"], TEXTS[j]);
            assert_eq!(line["visible"], i == 1 && j == 1);
        }
    }
    sync(&warm, h, 0, 2, None);
    let row_id = projected(&warm, 1).0;
    guest.close();
    let guest = Guest::open(&root);
    let h = &guest.0;
    assert_eq!(h.range().unwrap().high_water.0, 2);
    let changes = vec![start(&a, Some(&pa))];
    h.checkpoint_processes(late.clone(), changes.clone(), BOOT + 200, MS)
        .unwrap();
    h.checkpoint_processes(late, changes, BOOT + 200, MS)
        .unwrap();
    assert_eq!(
        h.range().unwrap().high_water.0,
        3,
        "one revision after the same Start is resubmitted"
    );
    if sealed {
        assert_eq!(h.import_receipt(logs).unwrap().through, 2);
    }
    guest.close();
    let guest = Guest::open(&root);
    let h = &guest.0;
    let mut local = Local::open(h, HistoryEncoding::Attribution);
    assert_eq!(h.range().unwrap().high_water.0, 3);
    let delta: AttributionHistoryPage =
        serde_json::from_value(local.read(h, Some(cursor(2)), 3)).unwrap();
    assert_eq!(delta.records.len(), 1);
    assert_eq!(delta.records[0].cursor, cursor(3));
    assert_eq!(
        delta.records[0].event,
        AttributionHistoryEvent::AttributionRevision {
            target: cursor(1),
            attribution: HistoryAttribution {
                process: a.clone(),
                ancestors: vec![pa.clone()]
            }
        }
    );
    for (i, (panel, consumer, f)) in panels.iter_mut().enumerate() {
        let update = follow(panel, consumer, &mut local, h, f.clone());
        assert_eq!(
            update,
            [json!({"type":"attribution_revision","target":cursor(1),
            "attribution":{"process":a,"ancestors":[pa]},"visible":i==0})]
        );
        assert_eq!(consumer.cursor(), Some(&cursor(3)));
        assert!(follow(panel, consumer, &mut local, h, f.clone()).is_empty());
    }
    let mut old = Local::open(h, HistoryEncoding::Compatibility);
    let compatibility: HistoryPage = serde_json::from_value(old.read(h, None, 3)).unwrap();
    let mut consumer = HistoryConsumer::new(Start::Beginning);
    consumer.range(compatibility.range.clone(), None).unwrap();
    let mut display = Vec::new();
    assert_eq!(
        consumer
            .display(&compatibility, &Filter::default(), &mut display)
            .unwrap()
            .cursor,
        cursor(3)
    );
    let display = lines(&display);
    assert_eq!(display.len(), 2);
    for (index, line) in display.iter().enumerate() {
        assert_eq!(line["cursor"], json!(cursor(index as u64 + 1)));
        let row: LogRecord = serde_json::from_value(line.clone()).unwrap();
        check_body(&row, index);
        assert_eq!(row.process.as_ref().unwrap(), [&a, &b][index]);
        assert_eq!(row.ancestors, [[pa.clone()], [pb.clone()]][index]);
    }

    sync(&warm, h, 2, 3, Some("4201"));
    sync(&fresh, h, 0, 3, Some("4201"));
    for db in [&warm, &fresh] {
        // Replay precisely the journal-generated page received over the negotiated wire.
        let archive = Store::open(db, None).unwrap();
        assert_eq!(archive.append_attribution_page(&delta).unwrap(), cursor(3));
    }
    sync(&warm, h, 3, 3, Some("4201"));
    sync(&fresh, h, 3, 3, Some("4201"));
    assert_eq!(projected(&warm, 1).0, row_id);
    for db in [&warm, &fresh] {
        let mut exported = Vec::new();
        store::export(db, &mut exported, &ExportRange::parse(None, None).unwrap()).unwrap();
        let records: Vec<AttributionHistoryRecord> = lines(&exported)
            .into_iter()
            .map(|l| serde_json::from_value(l["message"].clone()).unwrap())
            .collect();
        assert_eq!(
            records,
            [original.records.clone(), delta.records.clone()].concat()
        );
        for i in 0..2 {
            let row: LogRecord = serde_json::from_value(projected(db, i as u64 + 1).1).unwrap();
            check_body(&row, i);
            assert_eq!(row.process.as_ref().unwrap(), [&a, &b][i]);
            assert_eq!(row.ancestors, [[pa.clone()], [pb.clone()]][i]);
            let views = [
                &fa,
                &fb,
                &filter(&a, false),
                &filter(&b, false),
                &filter(&pa, false),
            ];
            assert_eq!(
                views.map(|f| matches_filter(f, &row, &row.ancestors)),
                [i == 0, i == 1, i == 0, i == 1, false]
            );
        }
    }
    println!("  originals=1,2 revision=3->1 exports=2 logs+1 revision per archive panel=2 rows+1 targeted update");
    guest.close();
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--history-roundtrip") {
        return history_roundtrip::run();
    }
    scenario(false);
    scenario(true);
}
