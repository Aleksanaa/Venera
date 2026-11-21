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
#[path = "support/history.rs"]
mod history;

const BIN: &str = env!("CARGO_BIN_EXE_venera-debug-host");

#[test]
fn help_lists_service_and_history_commands() {
    let output = Command::new(BIN).arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    let commands: Vec<_> = help
        .lines()
        .filter_map(|line| {
            if line.starts_with("  ") {
                line.split_whitespace().next()
            } else {
                None
            }
        })
        .collect();
    assert!(commands.contains(&"serve"));
    assert!(commands.contains(&"export"));
    assert!(!commands.contains(&"receive"));
    assert!(!commands.contains(&"web"));
}

#[test]
fn serve_cli_validates_guest_addresses_before_initialization() {
    let dir = TempDir::new().unwrap();
    for (cid, port) in [
        ("0", "49152"),
        ("1", "49152"),
        ("2", "49152"),
        ("4294967295", "49152"),
        ("42", "0"),
        ("42", "4294967295"),
        ("guest", "49152"),
    ] {
        let output = Command::new(BIN)
            .args(["serve", "--cid", cid, "--port", port, "--database"])
            .arg(dir.path().join("history.db"))
            .args(["--max-bytes", "1048576", "--control"])
            .arg(dir.path().join("control.sock"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("invalid value"));
    }
    assert!(!dir.path().join("history.db").exists());
    let output = Command::new(BIN)
        .args(["serve", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--cid <CID>"), "{help}");
    assert!(help.contains("--port <PORT>"), "{help}");
    assert!(help.contains("[default: 49152]"), "{help}");
}

#[test]
fn serve_cli_reports_minimum_archive_capacity() {
    let dir = TempDir::new().unwrap();
    let output = Command::new(BIN)
        .args(["serve", "--cid", "42", "--database"])
        .arg(dir.path().join("history.db"))
        .args(["--max-bytes", "73783", "--control"])
        .arg(dir.path().join("control.sock"))
        .arg("--no-web")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("81984"), "{error}");
}

#[cfg(not(feature = "web"))]
#[test]
fn dashboard_request_reports_build_requirement() {
    let dir = TempDir::new().unwrap();
    let output = Command::new(BIN)
        .args(["serve", "--cid", "42"])
        .arg("--database")
        .arg(dir.path().join("history.db"))
        .args(["--max-bytes", "1048576", "--control"])
        .arg(dir.path().join("control.sock"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Build with the web feature to serve the dashboard, or select --no-web\n"
    );
    assert!(!dir.path().join("control.sock").exists());
    assert!(!dir.path().join("history.db").exists());
}

#[cfg(feature = "web")]
fn serve_command(dir: &Path, addr: std::net::SocketAddr) -> Command {
    let mut command = mock_service(dir, "1048576");
    command
        .env("VENERA_TEST_WEB", addr.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

#[cfg(feature = "web")]
fn http(addr: std::net::SocketAddr, path: &str) -> Value {
    use std::net::TcpStream;
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    assert_eq!(headers.split_whitespace().nth(1), Some("200"), "{response}");
    serde_json::from_str(body).unwrap()
}

#[cfg(feature = "web")]
#[test]
fn combined_service_captures_controls_reconnects_and_restarts() {
    use std::net::TcpListener;
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = tcp.local_addr().unwrap();
    drop(tcp);
    let mut service = Receiver(serve_command(dir.path(), addr).spawn().unwrap());
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(initial["id"].as_str().unwrap(), status()),
    );
    let literal = r#"{ "jsonrpc":"2.0", "method":"log", "params":{"stream_id":"once","seq":"1","observed_at":"2026-11-10T08:00:01Z","source":"dbwin","component":"dxvk","level":"trace","raw_level":"trace","text":"combined 🙂","counter":18446744073709551615} }"#;
    history::capture(&guest, &serde_json::from_str(literal).unwrap());
    writeln!(guest, "{literal}").unwrap();
    wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["text"] == "combined 🙂")
    });
    assert_eq!(http(addr, "/api/health"), json!({"status":"ok"}));
    let history = http(addr, "/api/events?q=combined");
    assert_eq!(history["events"].as_array().unwrap().len(), 1);
    let raw: Value =
        serde_json::from_str(history["events"][0]["raw_message"].as_str().unwrap()).unwrap();
    assert_eq!(raw["event"]["record"]["text"], "combined 🙂");
    assert_eq!(http(addr, "/api/connection")["ready"], true);
    let web = std::thread::spawn(move || http(addr, "/api/system-info"));
    let request = read_request(&mut reader, "system.info.get");
    let report = json!({"collected_at":"2026-11-11T00:00:00Z","session_id":1,"sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Venera","bad":false,"warning":""}]}]});
    send(&mut guest, result(request["id"].as_str().unwrap(), report));
    assert_eq!(
        web.join().unwrap(),
        json!({"major":1,"type":"system.info.result","request_id":"web","ok":true,"collected_at":"2026-11-11T00:00:00Z","session_id":1,"sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Venera","bad":false,"warning":""}]}]})
    );
    let web = std::thread::spawn(move || http(addr, "/api/system-info"));
    let request = read_request(&mut reader, "system.info.get");
    send(
        &mut guest,
        json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32001,"message":"System report collection timed out","data":{"retry_after_ms":1000}}}),
    );
    assert_eq!(
        web.join().unwrap(),
        json!({"major":1,"type":"system.info.result","request_id":"web","ok":false,"error":{"code":-32001,"message":"System report collection timed out","data":{"retry_after_ms":1000}}})
    );
    let cli = Command::new(BIN)
        .args(["status", "--control"])
        .arg(dir.path().join("control.sock"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let request = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(request["id"].as_str().unwrap(), status()),
    );
    let output = cli.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        result("cli", status())
    );
    drop(listener);
    std::fs::remove_file(dir.path().join("guest.sock")).unwrap();
    drop(reader);
    drop(guest);
    wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["connected"] == false)
    });
    assert_eq!(http(addr, "/api/connection")["connected"], false);
    assert_eq!(
        http(addr, "/api/events?q=combined")["events"],
        history["events"]
    );
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    bootstrap(&mut guest, &mut reader);
    let initial = read_request(&mut reader, "status.get");
    send(
        &mut guest,
        result(initial["id"].as_str().unwrap(), status()),
    );
    assert_eq!(http(addr, "/api/connection")["ready"], true);
    // Keep a guest request in flight while the service stops.
    let pending_http = std::thread::spawn(move || {
        use std::net::TcpStream;
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "GET /api/system-info HTTP/1.0\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        body
    });
    read_request(&mut reader, "system.info.get");
    let started = Instant::now();
    assert!(Command::new("kill")
        .args(["-TERM", &service.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = service.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "service stop timed out");
        sleep(Duration::from_millis(10));
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(pending_http.join().unwrap().contains("503"));
    assert!(!dir.path().join("control.sock").exists());
    drop(reader);
    drop(guest);
    let _restarted = Receiver(serve_command(dir.path(), addr).spawn().unwrap());
    let _guest = accept(&listener);
    assert_eq!(
        http(addr, "/api/events?q=combined")["events"],
        history["events"]
    );
    let db = Connection::open(dir.path().join("history.db")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM events WHERE json_extract(message,'$.text')='combined 🙂'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[cfg(feature = "web")]
#[test]
fn startup_failures_release_owned_resources() {
    use std::net::TcpListener;
    for failure in ["database", "control", "tcp"] {
        let dir = TempDir::new().unwrap();
        let guest = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
        guest.set_nonblocking(true).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = tcp.local_addr().unwrap();
        let occupied = if failure == "tcp" {
            Some(tcp)
        } else {
            drop(tcp);
            None
        };
        if failure == "database" {
            std::fs::write(dir.path().join("history.db"), "broken database").unwrap();
        }
        if failure == "control" {
            std::fs::write(dir.path().join("control.sock"), "existing file").unwrap();
        }
        let started = Instant::now();
        let mut service = Receiver(serve_command(dir.path(), addr).spawn().unwrap());
        loop {
            if let Some(status) = service.0.try_wait().unwrap() {
                assert!(!status.success(), "{failure}");
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{failure} startup hung"
            );
            sleep(Duration::from_millis(10));
        }
        assert_eq!(
            guest.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        if failure == "control" {
            assert_eq!(
                std::fs::read_to_string(dir.path().join("control.sock")).unwrap(),
                "existing file"
            );
        } else {
            assert!(!dir.path().join("control.sock").exists());
        }
        drop(occupied);
        let _released = TcpListener::bind(addr).unwrap();
    }
}

#[cfg(feature = "web")]
#[test]
fn collection_failure_remains_visible_while_http_serves_control() {
    use std::net::TcpListener;
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = tcp.local_addr().unwrap();
    drop(tcp);
    let mut service = Receiver(serve_command(dir.path(), addr).spawn().unwrap());
    let mut guest = accept(&listener);
    assert_eq!(http(addr, "/api/health"), json!({"status":"ok"}));
    let db = Connection::open(dir.path().join("history.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_log BEFORE INSERT ON events WHEN json_extract(NEW.message,'$.type')='log' BEGIN SELECT RAISE(ABORT,'collection fixture failure'); END;").unwrap();
    send(&mut guest, log(1, "trigger failure"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let connection = http(addr, "/api/connection");
        if connection["history"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("collection fixture failure"))
        {
            break;
        }
        assert!(Instant::now() < deadline, "archive error was not reported");
        sleep(Duration::from_millis(10));
    }
    assert!(service.0.try_wait().unwrap().is_none());
    assert_eq!(http(addr, "/api/health"), json!({"status":"ok"}));
}

#[test]
fn headless_interrupt_closes_partial_control_and_cleans_socket() {
    let dir = TempDir::new().unwrap();
    let mut service = start(dir.path(), "1048576");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut client = loop {
        if let Ok(client) = UnixStream::connect(dir.path().join("control.sock")) {
            break client;
        }
        assert!(service.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(10));
    };
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    client.write_all(b"{\"jsonrpc\":").unwrap();
    assert!(Command::new("kill")
        .args(["-INT", &service.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    loop {
        if let Some(status) = service.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "interrupt timed out");
        sleep(Duration::from_millis(10));
    }
    let mut response = String::new();
    match client.read_to_string(&mut response) {
        Ok(_) => assert_eq!(response, ""),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
    }
    assert!(!dir.path().join("control.sock").exists());
    let _released = UnixListener::bind(dir.path().join("control.sock")).unwrap();
}

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
    history::capture(stream, &value);
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
        mock_service(dir, budget)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}

fn mock_service(dir: &Path, budget: &str) -> Command {
    history::start(dir);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "injected_service", "--nocapture"])
        .env("VENERA_TEST_DIR", dir)
        .env("VENERA_TEST_BUDGET", budget);
    command
}

#[test]
#[ignore]
fn injected_service() {
    struct UnixConnector {
        path: std::path::PathBuf,
        control_thread: Option<std::thread::ThreadId>,
    }
    struct Chunked(UnixStream);
    impl Read for Chunked {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let len = bytes.len().min(4096);
            self.0.read(&mut bytes[..len])
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
            let current = std::thread::current().id();
            let control = *self.control_thread.get_or_insert(current) == current;
            let path = if control {
                self.path.clone()
            } else {
                self.path.with_file_name("archive.sock")
            };
            let stream = UnixStream::connect(path)?;
            stream.set_nonblocking(true)?;
            Ok(Box::new(Chunked(stream)))
        }
    }
    let dir = std::path::PathBuf::from(std::env::var_os("VENERA_TEST_DIR").unwrap());
    let web = std::env::var("VENERA_TEST_WEB").ok();
    if let Err(error) = venera_debug_host::serve(
        UnixConnector {
            path: dir.join("guest.sock"),
            control_thread: None,
        },
        dir.join("history.db"),
        std::env::var("VENERA_TEST_BUDGET")
            .unwrap()
            .parse::<u64>()
            .unwrap(),
        dir.join("control.sock"),
        Some(dir),
        web.as_deref().unwrap_or("127.0.0.1:8787").parse().unwrap(),
        web.is_none(),
    ) {
        eprintln!("{error}");
        std::process::exit(1);
    }
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
    assert_eq!(connected["result"]["transport"], "VirtIO socket");
    send(&mut guest, log(1, "connection time"));
    wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["seq"] == "1")
    });
    drop(listener);
    std::fs::remove_file(dir.path().join("guest.sock")).unwrap();
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
    assert_eq!(raw["event"]["type"], "log");
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
    send(&mut guest, log(1, &"x".repeat(50000)));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = read_message(&mut control(
            dir.path(),
            "capacity",
            "connection.get",
            json!({}),
        ));
        if response["result"]["history"]["error"] == "storage budget cannot hold this history page"
        {
            assert_eq!(response["result"]["history"]["cursor"]["position"], "0");
            break;
        }
        assert!(Instant::now() < deadline, "expected storage error");
        sleep(Duration::from_millis(10))
    }
    assert!(receiver.0.try_wait().unwrap().is_none());
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
    assert!(receiver.0.wait().unwrap().success());
    assert!(!dir.path().join("control.sock").exists());
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
    assert_eq!(raw["event"]["record"]["seq"], "18446744073709551615");
    assert_eq!(raw["event"]["type"], "log");
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
    history::capture(&s.guest, &serde_json::from_str(&text).unwrap());
    writeln!(s.guest, "{text}").unwrap();
    wait_for(s.dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["text"] == "raw-text")
    });
    let raw: Value = serde_json::from_str(&stored_raw(s.dir.path(), "raw-text")).unwrap();
    assert_eq!(raw["event"]["record"]["text"], "raw-text");
    assert_eq!(raw["event"]["record"]["seq"], "77");
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
        .any(|row| row["message"]["event"]["type"] == "log"
            && row["message"]["event"]["record"]["text"] == "raw-text"));
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
