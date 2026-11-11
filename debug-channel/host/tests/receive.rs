use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_venera-debug-host");

fn discovery() -> Value {
    serde_json::from_str(include_str!("../../protocol/v1/guest.openrpc.json")).unwrap()
}
fn status() -> Value {
    json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}})
}
fn bootstrap(guest: &mut UnixStream, reader: &mut BufReader<UnixStream>) {
    let endpoint = read_request(reader, "endpoint.info");
    let discover = read_request(reader, "rpc.discover");
    send(
        guest,
        result(
            endpoint["id"].as_str().unwrap(),
            json!({"role":"guest","software_version":"test-guest","protocol_version":"1.0.0"}),
        ),
    );
    send(guest, result(discover["id"].as_str().unwrap(), discovery()));
}
fn request(id: &str, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn result(id: &str, value: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":value})
}
fn error(id: &str, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}
fn log(seq: u64, text: &str) -> Value {
    notification(
        "log",
        json!({
            "stream_id":"guest-test","seq":seq.to_string(),"observed_at":"2026-11-10T08:00:01Z",
            "source_time":null,"source":"dbwin","component":"dxvk","level":"trace","raw_level":"trace",
            "session_id":2,"process":null,"tid":null,"text":text
        }),
    )
}
fn gap() -> Value {
    notification(
        "gap",
        json!({"stream_id":"guest-test","reason":"overflow","count":"2","first_seq":"3","last_seq":"4"}),
    )
}
fn status_changed() -> Value {
    notification(
        "status.changed",
        json!({
            "forwarding_enabled":true,"revision":"7",
            "effective_filter":{"components":[],"levels":[],"targets":[]},
            "expires_at":null,"health":{"state":"ok"},"loss":{"count":0}
        }),
    )
}
fn send(stream: &mut UnixStream, value: Value) {
    writeln!(stream, "{value}").unwrap();
}
fn read_message(reader: &mut BufReader<UnixStream>) -> Value {
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if !line.trim().is_empty() {
            return serde_json::from_str(&line).unwrap();
        }
    }
}
fn read_request(reader: &mut BufReader<UnixStream>, method: &str) -> Value {
    let value = read_message(reader);
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["method"], method);
    assert!(value["id"].is_string());
    assert!(value["params"].is_object());
    value
}
struct Receiver(Child);
impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn start(dir: &Path, budget: &str) -> Receiver {
    Receiver(
        Command::new(BIN)
            .args(["receive", "--socket"])
            .arg(dir.join("guest.sock"))
            .args(["--database"])
            .arg(dir.join("history.db"))
            .args(["--max-bytes", budget, "--control"])
            .arg(dir.join("control.sock"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}
fn accept(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(6)))
                    .unwrap();
                return stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                sleep(Duration::from_millis(10))
            }
            Err(e) => panic!("accept: {e}"),
        }
    }
}
fn control(dir: &Path, id: &str, method: &str, params: Value) -> BufReader<UnixStream> {
    let mut stream = UnixStream::connect(dir.join("control.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    send(&mut stream, request(id, method, params));
    BufReader::new(stream)
}
fn exported(dir: &Path) -> Vec<Value> {
    let db = Connection::open(dir.join("history.db")).unwrap();
    let mut statement = db
        .prepare("SELECT message,received_at FROM events ORDER BY id")
        .unwrap();
    statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).unwrap().map(|row|{let (message,received)=row.unwrap();json!({"message":serde_json::from_str::<Value>(&message).unwrap(),"received_at":received})}).collect()
}
fn wait_for(dir: &Path, predicate: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let rows = exported(dir);
        if predicate(&rows) {
            return rows;
        }
        assert!(Instant::now() < deadline, "history: {rows:?}");
        sleep(Duration::from_millis(20))
    }
}
fn stored_raw(dir: &Path, text: &str) -> String {
    let db = Connection::open(dir.join("history.db")).unwrap();
    db.query_row(
        "SELECT raw_message FROM events WHERE message LIKE ?1",
        [format!("%{text}%")],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn system_report_preserves_bad_rows_and_matches_rpc_id() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _receiver = start(dir.path(), "1048576");
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(
            initial["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    let mut client = control(dir.path(), "literal-system", "system.info.get", json!({}));
    let req = read_request(&mut reader, "system.info.get");
    send(
        &mut guest,
        result(
            req["id"].as_str().unwrap(),
            json!({"collected_at":"2026-11-11T00:00:00Z","session_id":1,"sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Unavailable","bad":true,"warning":"检查图形驱动"}]}]}),
        ),
    );
    let response = read_message(&mut client);
    assert_eq!(response["id"], "literal-system");
    assert_eq!(response["result"]["sections"][0]["rows"][0]["bad"], true);
}

