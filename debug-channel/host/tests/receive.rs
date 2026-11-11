use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_venera-debug-host");

fn control(dir: &Path, id: &str) -> BufReader<UnixStream> {
    let mut stream = UnixStream::connect(dir.join("control.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(7)))
        .unwrap();
    send(
        &mut stream,
        json!({"major":1,"type":"filter.set","request_id":id,"filter":{"components":[],"levels":[],"targets":[]}}),
    );
    BufReader::new(stream)
}

#[test]
fn concurrent_requests_are_rejected_and_keep_local_correlation() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _receiver = start(dir.path(), "1048576");
    let guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    assert_eq!(read_message(&mut reader)["type"], "hello");
    assert_eq!(read_message(&mut reader)["type"], "status.get");
    let mut first = control(dir.path(), "client-literal-7");
    let request = read_message(&mut reader);
    assert_eq!(request["type"], "filter.set");
    assert_ne!(request["request_id"], "client-literal-7");
    let started = Instant::now();
    let mut second = control(dir.path(), "client-literal-8");
    let mut third = control(dir.path(), "client-literal-9");
    for (client, id) in [
        (&mut second, "client-literal-8"),
        (&mut third, "client-literal-9"),
    ] {
        let response = read_message(client);
        let decoded =
            venera_debug_protocol::decode(&serde_json::to_vec(&response).unwrap()).unwrap();
        let venera_debug_protocol::Message::RequestResult { result } = decoded.body else {
            panic!("request result")
        };
        assert_eq!(result.request_id, id);
        assert_eq!(result.operation, "filter.set");
        assert_eq!(
            response,
            json!({"major":1,"type":"request.result","request_id":id,"operation":"filter.set","ok":false,"error":{"code":"busy","message":"Another control request is in progress; query status before retrying"}})
        );
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        read_message(&mut first),
        json!({"major":1,"type":"request.result","request_id":"client-literal-7","operation":"filter.set","ok":false,"error":{"code":"outcome_unknown","message":"Request timed out; query status to confirm the effective configuration"}})
    );
    guest
        .set_read_timeout(Some(Duration::from_secs(6)))
        .unwrap();
    let mut extra = String::new();
    let error = reader.read_line(&mut extra).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    assert_eq!(extra, "");
}

#[test]
fn guest_success_failure_and_disconnect_restore_literal_request_id() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _receiver = start(dir.path(), "1048576");
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    read_message(&mut reader);
    read_message(&mut reader);
    for ok in [true, false] {
        let mut client = control(dir.path(), "client-literal-7");
        let request = read_message(&mut reader);
        let mut response = json!({"major":1,"type":"request.result","request_id":request["request_id"],"operation":"filter.set","ok":ok,"expires_at":null});
        if !ok {
            response["error"] = json!({"code":"rejected","message":"Guest rejected this filter"});
        }
        send(&mut guest, response.clone());
        response["request_id"] = json!("client-literal-7");
        assert_eq!(read_message(&mut client), response);
    }
    let mut client = control(dir.path(), "client-literal-7");
    assert_eq!(read_message(&mut reader)["type"], "filter.set");
    drop(reader);
    drop(guest);
    assert_eq!(
        read_message(&mut client),
        json!({"major":1,"type":"request.result","request_id":"client-literal-7","operation":"filter.set","ok":false,"error":{"code":"outcome_unknown","message":"Connection interrupted; query status after reconnection"}})
    );
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
            .arg("--database")
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
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}
fn read_message(reader: &mut BufReader<UnixStream>) -> Value {
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(&line).unwrap();
    }
}
fn send(stream: &mut UnixStream, value: Value) {
    writeln!(stream, "{value}").unwrap();
}
fn log(seq: u64, text: &str) -> Value {
    json!({"major":1,"type":"log","stream_id":"guest-test","seq":seq,"observed_at":"2026-11-10T08:00:01Z","source_time":null,"source":"dbwin","component":"dxvk","level":"trace","raw_level":"trace","session_id":2,"process":null,"tid":null,"text":text,"extra":{"retained":true}})
}
fn exported(dir: &Path) -> Vec<Value> {
    let output = Command::new(BIN)
        .arg("export")
        .arg("--database")
        .arg(dir.join("history.db"))
        .output()
        .unwrap();
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
}
fn wait_for(dir: &Path, predicate: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let rows = exported(dir);
        if predicate(&rows) {
            return rows;
        }
        assert!(Instant::now() < deadline, "history: {rows:?}");
        sleep(Duration::from_millis(20));
    }
}

