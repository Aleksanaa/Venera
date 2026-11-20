use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(8);

struct Service {
    child: Child,
    log: PathBuf,
}

impl Service {
    fn start(dir: &Path) -> Self {
        Self::with_budget(dir, 1048576)
    }

    fn with_budget(dir: &Path, budget: u64) -> Self {
        Self::launch(dir, budget, None)
    }

    fn launch(dir: &Path, budget: u64, web: Option<std::net::SocketAddr>) -> Self {
        let log = dir.join("service.log");
        let output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "injected_service", "--nocapture"])
            .env("VENERA_HISTORY_TEST_DIR", dir)
            .env("VENERA_HISTORY_TEST_BUDGET", budget.to_string())
            .env(
                "VENERA_HISTORY_TEST_WEB",
                web.map(|addr| addr.to_string()).unwrap_or_default(),
            )
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap();
        Self { child, log }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            eprintln!(
                "service log:\n{}",
                std::fs::read_to_string(&self.log).unwrap()
            );
        }
    }
}

struct Guest {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    journal: &'static str,
    oldest: u64,
    high_water: u64,
    queued: Option<Value>,
}

impl Guest {
    fn accept(
        listener: &UnixListener,
        journal: &'static str,
        oldest: u64,
        high_water: u64,
    ) -> Self {
        Self::accept_with_status_burst(listener, journal, oldest, high_water, 1)
    }

    fn accept_with_status_burst(
        listener: &UnixListener,
        journal: &'static str,
        oldest: u64,
        high_water: u64,
        burst: usize,
    ) -> Self {
        Self::negotiate(listener, journal, oldest, high_water, burst, false)
    }

    fn negotiate(
        listener: &UnixListener,
        journal: &'static str,
        oldest: u64,
        high_water: u64,
        burst: usize,
        attribution: bool,
    ) -> Self {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        let writer = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "guest connection deadline");
                    sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("guest connection: {error}"),
            }
        };
        writer.set_read_timeout(Some(TIMEOUT)).unwrap();
        writer.set_write_timeout(Some(TIMEOUT)).unwrap();
        let mut guest = Self {
            reader: BufReader::new(writer.try_clone().unwrap()),
            writer,
            journal,
            oldest,
            high_water,
            queued: None,
        };
        loop {
            let mut line = String::new();
            if guest.reader.read_line(&mut line).unwrap() == 0 {
                return Self::negotiate(listener, journal, oldest, high_water, burst, attribution);
            }
            if line.trim().is_empty() {
                continue;
            }
            let request: Value = serde_json::from_str(&line).unwrap();
            match request["method"].as_str().unwrap() {
                "endpoint.info" => guest.reply(&request, json!({"role":"guest","software_version":"history-fixture","protocol_version":"1.0.0",
                    "capabilities": if attribution { vec!["history.attribution"] } else { vec![] }})),
                "rpc.discover" => guest.reply(&request, discovery()),
                "history.range" => { guest.queued = Some(request); return guest; }
                "status.get" => {
                    std::thread::spawn(move || {
                        let status = json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}});
                        guest.reply(&request, status.clone());
                        loop {
                            let mut line = String::new();
                            match guest.reader.read_line(&mut line) { Ok(0) | Err(_) => break, _ => () }
                            if line.trim().is_empty() { continue; }
                            let request: Value = serde_json::from_str(&line).unwrap();
                            for _ in 0..burst {guest.send(json!({"jsonrpc":"2.0","method":"status.changed","params":status.clone()}));}
                            guest.reply(&request, status.clone());
                        }
                    });
                    return Self::negotiate(listener, journal, oldest, high_water, burst, attribution);
                }
                method => panic!("unexpected initial method {method}"),
            }
        }
    }

    fn send(&mut self, value: Value) {
        writeln!(self.writer, "{value}").unwrap();
    }

    fn reply(&mut self, request: &Value, value: Value) {
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"result":value}));
    }

    fn range(&self) -> Value {
        json!({"journal_id":self.journal,"oldest":self.oldest.to_string(),"high_water":self.high_water.to_string()})
    }

    // Dispatch negotiation and control traffic independently of history scheduling.
    fn until(&mut self, method: &str) -> Value {
        self.until_one_of(&[method])
    }

    fn until_one_of(&mut self, methods: &[&str]) -> Value {
        let method = methods.join(" or ");
        let deadline = Instant::now() + TIMEOUT;
        let mut observed = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "waiting for {method}");
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining))
                .unwrap();
            let request: Value = if let Some(request) = self.queued.take() {
                request
            } else {
                let mut line = String::new();
                let length = self.reader.read_line(&mut line).unwrap_or_else(|error| {
                    panic!("waiting for {method}: {error}; received {observed:?}")
                });
                assert!(length > 0, "guest disconnected while waiting for {method}");
                if line.trim().is_empty() {
                    continue;
                }
                serde_json::from_str(&line).unwrap()
            };
            let name = request["method"].as_str().unwrap();
            observed.push(name.to_owned());
            if methods.contains(&name) {
                return request;
            }
            let response = match name {
                "endpoint.info" => {
                    json!({"role":"guest","software_version":"history-fixture","protocol_version":"1.0.0"})
                }
                "rpc.discover" => discovery(),
                "status.get" => {
                    json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}})
                }
                "history.range" => self.range(),
                "history.ack" => request["params"].clone(),
                "history.read" => {
                    let position = request["params"]["after"]["position"]
                        .as_str()
                        .unwrap_or("0");
                    json!({"range":self.range(),"records":[],"missing":[],
                        "next":{"journal_id":self.journal,"position":position}})
                }
                other => panic!("unexpected guest method {other} while waiting for {method}"),
            };
            self.reply(&request, response);
        }
    }

    fn page(&mut self, request: &Value, records: Value, missing: Value, next: u64) {
        self.reply(
            request,
            json!({
                "range":self.range(), "records":records, "missing":missing,
                "next":{"journal_id":self.journal,"position":next.to_string()}
            }),
        );
    }

    fn assert_history_idle(&mut self) {
        self.reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        let mut line = String::new();
        let result = self.reader.read_line(&mut line);
        assert!(
            matches!(result, Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "unexpected history traffic: {line}"
        );
    }

    fn ack(&mut self, position: &str) -> Value {
        let request = self.until("history.ack");
        assert_eq!(
            request["params"],
            json!({"cursor":{"journal_id":self.journal,"position":position}})
        );
        request
    }
}