#[test]
fn connection_query_tracks_live_receiver_and_disconnect() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _receiver = start(dir.path(), "1048576");
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(
            initial["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    let query = || {
        let mut c = UnixStream::connect(dir.path().join("control.sock")).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        send(&mut c, request("connection", "connection.get", json!({})));
        read_message(&mut BufReader::new(c))
    };
    let connected = query();
    assert_eq!(connected["result"]["connected"], true);
    send(&mut guest, log(1, "connection time"));
    wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["seq"] == "1")
    });
    drop(reader);
    drop(guest);
    wait_for(dir.path(), |rows| {
        rows.iter().any(|r| {
            r["message"]["type"] == "host.connection" && r["message"]["connected"] == false
        })
    });
    assert_eq!(query()["result"]["connected"], false);
}

#[test]
fn concurrent_requests_are_rejected_and_keep_local_correlation() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _receiver = start(dir.path(), "1048576");
    let guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    let mut guest = guest;
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(
            initial["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    let mut first = control(
        dir.path(),
        "first",
        "filter.set",
        json!({"filter":{"components":[],"levels":[],"targets":[]}}),
    );
    let req = read_request(&mut reader, "filter.set");
    let second = control(dir.path(), "second", "filter.reset", json!({}));
    let mut second = second;
    let third = control(dir.path(), "third", "status.get", json!({}));
    let mut third = third;
    assert_eq!(read_message(&mut second)["id"], "second");
    assert_eq!(read_message(&mut third)["id"], "third");
    let _ = req;
    let response = read_message(&mut first);
    assert_eq!(response["id"], "first");
    assert_eq!(response["error"]["data"]["code"], "outcome_unknown");
}

#[test]
fn guest_success_failure_and_disconnect_restore_literal_request_id() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _receiver = start(dir.path(), "1048576");
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(
            initial["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    let mut client = control(
        dir.path(),
        "client-literal-7",
        "filter.set",
        json!({"filter":{"components":[],"levels":[],"targets":[]}}),
    );
    let req = read_request(&mut reader, "filter.set");
    send(
        &mut guest,
        result(
            req["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"2","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    assert_eq!(read_message(&mut client)["id"], "client-literal-7");
    let mut client = control(dir.path(), "client-literal-8", "filter.reset", json!({}));
    let req = read_request(&mut reader, "filter.reset");
    send(
        &mut guest,
        error(req["id"].as_str().unwrap(), -32010, "rejected"),
    );
    let response = read_message(&mut client);
    assert_eq!(response["id"], "client-literal-8");
    assert_eq!(
        response["error"],
        json!({"code":-32010,"message":"rejected"})
    );
    let mut client = control(dir.path(), "client-literal-9", "status.get", json!({}));
    let _ = read_request(&mut reader, "status.get");
    drop(reader);
    drop(guest);
    let response = read_message(&mut client);
    assert_eq!(response["id"], "client-literal-9");
    assert_eq!(response["error"]["data"]["code"], "outcome_unknown");
}

#[test]
fn receive_control_reconnect_and_export_through_cli() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let receiver = start(dir.path(), "1048576");
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(
            initial["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    send(&mut guest, log(1, "中文\n🙂"));
    send(&mut guest, gap());
    send(&mut guest, status_changed());
    let _rows = wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["text"] == "中文\n🙂")
            && rows.iter().any(|r| r["message"]["type"] == "gap")
            && rows.iter().any(|r| r["message"]["type"] == "status")
    });
    let raw: Value = serde_json::from_str(&stored_raw(dir.path(), "中文")).unwrap();
    assert_eq!(raw["method"], "log");
    let child = Command::new(BIN)
        .arg("status")
        .arg("--control")
        .arg(dir.path().join("control.sock"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let req = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(
            req["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"7","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    assert!(child.wait_with_output().unwrap().status.success());
    drop(receiver);
}

#[test]
fn capacity_evicts_oldest_and_reclaims_physical_space() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _receiver = start(dir.path(), "131072");
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(
            initial["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    for seq in 0..100 {
        send(&mut guest, log(seq, &"x".repeat(3000)));
    }
    send(&mut guest, log(100, "newest"));
    let rows = wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["text"] == "newest")
    });
    let sequences: Vec<u64> = rows
        .iter()
        .filter_map(|r| r["message"]["seq"].as_str()?.parse().ok())
        .collect();
    assert_eq!(sequences.last(), Some(&100));
    assert!(sequences[0] > 0);
    assert!(sequences.windows(2).all(|p| p[1] == p[0] + 1));
}

#[test]
fn insufficient_record_capacity_reports_storage_error() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let mut receiver = start(dir.path(), "65536");
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(
            initial["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    send(&mut guest, log(1, &"x".repeat(50000)));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = receiver.0.try_wait().unwrap() {
            assert_eq!(status.code(), Some(1));
            break;
        }
        assert!(Instant::now() < deadline, "expected storage error");
        sleep(Duration::from_millis(10))
    }
    let mut error_text = String::new();
    receiver
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut error_text)
        .unwrap();
    assert_eq!(error_text, "storage budget cannot hold this record\n");
}

