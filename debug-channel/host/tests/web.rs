#![cfg(feature = "web")]

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    io::{ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

struct Server {
    child: Child,
    addr: SocketAddr,
    dir: tempfile::TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Server {
    fn start(populate: bool) -> Self {
        Self::with_control(populate, false)
    }
    fn with_control(populate: bool, control: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<h1>History</h1>").unwrap();
        std::fs::write(dir.path().join("app.js"), "window.historyReady=true;").unwrap();
        if populate {
            let db = Connection::open(dir.path().join("history.db")).unwrap();
            db.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY,received_at TEXT NOT NULL,message TEXT NOT NULL)").unwrap();
            for (id, received, raw) in [
                (
                    "9007199254740993",
                    "2026-11-10T08:00:00Z",
                    r#"{"component":"UMD","level":"INFO","process":{"pid":42,"exe_path":"C:\\Game.exe"},"text":"first"}"#,
                ),
                (
                    "9007199254740994",
                    "2026-11-10T09:00:01+01:00",
                    r#"{"component":"umd","level":"ERROR","process":{"pid":42,"exe_path":"C:\\Game.exe"},"text":"中文 Hello","seq":18446744073709551615}"#,
                ),
                (
                    "9007199254740995",
                    "2026-11-10T08:00:02Z",
                    r#"{"component":"KMD","level":"INFO","process":{"pid":142,"exe_path":"C:\\Other.exe"},"text":"latest"}"#,
                ),
            ] {
                db.execute(
                    "INSERT INTO events VALUES(?1,?2,?3)",
                    params![id, received, raw],
                )
                .unwrap();
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut command = Command::new(env!("CARGO_BIN_EXE_venera-debug-host"));
        command
            .args(["web", "--database"])
            .arg(dir.path().join("history.db"))
            .arg("--assets")
            .arg(dir.path())
            .args(["--listen", &addr.to_string()])
            .stdout(Stdio::null());
        if control {
            command
                .arg("--control")
                .arg(dir.path().join("control.sock"));
        }
        let child = command.spawn().unwrap();
        let mut server = Self { child, addr, dir };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(server.child.try_wait().unwrap().is_none(), "server stopped");
            let ready = || -> std::io::Result<bool> {
                let mut stream = TcpStream::connect(addr)?;
                stream.set_read_timeout(Some(Duration::from_millis(100)))?;
                stream.set_write_timeout(Some(Duration::from_millis(100)))?;
                write!(stream, "GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
                let mut response = String::new();
                stream.read_to_string(&mut response)?;
                Ok(response.split_once("\r\n\r\n").is_some_and(|(headers, body)| {
                    headers.split_whitespace().nth(1) == Some("200")
                        && serde_json::from_str::<Value>(body).ok() == Some(json!({"status":"ok"}))
                }))
            };
            if ready().unwrap_or(false) {
                break;
            }
            assert!(Instant::now() < deadline, "server startup timed out");
            sleep(Duration::from_millis(20));
        }
        server
    }
    fn get(&self, path: &str) -> (u16, String) {
        self.request("GET", path)
    }
    fn request(&self, method: &str, path: &str) -> (u16, String) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match TcpStream::connect(self.addr) {
                Ok(stream) => break stream,
                Err(error)
                    if error.kind() == ErrorKind::ConnectionRefused && Instant::now() < deadline =>
                {
                    sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("HTTP connection to {} failed: {error}", self.addr),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.0\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        (
            headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
            body.to_owned(),
        )
    }
    fn json(&self, path: &str) -> Value {
        let (status, body) = self.get(path);
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }
    fn ids(&self, query: &str) -> Vec<String> {
        self.json(&format!("/api/events{query}"))["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn live_endpoints_return_configuration_error_and_forward_control_responses() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
    };
    let server = Server::start(false);
    for endpoint in ["/api/system-info", "/api/connection"] {
        let (status, body) = server.get(endpoint);
        assert_eq!(status, 503);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"error":"Live connection is not configured"})
        );
    }
    let server = Server::with_control(false, true);
    let listener = UnixListener::bind(server.dir.path().join("control.sock")).unwrap();
    let system = json!({"collected_at":"2026-11-11T00:00:00Z","session_id":1,"sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Unavailable","bad":true,"warning":"请检查图形驱动。\n重新启动 Windows 后重试。"}]}
    ]});
    let connection = json!({"transport":"VirtIO Serial","channel":"org.venera.debug.0","socket_path":"/run/guest.sock","connected":false,"last_received_at":null});
    let responses = [system.clone(), connection.clone()];
    let worker = std::thread::spawn(move || {
        for (kind, response) in ["system.info.get", "connection.get"]
            .into_iter()
            .zip(responses)
        {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["jsonrpc"], "2.0");
            assert_eq!(request["method"], kind);
            assert_eq!(request["id"], "web");
            writeln!(
                stream,
                "{}",
                json!({"jsonrpc":"2.0","id":"web","result":response})
            )
            .unwrap();
        }
    });
    assert_eq!(
        server.json("/api/system-info"),
        json!({"major":1,"type":"system.info.result","request_id":"web","ok":true,
            "collected_at":"2026-11-11T00:00:00Z","session_id":1,
            "sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Unavailable","bad":true,"warning":"请检查图形驱动。\n重新启动 Windows 后重试。"}]}]})
    );
    assert_eq!(server.json("/api/connection"), connection);
    worker.join().unwrap();
}