#[test]
fn receive_control_reconnect_and_export_through_cli() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let receiver = start(dir.path(), "1048576");
    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    assert_eq!(read_message(&mut reader)["type"], "hello");
    assert_eq!(read_message(&mut reader)["type"], "status.get");
    send(&mut guest, log(1, "中文\n🙂"));
    guest
        .write_all(b"bad\n{\"major\":2,\"type\":\"future\"}\n")
        .unwrap();
    send(&mut guest, log(2, "after malformed"));
    let rows = wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["seq"] == 2)
    });
    let first = rows.iter().find(|r| r["message"]["seq"] == 1).unwrap();
    assert_eq!(first["message"]["text"], "中文\n🙂");
    assert_eq!(first["message"]["extra"], json!({"retained":true}));
    assert!(first["received_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        rows.iter()
            .filter(|r| r["message"]["type"] == "host.protocol_error")
            .count(),
        2
    );

    for operation in ["status", "filter-set", "filter-reset"] {
        let mut cli = Command::new(BIN);
        cli.arg(operation)
            .arg("--control")
            .arg(dir.path().join("control.sock"));
        if operation == "filter-set" {
            cli.args([
                "--filter",
                "{\"components\":[\"dxvk\"],\"levels\":[],\"targets\":[]}",
                "--duration-ms",
                "60000",
            ]);
        }
        let child = cli
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let request = read_message(&mut reader);
        let expected_operation = match operation {
            "status" => "status.get",
            "filter-set" => "filter.set",
            _ => "filter.reset",
        };
        assert_eq!(request["type"], expected_operation);
        if operation == "filter-set" {
            assert_eq!(
                request["filter"],
                json!({"components":["dxvk"],"levels":[],"targets":[]})
            );
            assert_eq!(request["duration_ms"], 60000);
        }
        send(
            &mut guest,
            json!({"major":1,"type":"request.result","request_id":"different-request","operation":expected_operation,"ok":true}),
        );
        send(
            &mut guest,
            json!({"major":1,"type":"request.result","request_id":request["request_id"],"operation":"other.operation","ok":true}),
        );
        let mut response = json!({"major":1,"type":"request.result","request_id":request["request_id"],"operation":expected_operation,"ok":true,"revision":7,"effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"forwarding_enabled":true});
        send(&mut guest, response.clone());
        response["request_id"] = json!("cli");
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&result.stdout).unwrap(),
            response
        );
    }

    let child = Command::new(BIN)
        .arg("filter-reset")
        .arg("--control")
        .arg(dir.path().join("control.sock"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert_eq!(read_message(&mut reader)["type"], "filter.reset");
    guest.write_all(b"unfinished").unwrap();
    drop(reader);
    drop(guest);
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["error"]["code"],
        "outcome_unknown"
    );
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["type"], "request.result");
    assert_eq!(response["request_id"], "cli");
    assert_eq!(response["operation"], "filter.reset");

    let mut guest = accept(&listener);
    let mut reader = BufReader::new(guest.try_clone().unwrap());
    assert_eq!(read_message(&mut reader)["type"], "hello");
    assert_eq!(read_message(&mut reader)["type"], "status.get");
    send(&mut guest, log(3, "after reconnect"));
    wait_for(dir.path(), |rows| {
        rows.iter()
            .any(|r| r["message"]["text"] == "after reconnect")
    });
    drop(receiver);
    drop(reader);
    drop(guest);
    let _receiver = start(dir.path(), "1048576");
    let mut guest = accept(&listener);
    send(&mut guest, log(4, "after restart"));
    let rows = wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["seq"] == 4)
    });
    assert_eq!(
        rows.iter()
            .filter(|r| r["message"]["type"] == "log")
            .map(|r| r["message"]["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "中文\n🙂",
            "after malformed",
            "after reconnect",
            "after restart"
        ]
    );
}