fn discovery() -> Value {
    // Frozen method names keep the peer independent of generated protocol documents.
    json!({"openrpc":"1.4.1","info":{"title":"History fixture","version":"1.0.0"},"methods":[
        {"name":"endpoint.info","paramStructure":"by-name","params":[]},
        {"name":"rpc.discover","paramStructure":"by-name","params":[]},
        {"name":"status.get","paramStructure":"by-name","params":[]},
        {"name":"history.range","paramStructure":"by-name","params":[]},
        {"name":"history.read","paramStructure":"by-name","params":[]},
        {"name":"history.ack","paramStructure":"by-name","params":[]}
    ],"components":{"schemas":{}}})
}

fn record(journal: &str, position: u64, text: &str) -> Value {
    json!({
        "cursor":{"journal_id":journal,"position":position.to_string()},
        "boot_id":"boot-one","recorded_at_ms":"1794297601000","startup":false,
        "event":{"type":"log","record":{
            "stream_id":"capture-one","seq":position.to_string(),"observed_at":"2026-11-10T08:00:01Z",
            "source_time":null,"source":"dbwin","component":"dxvk","level":"trace","raw_level":"trace",
            "session_id":2,"process":null,"tid":null,"text":text
        }}
    })
}

fn texts(dir: &Path) -> Vec<String> {
    let db = Connection::open(dir.join("history.db")).unwrap();
    db.busy_timeout(Duration::from_secs(2)).unwrap();
    let mut statement = db.prepare("SELECT json_extract(message,'$.text') FROM events WHERE json_extract(message,'$.type')='log' ORDER BY id").unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn identities(dir: &Path) -> Vec<Value> {
    let db = Connection::open(dir.join("history.db")).unwrap();
    db.busy_timeout(Duration::from_secs(2)).unwrap();
    let mut statement = db.prepare("SELECT journal_id,position,raw_message FROM events WHERE position IS NOT NULL ORDER BY id").unwrap();
    statement
        .query_map([], |row| {
            let raw: String = row.get(2)?;
            Ok(
                json!({"journal_id":row.get::<_,String>(0)?,"position":row.get::<_,String>(1)?,
            "envelope":serde_json::from_str::<Value>(&raw).unwrap()["cursor"]}),
            )
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn missing(dir: &Path) -> Vec<Value> {
    let db = Connection::open(dir.join("history.db")).unwrap();
    db.busy_timeout(Duration::from_secs(2)).unwrap();
    let mut statement = db.prepare("SELECT message FROM events WHERE json_extract(message,'$.reason')='retention_expired' ORDER BY id").unwrap();
    statement
        .query_map([], |row| {
            let message: String = row.get(0)?;
            let message: Value = serde_json::from_str(&message).unwrap();
            Ok(
                json!({"journal_id":message["journal_id"],"first":message["first_position"],
            "last":message["last_position"],"reason":message["reason"]}),
            )
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn first_read(guest: &mut Guest) -> Value {
    let request = guest.until("history.read");
    let after = &request["params"]["after"];
    assert!(
        after.is_null() || *after == json!({"journal_id":guest.journal,"position":"0"}),
        "initial cursor: {after}"
    );
    request
}

fn control(dir: &Path, method: &str) -> Value {
    let mut stream = UnixStream::connect(dir.join("control.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        json!({"jsonrpc":"2.0","id":"control-test","method":method,"params":{}})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], "control-test");
    response["result"].clone()
}

fn wait_for_capacity(dir: &Path, position: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let history = &control(dir, "connection.get")["history"];
        if history["state"] == "storage_error" {
            assert_eq!(history["cursor"]["position"], position);
            return;
        }
        assert!(Instant::now() < deadline, "history: {history}");
        sleep(Duration::from_millis(10));
    }
}

#[test]
fn archive_reduces_pages_to_capacity_and_acknowledges_committed_prefixes() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _service = Service::with_budget(dir.path(), 262144);
    let mut guest = Guest::accept(&listener, "journal-a", 1, 256);
    let read = first_read(&mut guest);
    let records: Vec<_> = (1..=256)
        .map(|p| record("journal-a", p, &"x".repeat(1024)))
        .collect();
    guest.page(&read, json!(records), json!([]), 256);
    let db = Connection::open(dir.path().join("history.db")).unwrap();
    let mut after = 0;
    while after < 256 {
        let ack = guest.until_one_of(&["history.read", "history.ack"]);
        assert_eq!(ack["method"], "history.ack");
        let next: u64 = ack["params"]["cursor"]["position"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(next > after && next <= 256);
        if after == 0 {
            assert!(next < 256);
        }
        let position: String = db
            .query_row(
                "SELECT position FROM history_checkpoints WHERE journal_id='journal-a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(position, next.to_string());
        guest.reply(&ack, ack["params"].clone());
        after = next;
    }
    assert_eq!(identities(dir.path()).last().unwrap()["position"], "256");
    assert_eq!(texts(dir.path()).last().unwrap(), &"x".repeat(1024));
}

#[test]
fn delayed_reader_retries_and_commits_inside_an_existing_retention_gap() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _service = Service::with_budget(dir.path(), 262144);
    let mut guest = Guest::accept(&listener, "j", 1, 6);
    let read = first_read(&mut guest);

    // Frozen archive representation from the original missing-page writer.
    let db = Connection::open(dir.path().join("history.db")).unwrap();
    db.execute_batch(r#"BEGIN IMMEDIATE;
        INSERT INTO events(received_at,message,journal_id) VALUES('2026-11-11T00:00:00Z',
        '{"major":1,"type":"gap","stream_id":"j","journal_id":"j","reason":"retention_expired","count":"6","first_seq":null,"last_seq":null,"first_position":"1","last_position":"6"}', 'j');
        INSERT INTO history_checkpoints VALUES('j','6'); COMMIT;"#).unwrap();
    let expired = json!([{"first":"1","last":"2","reason":"retention_expired"}]);
    guest.page(
        &read,
        json!([record("j", 3, "three"), record("j", 4, &"x".repeat(100000))]),
        expired,
        4,
    );
    guest.oldest = 7;
    loop {
        let request = guest.until_one_of(&["history.read", "history.ack"]);
        if request["method"] == "history.read" {
            let after: u64 = request["params"]["after"]["position"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            guest.page(
                &request,
                json!([]),
                json!([{"first":(after+1).to_string(),"last":"6","reason":"retention_expired"}]),
                6,
            );
            continue;
        }
        let pos = request["params"]["cursor"]["position"].as_str().unwrap();
        assert!(matches!(pos, "2" | "3"), "acknowledged {pos}");
        guest.reply(&request, request["params"].clone());
        if pos == "3" {
            break;
        }
    }
    wait_for_capacity(dir.path(), "3");
    guest.assert_history_idle();
    assert_eq!(texts(dir.path()), ["three"]);
    assert_eq!(
        identities(dir.path()),
        [json!({"journal_id":"j","position":"3","envelope":{"journal_id":"j","position":"3"}})]
    );
    let mut stmt = db.prepare("SELECT json_extract(message,'$.first_position'),json_extract(message,'$.last_position'),json_extract(message,'$.count') FROM events WHERE journal_id='j' AND position IS NULL ORDER BY id").unwrap();
    let gaps: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        gaps,
        [
            ("1".into(), "2".into(), "2".into()),
            ("4".into(), "6".into(), "3".into())
        ]
    );
    assert_eq!(
        db.query_row(
            "SELECT position FROM history_checkpoints WHERE journal_id='j'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "6"
    );
}

#[test]
fn archive_capacity_error_keeps_single_record_progress_pending() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _service = Service::with_budget(dir.path(), 262144);
    let mut guest = Guest::accept(&listener, "journal-a", 1, 1);
    let read = first_read(&mut guest);
    guest.page(
        &read,
        json!([record("journal-a", 1, &"x".repeat(100000))]),
        json!([]),
        1,
    );
    wait_for_capacity(dir.path(), "0");
    guest.assert_history_idle();
    let db = Connection::open(dir.path().join("history.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM history_checkpoints", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn archive_write_lock_preserves_control_responses_and_flushes_diagnostics() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let mut service = Service::start(dir.path());
    let mut guest = Guest::accept_with_status_burst(&listener, "journal-a", 1, 1, 60);
    let read = first_read(&mut guest);
    let db = Connection::open(dir.path().join("history.db")).unwrap();
    db.busy_timeout(Duration::from_secs(2)).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    guest.page(
        &read,
        json!([record("journal-a", 1, "locked history")]),
        json!([]),
        1,
    );
    let release = Instant::now() + Duration::from_millis(5750);
    for _ in 0..5 {
        assert_eq!(control(dir.path(), "status.get")["revision"], "1");
        sleep(Duration::from_millis(500));
    }
    assert_eq!(service.child.try_wait().unwrap(), None);
    assert_eq!(
        control(dir.path(), "connection.get")["archive_writes"]["state"],
        "waiting"
    );
    sleep(release.saturating_duration_since(Instant::now()));
    db.execute_batch("COMMIT").unwrap();
    let ack = guest.ack("1");
    guest.reply(&ack, ack["params"].clone());
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let count: i64 = db
            .query_row(
                "SELECT coalesce(sum(CASE WHEN json_extract(message,'$.type')='status' THEN 1 WHEN json_extract(message,'$.kind')='status' THEN json_extract(message,'$.count') ELSE 0 END),0) FROM events",
                [],
                |r| r.get(0),
            )
            .unwrap();
        if count == 300 {
            break;
        }
        assert!(Instant::now() < deadline, "archived status count: {count}");
        sleep(Duration::from_millis(10));
    }
    assert_eq!(texts(dir.path()), ["locked history"]);
    assert_eq!(
        control(dir.path(), "connection.get")["archive_writes"]["state"],
        "ready"
    );
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(control(dir.path(), "status.get")["revision"], "1");
    assert_eq!(
        unsafe { libc::kill(service.child.id() as i32, libc::SIGTERM) },
        0
    );
    sleep(Duration::from_millis(100));
    db.execute_batch("COMMIT").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(exit) = service.child.try_wait().unwrap() {
            assert_eq!(exit.code(), Some(0));
            break;
        }
        assert!(Instant::now() < deadline, "service shutdown deadline");
        sleep(Duration::from_millis(10));
    }
    assert_eq!(
        db.query_row(
            "SELECT coalesce(sum(CASE WHEN json_extract(message,'$.type')='status' THEN 1 WHEN json_extract(message,'$.kind')='status' THEN json_extract(message,'$.count') ELSE 0 END),0) FROM events",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        360
    );
}

#[test]
fn archive_connection_replays_from_committed_progress_after_disconnect() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _service = Service::start(dir.path());
    let mut guest = Guest::accept(&listener, "journal-a", 1, 2);
    let read = first_read(&mut guest);
    guest.page(
        &read,
        json!([record("journal-a", 1, "one"), record("journal-a", 2, "two")]),
        json!([]),
        2,
    );
    guest.ack("2");
    drop(guest);
    let mut guest = Guest::accept(&listener, "journal-a", 1, 3);
    let read = guest.until("history.read");
    assert_eq!(
        read["params"]["after"],
        json!({"journal_id":"journal-a","position":"2"})
    );
    guest.page(
        &read,
        json!([record("journal-a", 3, "three")]),
        json!([]),
        3,
    );
    guest.ack("3");
    assert_eq!(texts(dir.path()), ["one", "two", "three"]);
}

#[test]
fn two_archives_consume_one_journal_with_independent_progress() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let listener = UnixListener::bind(first.path().join("guest.sock")).unwrap();
    std::os::unix::fs::symlink(
        first.path().join("guest.sock"),
        second.path().join("guest.sock"),
    )
    .unwrap();
    let _first_service = Service::start(first.path());
    let mut a = Guest::accept(&listener, "journal-a", 1, 2);
    let a_read = first_read(&mut a);
    a.page(
        &a_read,
        json!([record("journal-a", 1, "one"), record("journal-a", 2, "two")]),
        json!([]),
        2,
    );
    a.ack("2");
    let _second_service = Service::start(second.path());
    let mut b = Guest::accept(&listener, "journal-a", 1, 2);
    let b_read = first_read(&mut b);
    b.page(
        &b_read,
        json!([record("journal-a", 1, "one"), record("journal-a", 2, "two")]),
        json!([]),
        2,
    );
    b.ack("2");
    assert_eq!(texts(first.path()), ["one", "two"]);
    assert_eq!(texts(second.path()), ["one", "two"]);
}

#[test]
fn forwarding_pause_resumes_history_from_the_same_cursor() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _service = Service::start(dir.path());
    let mut guest = Guest::accept(&listener, "journal-a", 1, 2);
    let read = first_read(&mut guest);
    guest.send(json!({"jsonrpc":"2.0","id":read["id"],"error":{"code":-32005,"message":"Log forwarding is paused"}}));
    let resumed = guest.until("history.read");
    assert_eq!(
        resumed["params"],
        json!({"after":{"journal_id":"journal-a","position":"0"},"through":"2","limit":256})
    );
    guest.page(
        &resumed,
        json!([
            record("journal-a", 1, "paused one"),
            record("journal-a", 2, "paused two")
        ]),
        json!([]),
        2,
    );
    guest.ack("2");
    assert_eq!(texts(dir.path()), ["paused one", "paused two"]);
}

#[test]
fn control_connection_responds_while_history_page_is_pending() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _service = Service::start(dir.path());
    let mut guest = Guest::accept(&listener, "journal-a", 1, 1);
    let read = first_read(&mut guest);
    let mut control = UnixStream::connect(dir.path().join("control.sock")).unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    writeln!(
        control,
        "{}",
        json!({"jsonrpc":"2.0","id":"while-history-waits","method":"status.get","params":{}})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(control).read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        response,
        json!({"jsonrpc":"2.0","id":"while-history-waits","result":{"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}})
    );
    guest.page(
        &read,
        json!([record("journal-a", 1, "history completed")]),
        json!([]),
        1,
    );
    guest.ack("1");
    assert_eq!(texts(dir.path()), ["history completed"]);
}

#[test]
fn backlog_and_live_overlap_commit_once_before_ack() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _service = Service::start(dir.path());
    let mut guest = Guest::accept(&listener, "journal-a", 1, 3);
    let read = first_read(&mut guest);
    let live = record("journal-a", 3, "live overlap");
    guest.send(json!({"jsonrpc":"2.0","method":"log","params":live["event"]["record"]}));
    guest.page(
        &read,
        json!([
            record("journal-a", 1, "backlog one"),
            record("journal-a", 2, "backlog two"),
            live
        ]),
        json!([]),
        3,
    );
    let ack = guest.ack("3");
    assert_eq!(
        texts(dir.path()),
        ["backlog one", "backlog two", "live overlap"]
    );
    assert_eq!(
        identities(dir.path()),
        vec![
            json!({"journal_id":"journal-a","position":"1","envelope":{"journal_id":"journal-a","position":"1"}}),
            json!({"journal_id":"journal-a","position":"2","envelope":{"journal_id":"journal-a","position":"2"}}),
            json!({"journal_id":"journal-a","position":"3","envelope":{"journal_id":"journal-a","position":"3"}}),
        ]
    );
    guest.reply(&ack, ack["params"].clone());
}

#[test]
fn restart_after_commit_resumes_and_replayed_records_stay_unique() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let service = Service::start(dir.path());
    let mut guest = Guest::accept(&listener, "journal-a", 1, 2);
    let read = first_read(&mut guest);
    guest.page(
        &read,
        json!([record("journal-a", 1, "one"), record("journal-a", 2, "two")]),
        json!([]),
        2,
    );
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let saved = texts(dir.path());
        if saved == ["one", "two"] {
            break;
        }
        assert!(Instant::now() < deadline, "committed history: {saved:?}");
        sleep(Duration::from_millis(10));
    }
    // Close without receiving confirmation, then reopen the same host database.
    drop(guest);
    drop(service);
    let _service = Service::start(dir.path());
    let mut guest = Guest::accept(&listener, "journal-a", 1, 3);
    let read = guest.until("history.read");
    assert_eq!(
        read["params"]["after"],
        json!({"journal_id":"journal-a","position":"2"})
    );
    guest.page(
        &read,
        json!([record("journal-a", 3, "three")]),
        json!([]),
        3,
    );
    guest.ack("3");
    assert_eq!(texts(dir.path()), ["one", "two", "three"]);
}

#[test]
fn missing_intervals_and_retained_records_resume_together() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let service = Service::start(dir.path());
    let mut guest = Guest::accept(&listener, "journal-a", 3, 5);
    let read = first_read(&mut guest);
    guest.page(
        &read,
        json!([
            record("journal-a", 3, "three"),
            record("journal-a", 5, "five")
        ]),
        json!([
            {"first":"1","last":"2","reason":"retention_expired"},
            {"first":"4","last":"4","reason":"retention_expired"}
        ]),
        5,
    );
    guest.ack("5");
    assert_eq!(texts(dir.path()), ["three", "five"]);
    let expected_missing = vec![
        json!({"journal_id":"journal-a","first":"1","last":"2","reason":"retention_expired"}),
        json!({"journal_id":"journal-a","first":"4","last":"4","reason":"retention_expired"}),
    ];
    assert_eq!(missing(dir.path()), expected_missing);
    drop(guest);
    drop(service);
    let _service = Service::start(dir.path());
    let mut guest = Guest::accept(&listener, "journal-a", 3, 6);
    let read = guest.until("history.read");
    assert_eq!(
        read["params"]["after"],
        json!({"journal_id":"journal-a","position":"5"})
    );
    guest.page(&read, json!([record("journal-a", 6, "six")]), json!([]), 6);
    guest.ack("6");
    assert_eq!(texts(dir.path()), ["three", "five", "six"]);
    assert_eq!(missing(dir.path()), expected_missing);
}

#[test]
fn journal_replacement_keeps_independent_positions() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    for (journal, text, expected) in [
        ("journal-a", "first journal", vec!["first journal"]),
        (
            "journal-b",
            "second journal",
            vec!["first journal", "second journal"],
        ),
    ] {
        let _service = Service::start(dir.path());
        let mut guest = Guest::accept(&listener, journal, 1, 1);
        let read = first_read(&mut guest);
        guest.page(&read, json!([record(journal, 1, text)]), json!([]), 1);
        guest.ack("1");
        assert_eq!(texts(dir.path()), expected);
    }
}

#[test]
#[cfg(feature = "web")]
fn negotiated_attribution_survives_independent_archive_progress() {
    use std::net::{SocketAddr, TcpListener, TcpStream};

    fn get(addr: SocketAddr, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        (
            headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
            body.into(),
        )
    }

    fn attach(listener: &UnixListener, high: u64, after: &str) -> (Guest, Value) {
        let mut guest = Guest::negotiate(listener, "reused-pid", 1, high, 0, true);
        let range = guest.until("history.range");
        guest.send(
            json!({"jsonrpc":"2.0","id":"guest-endpoint","method":"endpoint.info","params":{}}),
        );
        let mut line = String::new();
        guest.reader.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], "guest-endpoint");
        assert_eq!(
            response["result"]["capabilities"],
            json!(["history.attribution"])
        );
        guest.reply(&range, guest.range());
        let read = guest.until("history.read");
        assert_eq!(
            read["params"]["after"],
            json!({"journal_id":"reused-pid","position":after})
        );
        (guest, read)
    }

    let warm = TempDir::new().unwrap();
    let cold = TempDir::new().unwrap();
    let warm_listener = UnixListener::bind(warm.path().join("guest.sock")).unwrap();
    let cold_listener = UnixListener::bind(cold.path().join("guest.sock")).unwrap();
    let mut a_log = record("reused-pid", 1, "before PID reuse");
    a_log["event"]["record"]["process"] = json!({"pid":42});
    a_log["event"]["record"]["source_time"] = json!("2026-11-10T08:00:00.1234567Z");
    let mut b_log = record("reused-pid", 2, "after PID reuse");
    b_log["event"]["record"]["process"] = json!({"pid":42,"start_time_filetime":"500"});
    b_log["event"]["record"]["ancestors"] = json!([{"pid":11,"start_time_filetime":"110"}]);
    let mut revision = record("reused-pid", 3, "");
    revision["event"] = json!({"type":"attribution_revision","target":a_log["cursor"],
        "attribution":{"process":{"pid":42,"start_time_filetime":"200","exe_path":"A.exe"},
                       "ancestors":[{"pid":10,"start_time_filetime":"100"}]}});

    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let original_id = {
        let _service = Service::launch(warm.path(), 1048576, Some(addr));
        let (mut guest, read) = attach(&warm_listener, 1, "0");
        guest.page(&read, json!([a_log]), json!([]), 1);
        guest.ack("1"); // Interrupt after the local commit, before the ACK reply.
        let (_, rows) = get(addr, "/api/events?component=dxvk");
        let row: Value = serde_json::from_str::<Value>(&rows).unwrap()["events"][0].clone();
        assert_eq!(row["message"]["process"]["pid"], 42);
        assert!(row["message"]["process"]["start_time_filetime"].is_null());
        row["id"].as_str().unwrap().to_owned()
    };
    {
        let _service = Service::start(cold.path());
        let (mut guest, read) = attach(&cold_listener, 3, "0");
        guest.page(&read, json!([a_log, b_log, revision]), json!([]), 3);
        guest.ack("3");
        assert_eq!(texts(cold.path()), ["before PID reuse", "after PID reuse"]);
    }
    let _service = Service::launch(warm.path(), 1048576, Some(addr));
    let (mut guest, read) = attach(&warm_listener, 4, "1");
    guest.page(
        &read,
        json!([b_log, revision]),
        json!([{"first":"4","last":"4","reason":"retention_expired"}]),
        4,
    );
    let ack = guest.ack("4");
    guest.reply(&ack, ack["params"].clone());
    // A retransmitted response is ignored by the real sync connection.
    guest.page(
        &read,
        json!([b_log, revision]),
        json!([{"first":"4","last":"4","reason":"retention_expired"}]),
        4,
    );

    let rows = |path: &str| {
        let (status, body) = get(addr, path);
        assert_eq!(status, 200);
        serde_json::from_str::<Value>(&body).unwrap()["events"]
            .as_array()
            .unwrap()
            .clone()
    };
    let all = rows("/api/events");
    assert_eq!(
        all.iter()
            .filter_map(|row| row["message"]["text"].as_str())
            .collect::<Vec<_>>(),
        ["before PID reuse", "after PID reuse"]
    );
    assert_eq!(
        all.iter()
            .filter(|row| row["message"]["type"] == "gap")
            .map(|row| row["message"]["first_position"].clone())
            .collect::<Vec<_>>(),
        [json!("4")]
    );
    assert!(
        all.iter()
            .all(|row| row["message"]["type"] != "attribution_revision")
    );
    let a = &all.iter().find(|row| row["id"] == original_id).unwrap();
    for (program, expected) in [("A.exe", Some(*a)), ("B.exe", None)] {
        let (status, body) = get(addr, &format!("/api/events?after={original_id}&program={program}"));
        assert_eq!(status, 200);
        let page: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(page["changes"], json!([{"id":original_id,"event":expected}]));
        let (_, next) = get(addr, &format!("/api/events?after={}",page["next_id"].as_str().unwrap()));
        assert_eq!(serde_json::from_str::<Value>(&next).unwrap()["changes"], json!([]));
    }
    assert_eq!(a["message"]["process"]["start_time_filetime"], "200");
    assert_eq!(a["message"]["ancestors"][0]["start_time_filetime"], "100");
    assert_eq!(get(addr, &format!("/api/events/{original_id}")).0, 200);
    for (query, expected) in [
        ("pid=42&start_time_filetime=200", vec!["before PID reuse"]),
        ("pid=42&start_time_filetime=500", vec!["after PID reuse"]),
        (
            "pid=10&start_time_filetime=100&include_descendants=true",
            vec!["before PID reuse"],
        ),
        (
            "pid=11&start_time_filetime=110&include_descendants=true",
            vec!["after PID reuse"],
        ),
        ("pid=10&start_time_filetime=100", vec![]),
    ] {
        let hits = rows(&format!("/api/events?{query}"));
        assert_eq!(
            hits.iter()
                .map(|row| row["message"]["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
    }

    let messages = |body: &str| {
        body.lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["message"].clone())
            .collect::<Vec<_>>()
    };
    let (status, body) = get(
        addr,
        "/api/export?pid=10&start_time_filetime=100&include_descendants=true",
    );
    assert_eq!(status, 200);
    let raw = messages(&body);
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0]["event"]["record"]["process"]["pid"], 42);
    assert!(raw[0]["event"]["record"]["process"]["start_time_filetime"].is_null());
    assert_eq!(
        raw[0]["event"]["record"]["source_time"],
        "2026-11-10T08:00:00.1234567Z"
    );
    assert_eq!(raw[0]["event"]["record"]["text"], "before PID reuse");
    let (_, body) = get(addr, "/api/export");
    let revisions: Vec<_> = messages(&body)
        .into_iter()
        .filter(|row| row["event"]["type"] == "attribution_revision")
        .collect();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0]["cursor"]["position"], "3");
    assert_eq!(revisions[0]["event"]["target"]["position"], "1");
    assert_eq!(
        revisions[0]["event"]["attribution"]["process"]["start_time_filetime"],
        "200"
    );
}