#[test]
fn service_termination_and_restart_preserve_committed_history() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let mut receiver = start(dir.path(), "131072");
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(
            initial["id"].as_str().unwrap(),
            json!({"forwarding_enabled":true,"revision":"1","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
        ),
    );
    send(&mut guest, log(1, "before service stop"));
    wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["seq"] == "1")
    });
    assert!(Command::new("kill")
        .args(["-TERM", &receiver.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let _ = receiver.0.wait();
    drop(guest);
    let _receiver = start(dir.path(), "131072");
    let mut guest = accept(&listener);
    send(&mut guest, log(2, "after service restart"));
    let rows = wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["seq"] == "2")
    });
    assert_eq!(
        rows.iter()
            .filter_map(|r| r["message"]["text"].as_str())
            .collect::<Vec<_>>(),
        vec!["before service stop", "after service restart"]
    );
}

struct Session {
    dir: TempDir,
    _receiver: Receiver,
    guest: UnixStream,
    reader: BufReader<UnixStream>,
    endpoint: Value,
    discover: Value,
    listener: UnixListener,
}
impl Session {
    fn connecting() -> Self {
        let dir = TempDir::new().unwrap();
        let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
        let receiver = start(dir.path(), "1048576");
        let guest = accept(&listener);
        let mut reader = BufReader::new(guest.try_clone().unwrap());
        let mut lf = String::new();
        reader.read_line(&mut lf).unwrap();
        assert_eq!(lf, "\n");
        let endpoint = read_request(&mut reader, "endpoint.info");
        let discover = read_request(&mut reader, "rpc.discover");
        Self {
            dir,
            _receiver: receiver,
            guest,
            reader,
            endpoint,
            discover,
            listener,
        }
    }
    fn metadata(&mut self, endpoint: Value, doc: Value) {
        send(
            &mut self.guest,
            json!([
                result(self.discover["id"].as_str().unwrap(), doc),
                result(self.endpoint["id"].as_str().unwrap(), endpoint)
            ]),
        );
    }
    fn ready() -> Self {
        let mut session = Self::connecting();
        session.metadata(
            json!({"role":"guest","software_version":"test-guest","protocol_version":"1.0.0"}),
            discovery(),
        );
        let request = read_request(&mut session.reader, "status.get");
        send(
            &mut session.guest,
            result(request["id"].as_str().unwrap(), status()),
        );
        session
    }
    fn query(&self, method: &str) -> Value {
        read_message(&mut control(self.dir.path(), "query", method, json!({})))
    }
    fn barrier(&mut self) {
        send(
            &mut self.guest,
            request("barrier", "endpoint.info", json!({})),
        );
        let reply = read_message(&mut self.reader);
        assert_eq!(reply["id"], "barrier");
        assert_eq!(reply["result"]["role"], "host");
        assert_eq!(reply["result"]["protocol_version"], "1.0.0");
    }
}