#[test]
fn capacity_evicts_oldest_and_reclaims_physical_space() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let _receiver = start(dir.path(), "131072");
    let mut guest = accept(&listener);
    let sampling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let peak = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let sampler = {
        let sampling = sampling.clone();
        let peak = peak.clone();
        let path = dir.path().to_owned();
        std::thread::spawn(move || {
            while sampling.load(std::sync::atomic::Ordering::Relaxed) {
                let bytes: u64 = [
                    "history.db",
                    "history.db-wal",
                    "history.db-shm",
                    "history.db-journal",
                ]
                .iter()
                .map(|name| {
                    std::fs::metadata(path.join(name))
                        .map(|m| m.len())
                        .unwrap_or(0)
                })
                .sum();
                peak.fetch_max(bytes, std::sync::atomic::Ordering::Relaxed);
                sleep(Duration::from_millis(1));
            }
        })
    };
    for seq in 0..100 {
        send(&mut guest, log(seq, &"x".repeat(3000)));
    }
    send(&mut guest, log(100, "newest"));
    let rows = wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["text"] == "newest")
    });
    let sequences: Vec<u64> = rows
        .iter()
        .filter_map(|r| r["message"]["seq"].as_u64())
        .collect();
    assert_eq!(sequences.last(), Some(&100));
    assert!(sequences[0] > 0, "oldest entries evicted: {sequences:?}");
    assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));
    let bytes: u64 = [
        "history.db",
        "history.db-wal",
        "history.db-shm",
        "history.db-journal",
    ]
    .iter()
    .map(|name| {
        std::fs::metadata(dir.path().join(name))
            .map(|m| m.len())
            .unwrap_or(0)
    })
    .sum();
    assert!(bytes <= 131072, "physical bytes: {bytes}");
    sampling.store(false, std::sync::atomic::Ordering::Relaxed);
    sampler.join().unwrap();
    let peak = peak.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        peak > 16384 && peak <= 131072,
        "sampled physical peak: {peak}"
    );
}

#[test]
fn insufficient_record_capacity_reports_storage_error() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let mut receiver = start(dir.path(), "65536");
    let mut guest = accept(&listener);
    send(&mut guest, log(1, &"x".repeat(50000)));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = receiver.0.try_wait().unwrap() {
            assert_eq!(status.code(), Some(1));
            break;
        }
        assert!(Instant::now() < deadline, "expected storage error");
        sleep(Duration::from_millis(10));
    }
    let mut error = String::new();
    std::io::Read::read_to_string(&mut receiver.0.stderr.take().unwrap(), &mut error).unwrap();
    assert_eq!(error, "storage budget cannot hold this record\n");
    let output = Command::new(BIN)
        .args(["receive", "--socket"])
        .arg(dir.path().join("guest.sock"))
        .arg("--database")
        .arg(dir.path().join("tiny.db"))
        .args(["--max-bytes", "1", "--control"])
        .arg(dir.path().join("tiny.sock"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "storage budget must be at least 65536 bytes\n"
    );
}

#[test]
fn service_termination_and_restart_preserve_committed_history() {
    let dir = TempDir::new().unwrap();
    let listener = UnixListener::bind(dir.path().join("guest.sock")).unwrap();
    let mut receiver = start(dir.path(), "131072");
    let mut guest = accept(&listener);
    send(&mut guest, log(1, "before service stop"));
    wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["seq"] == 1)
    });
    assert!(Command::new("kill")
        .args(["-TERM", &receiver.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let status = receiver.0.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(15));
    drop(guest);
    let _receiver = start(dir.path(), "131072");
    let mut guest = accept(&listener);
    send(&mut guest, log(2, "after service restart"));
    let rows = wait_for(dir.path(), |rows| {
        rows.iter().any(|r| r["message"]["seq"] == 2)
    });
    assert_eq!(
        rows.iter()
            .filter(|r| r["message"]["type"] == "log")
            .map(|r| r["message"]["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["before service stop", "after service restart"]
    );
}