#[test]
fn system_report_projects_rpc_errors_and_connection_keeps_rpc_errors() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
    };
    let server = Server::with_control(false, true);
    let listener = UnixListener::bind(server.dir.path().join("control.sock")).unwrap();
    let worker = std::thread::spawn(move || {
        for method in ["system.info.get", "connection.get"] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], method);
            writeln!(stream, "{}", json!({"jsonrpc":"2.0","id":"web","error":{
                "code":-32001,"message":"System report collection timed out",
                "data":{"retry_after_ms":1000}
            }})).unwrap();
        }
    });
    assert_eq!(
        server.json("/api/system-info"),
        json!({"major":1,"type":"system.info.result","request_id":"web","ok":false,
            "error":{"code":-32001,"message":"System report collection timed out","data":{"retry_after_ms":1000}}})
    );
    assert_eq!(
        server.json("/api/connection"),
        json!({"jsonrpc":"2.0","id":"web","error":{"code":-32001,
            "message":"System report collection timed out","data":{"retry_after_ms":1000}}})
    );
    worker.join().unwrap();
}

#[test]
fn history_pages_filters_and_details_over_http() {
    let server = Server::start(true);
    assert_eq!(server.json("/api/health"), json!({"status":"ok"}));
    assert_eq!(
        server.ids("?limit=2"),
        ["9007199254740994", "9007199254740995"]
    );
    assert_eq!(
        server.ids("?before=9007199254740995&limit=1"),
        ["9007199254740994"]
    );
    assert_eq!(
        server.ids("?after=9007199254740993&limit=1"),
        ["9007199254740994"]
    );
    assert_eq!(
        server.ids("?component=UmD"),
        ["9007199254740993", "9007199254740994"]
    );
    assert_eq!(server.ids("?level=error"), ["9007199254740994"]);
    assert_eq!(
        server.ids("?program=gAME.ExE"),
        ["9007199254740993", "9007199254740994"]
    );
    assert_eq!(
        server.ids("?program=42"),
        ["9007199254740993", "9007199254740994"]
    );
    assert_eq!(
        server.ids("?q=hELLo&component=UMD&level=error&program=42"),
        ["9007199254740994"]
    );
    assert_eq!(server.ids("?q=hELLo&component=KMD"), Vec::<String>::new());
    assert_eq!(
        server.ids("?from=2026-11-10T09:00:01%2B01:00&until=2026-11-10T08:00:02Z"),
        ["9007199254740994"]
    );
    let result = server.json("/api/events?component=UMD&limit=1");
    assert_eq!(result["oldest_id"], "9007199254740993");
    assert_eq!(result["latest_id"], "9007199254740995");
    let detail = server.json("/api/events/9007199254740994");
    assert_eq!(detail["id"], "9007199254740994");
    assert_eq!(detail["received_at"], "2026-11-10T09:00:01+01:00");
    assert_eq!(
        detail["raw_message"],
        r#"{"component":"umd","level":"ERROR","process":{"pid":42,"exe_path":"C:\\Game.exe"},"text":"中文 Hello","seq":18446744073709551615}"#
    );
    assert_eq!(detail["message"]["text"], "中文 Hello");
    let db = Connection::open(server.dir.path().join("history.db")).unwrap();
    db.execute("INSERT INTO events VALUES(9007199254740996,'2026-11-10T08:00:03Z','{\"text\":\"arrival\"}')", []).unwrap();
    assert_eq!(server.ids("?after=9007199254740995"), ["9007199254740996"]);
}