#[test]
fn negotiation_blocks_calls_immediately_and_preserves_bootstrap_error() {
    let mut s = Session::connecting();
    let before = Instant::now();
    let reply = s.query("system.info.get");
    assert_eq!(reply["error"]["message"], "not_ready");
    assert!(before.elapsed() < Duration::from_secs(1));
    s.barrier();
    let failure = json!({"code":-32090,"message":"negotiation refused","data":{"retry":false}});
    send(
        &mut s.guest,
        json!({"jsonrpc":"2.0","id":s.endpoint["id"],"error":failure}),
    );
    s.barrier();
    assert_eq!(s.query("status.get")["error"], failure);
}

#[test]
fn negotiation_timeout_is_visible_to_immediate_control_queries() {
    let mut s = Session::connecting();
    sleep(Duration::from_millis(5200));
    let before = Instant::now();
    assert_eq!(
        s.query("system.info.get")["error"]["message"],
        "Handshake timed out"
    );
    assert!(before.elapsed() < Duration::from_secs(1));
    s.barrier();
}

#[test]
fn incompatible_endpoint_and_discovery_prevent_business_calls() {
    for (role, version, doc_version) in [
        ("guest", "2.0.0", "2.0.0"),
        ("guest", "1.0.0", "1.1.0"),
        ("host", "1.0.0", "1.0.0"),
    ] {
        let mut s = Session::connecting();
        let mut doc = discovery();
        doc["info"]["version"] = json!(doc_version);
        s.metadata(
            json!({"role":role,"software_version":"other","protocol_version":version}),
            doc,
        );
        s.barrier();
        let before = Instant::now();
        assert_eq!(
            s.query("system.info.get")["error"]["message"],
            "incompatible_protocol"
        );
        assert!(before.elapsed() < Duration::from_secs(1));
        s.barrier();
    }
}

#[test]
fn discovery_gates_supported_methods_and_exposes_peer_metadata() {
    let mut s = Session::connecting();
    let mut doc = discovery();
    doc["methods"]
        .as_array_mut()
        .unwrap()
        .retain(|m| m["name"] != "system.info.get");
    s.metadata(
        json!({"role":"guest","software_version":"test-guest","protocol_version":"1.0.0"}),
        doc,
    );
    let initial = read_request(&mut s.reader, "status.get");
    send(
        &mut s.guest,
        result(initial["id"].as_str().unwrap(), status()),
    );
    let reply = s.query("system.info.get");
    assert_eq!(reply["error"]["message"], "unsupported");
    s.barrier();
    let connection = s.query("connection.get");
    assert_eq!(connection["result"]["protocol_version"], "1.0.0");
    assert_eq!(connection["result"]["software_version"], "test-guest");
    assert_eq!(connection["result"]["role"], "guest");
    assert_eq!(connection["result"]["ready"], true);
    let methods = connection["result"]["discovered_methods"]
        .as_array()
        .unwrap();
    assert!(methods.contains(&json!("filter.set")));
    assert!(!methods.contains(&json!("system.info.get")));
}

#[test]
fn reconnect_starts_fresh_metadata_and_capabilities() {
    let mut s = Session::ready();
    assert_eq!(
        s.query("connection.get")["result"]["software_version"],
        "test-guest"
    );
    s.guest.shutdown(std::net::Shutdown::Both).unwrap();
    wait_for(s.dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["connected"] == false)
    });
    s.guest = accept(&s.listener);
    s.reader = BufReader::new(s.guest.try_clone().unwrap());
    s.endpoint = read_request(&mut s.reader, "endpoint.info");
    s.discover = read_request(&mut s.reader, "rpc.discover");
    let reply = s.query("connection.get");
    assert_eq!(reply["result"]["protocol_version"], Value::Null);
    assert_eq!(reply["result"]["discovered_methods"], Value::Null);
    assert_eq!(s.query("status.get")["error"]["message"], "not_ready");
    let mut doc = discovery();
    doc["methods"]
        .as_array_mut()
        .unwrap()
        .retain(|m| m["name"] != "filter.reset");
    s.metadata(
        json!({"role":"guest","software_version":"new-guest","protocol_version":"1.0.0"}),
        doc,
    );
    let initial = read_request(&mut s.reader, "status.get");
    send(
        &mut s.guest,
        result(initial["id"].as_str().unwrap(), status()),
    );
    assert_eq!(s.query("filter.reset")["error"]["message"], "unsupported");
    assert_eq!(
        s.query("connection.get")["result"]["software_version"],
        "new-guest"
    );
    s.barrier();
}