#[test]
#[ignore]
fn injected_service() {
    struct UnixConnector(PathBuf);
    struct Chunked(UnixStream);
    impl Read for Chunked {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let length = bytes.len().min(4096);
            self.0.read(&mut bytes[..length])
        }
    }
    impl Write for Chunked {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.write(&bytes[..bytes.len().min(31)])
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }
    impl venera_debug_host::transport::Connector for UnixConnector {
        fn connect(
            &mut self,
            _: Duration,
        ) -> std::io::Result<Box<dyn venera_debug_host::transport::Stream>> {
            let stream = UnixStream::connect(&self.0)?;
            stream.set_nonblocking(true)?;
            Ok(Box::new(Chunked(stream)))
        }
    }
    let dir = PathBuf::from(std::env::var_os("VENERA_HISTORY_TEST_DIR").unwrap());
    let web = std::env::var("VENERA_HISTORY_TEST_WEB")
        .ok()
        .and_then(|addr| addr.parse().ok());
    if let Err(error) = venera_debug_host::serve(
        UnixConnector(dir.join("guest.sock")),
        dir.join("history.db"),
        std::env::var("VENERA_HISTORY_TEST_BUDGET")
            .unwrap()
            .parse::<u64>()
            .unwrap(),
        dir.join("control.sock"),
        Some(dir),
        web.unwrap_or_else(|| "127.0.0.1:8787".parse().unwrap()),
        web.is_none(),
    ) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