#[test]
fn export_keeps_literal_numbers_and_static_routes_serve_assets() {
    let server = Server::start(true);
    let (status, body) = server.request("POST", "/api/events");
    assert_eq!(status, 405);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({"error":"Method not supported"})
    );
    let (status, body) = server.get("/api/export?component=UMD&level=error&program=game&q=HELLO&from=2026-11-10T08:00:01Z&until=2026-11-10T09:00:02%2B01:00");
    assert_eq!(status, 200);
    assert_eq!(body, "{\"received_at\":\"2026-11-10T09:00:01+01:00\",\"message\":{\"component\":\"umd\",\"level\":\"ERROR\",\"process\":{\"pid\":42,\"exe_path\":\"C:\\\\Game.exe\"},\"text\":\"中文 Hello\",\"seq\":18446744073709551615}}\n");
    assert_eq!(
        server.get("/app.js"),
        (200, "window.historyReady=true;".into())
    );
    assert_eq!(server.get("/history"), (200, "<h1>History</h1>".into()));
    for path in ["/api/unknown", "/api/events/1"] {
        let (status, body) = server.get(path);
        assert_eq!(status, 404);
        assert!(serde_json::from_str::<Value>(&body).unwrap()["error"].is_string());
    }
    for path in [
        "/api/events?limit=0",
        "/api/events?limit=1001",
        "/api/events?before=x",
        "/api/events?before=1&after=2",
        "/api/events?from=yesterday",
        "/api/export?from=2026-11-11T00:00:00Z&until=2026-11-10T00:00:00Z",
    ] {
        let (status, body) = server.get(path);
        assert_eq!(status, 400, "{path}: {body}");
        assert!(serde_json::from_str::<Value>(&body).unwrap()["error"].is_string());
    }
}

#[test]
fn missing_history_returns_json_errors() {
    let server = Server::start(false);
    for path in ["/api/events", "/api/events/1", "/api/export"] {
        let (status, body) = server.get(path);
        assert_eq!(status, 500);
        assert!(serde_json::from_str::<Value>(&body).unwrap()["error"].is_string());
    }
    assert!(!server.dir.path().join("history.db").exists());
}

#[test]
fn mixed_history_filters_projection_and_exports_rpc_notifications() {
    let server = Server::start(true);
    let db = Connection::open(server.dir.path().join("history.db")).unwrap();
    db.execute_batch("ALTER TABLE events ADD COLUMN raw_message TEXT")
        .unwrap();
    let raw = "{\n \"jsonrpc\": \"2.0\", \"method\": \"log\", \"params\": {\"stream_id\":\"stream\",\"seq\":\"18446744073709551615\",\"observed_at\":\"2026-11-10T08:00:01Z\",\"source\":\"driver\",\"component\":\"UMD\",\"level\":\"ERROR\",\"raw_level\":\"error\",\"counter\":18446744073709551615,\"text\":\"中文  🙂\\nnext\"} }";
    db.execute(
        "INSERT INTO events VALUES(9007199254740996,'2026-11-10T08:00:03Z',?1,?2)",
        params![
            r#"{"component":"UMD","level":"ERROR","process":{"pid":42},"text":"projection Hello"}"#,
            raw
        ],
    )
    .unwrap();
    let detail = server.json("/api/events/9007199254740996");
    assert_eq!(detail["raw_message"], raw);
    assert_eq!(detail["message"]["text"], "projection Hello");
    assert_eq!(server.ids("?q=projection"), ["9007199254740996"]);
    let (status, body) = server.get("/api/export?component=UMD&level=error&program=42&q=HELLO");
    assert_eq!(status, 200);
    let rows: Vec<Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![
            json!({"received_at":"2026-11-10T09:00:01+01:00","message":{"component":"umd","level":"ERROR","process":{"pid":42,"exe_path":"C:\\Game.exe"},"text":"中文 Hello","seq":18446744073709551615u64}}),
            json!({"received_at":"2026-11-10T08:00:03Z","message":{"jsonrpc":"2.0","method":"log","params":{"stream_id":"stream","seq":"18446744073709551615","observed_at":"2026-11-10T08:00:01Z","source":"driver","component":"UMD","level":"ERROR","raw_level":"error","counter":18446744073709551615u64,"text":"中文  🙂\nnext"}}}),
        ]
    );
    assert!(body.contains("\"counter\":18446744073709551615"));
}