#[test]
fn guest_batches_aggregate_calls_and_keep_numeric_ids_and_notifications() {
    let mut s = Session::ready();
    send(
        &mut s.guest,
        json!([
            {"jsonrpc":"2.0","id":17,"method":"endpoint.info","params":{}},
            log(u64::MAX,"batch-a"),
            {"jsonrpc":"2.0","id":18,"method":"rpc.discover","params":{}},
            gap()
        ]),
    );
    let reply = read_message(&mut s.reader);
    assert_eq!(reply.as_array().unwrap().len(), 2);
    assert_eq!(reply[0]["id"], 17);
    assert_eq!(reply[0]["result"]["role"], "host");
    assert_eq!(reply[1]["id"], 18);
    assert_eq!(reply[1]["result"]["info"]["title"], "Venera host");
    wait_for(s.dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["text"] == "batch-a")
    });
    let raw: Value = serde_json::from_str(&stored_raw(s.dir.path(), "batch-a")).unwrap();
    assert_eq!(raw["params"]["seq"], "18446744073709551615");
    assert_eq!(raw["method"], "log");
    send(&mut s.guest, json!([log(2, "batch-b"), status_changed()]));
    s.barrier();
    wait_for(s.dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["text"] == "batch-b")
    });
}

#[test]
fn mixed_batch_response_matches_string_id_and_retains_guest_error_data() {
    let mut s = Session::ready();
    let mut client = control(s.dir.path(), "local", "filter.reset", json!({}));
    let req = read_request(&mut s.reader, "filter.reset");
    let failure = json!({"code":-32012,"message":"filter rejected","data":{"revision":"18446744073709551615","reason":"rule"}});
    send(
        &mut s.guest,
        json!([
            result("unrelated",status()),
            {"jsonrpc":"2.0","id":55,"result":{}},
            {"jsonrpc":"2.0","id":req["id"],"error":failure},
            request("guest-doc","rpc.discover",json!({})),
            log(5,"mixed")
        ]),
    );
    assert_eq!(
        read_message(&mut client),
        json!({"jsonrpc":"2.0","id":"local","error":failure})
    );
    let reply = read_message(&mut s.reader);
    assert_eq!(reply.as_array().unwrap().len(), 1);
    assert_eq!(reply[0]["id"], "guest-doc");
    wait_for(s.dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["text"] == "mixed")
    });
}

#[test]
fn partial_busy_request_waits_for_complete_packet_and_echoes_numeric_id() {
    let mut s = Session::ready();
    let mut first = control(s.dir.path(), "first", "status.get", json!({}));
    let req = read_request(&mut s.reader, "status.get");
    let mut second = UnixStream::connect(s.dir.path().join("control.sock")).unwrap();
    second
        .set_read_timeout(Some(Duration::from_millis(150)))
        .unwrap();
    second
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":42,")
        .unwrap();
    let mut bytes = [0; 1];
    let error = second.read(&mut bytes).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    second
        .write_all(b"\"method\":\"filter.reset\",\"params\":{}}\n")
        .unwrap();
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let reply = read_message(&mut BufReader::new(second));
    assert_eq!(reply["id"], 42);
    assert_eq!(reply["error"]["data"]["code"], "busy");
    send(&mut s.guest, result(req["id"].as_str().unwrap(), status()));
    assert_eq!(read_message(&mut first)["result"]["revision"], "1");
}

#[test]
fn invalid_results_and_malformed_control_report_protocol_errors() {
    let mut s = Session::ready();
    for method in [
        "status.get",
        "filter.set",
        "filter.reset",
        "system.info.get",
        "rpc.discover",
    ] {
        let params = if method == "filter.set" {
            json!({"filter":{"components":[],"levels":[],"targets":[]}})
        } else {
            json!({})
        };
        let mut client = control(s.dir.path(), "invalid", method, params);
        let req = read_request(&mut s.reader, method);
        send(&mut s.guest, result(req["id"].as_str().unwrap(), json!({})));
        let reply = read_message(&mut client);
        assert_eq!(reply["id"], "invalid");
        assert_eq!(reply["error"]["code"], -32004);
    }
    let mut client = UnixStream::connect(s.dir.path().join("control.sock")).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client.write_all(b"bad\n").unwrap();
    assert_eq!(
        read_message(&mut BufReader::new(client)),
        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
    );
    let mut client = control(s.dir.path(), "bad-params", "filter.set", json!({}));
    let reply = read_message(&mut client);
    assert_eq!(reply["id"], "bad-params");
    assert_eq!(reply["error"]["code"], -32602);
    s.barrier();
}

#[test]
fn numeric_control_correlation_and_invalid_response_recovery() {
    let mut s = Session::ready();
    let mut client = UnixStream::connect(s.dir.path().join("control.sock")).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    send(
        &mut client,
        json!({"jsonrpc":"2.0","id":901,"method":"status.get","params":{}}),
    );
    let req = read_request(&mut s.reader, "status.get");
    send(
        &mut s.guest,
        json!({"jsonrpc":"2.0","id":req["id"],"result":status(),"error":{"code":-32000,"message":"both"}}),
    );
    s.barrier();
    send(&mut s.guest, result(req["id"].as_str().unwrap(), status()));
    assert_eq!(
        read_message(&mut BufReader::new(client)),
        json!({"jsonrpc":"2.0","id":901,"result":status()})
    );
}

#[test]
fn discover_cli_returns_guest_artifact_and_single_notification_keeps_raw_text() {
    let mut s = Session::ready();
    let child = Command::new(BIN)
        .arg("discover")
        .arg("--control")
        .arg(s.dir.path().join("control.sock"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let req = read_request(&mut s.reader, "rpc.discover");
    send(
        &mut s.guest,
        result(req["id"].as_str().unwrap(), discovery()),
    );
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["result"], discovery());
    let text = format!("  {}  ", log(77, "raw-text"));
    writeln!(s.guest, "{text}").unwrap();
    wait_for(s.dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["text"] == "raw-text")
    });
    assert_eq!(stored_raw(s.dir.path(), "raw-text"), text);
    let export = Command::new(BIN)
        .arg("export")
        .arg("--database")
        .arg(s.dir.path().join("history.db"))
        .output()
        .unwrap();
    assert!(export.status.success());
    let rows: Vec<Value> = String::from_utf8(export.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(rows
        .iter()
        .any(|row| row["message"]["method"] == "log"
            && row["message"]["params"]["text"] == "raw-text"));
}

#[test]
fn filter_cli_transmits_typed_params_and_returns_effective_status() {
    let mut s = Session::ready();
    let child = Command::new(BIN)
        .args(["filter-set", "--control"])
        .arg(s.dir.path().join("control.sock"))
        .args([
            "--filter",
            r#"{"components":["dxvk"],"levels":["trace"],"targets":[]}"#,
            "--duration-ms",
            "60000",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let req = read_request(&mut s.reader, "filter.set");
    assert_eq!(
        req["params"],
        json!({"filter":{"components":["dxvk"],"levels":["trace"],"targets":[]},"duration_ms":"60000"})
    );
    send(&mut s.guest, result(req["id"].as_str().unwrap(), status()));
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        result("cli", status())
    );
    let local = s.query("endpoint.info");
    assert_eq!(local["result"]["role"], "host");
    assert_eq!(local["result"]["protocol_version"], "1.0.0");
}

#[test]
fn guest_parse_errors_return_null_id_and_frame_recovers_after_overflow() {
    let mut s = Session::ready();
    s.guest.write_all(b"bad\n").unwrap();
    assert_eq!(
        read_message(&mut s.reader),
        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
    );
    s.guest.write_all(&vec![b'x'; 1024 * 1024 + 1]).unwrap();
    s.guest.write_all(b"\n").unwrap();
    let reply = read_message(&mut s.reader);
    assert_eq!(reply["id"], Value::Null);
    assert_eq!(reply["error"]["code"], -32700);
    s.barrier();
}
