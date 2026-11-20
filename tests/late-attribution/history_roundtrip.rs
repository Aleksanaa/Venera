use super::*;

// Observe the negotiated pages and confirm the commit through a separate SQLite handle
// while handling each ACK from the production receiver.
fn transfer(db: &Path, history: &History) -> (Vec<(u64, u64, usize)>, Vec<u64>) {
    let (host, guest) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    guest
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let status = Arc::new(Mutex::new(json!(null)));
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel();
    let db_path = db.to_owned();
    let history = history.clone();
    let server = thread::spawn(move || {
        let mut stream = BufReader::new(guest);
        let mut connection = Connection::new(2);
        for request in connection.connect() {
            stream
                .get_mut()
                .write_all(&line(&request).unwrap())
                .unwrap();
        }
        let (mut pages, mut acks) = (Vec::new(), Vec::new());
        let mut bytes = Vec::new();
        while stream.read_until(b'\n', &mut bytes).unwrap() != 0 {
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let responses = connection.receive_negotiated(
                &bytes,
                |_| panic!("unexpected host control request"),
                |rpc, _, encoding| {
                    assert_eq!(encoding, HistoryEncoding::Attribution);
                    history.rpc_encoded(rpc, true, encoding)
                },
            );
            assert_eq!(responses.parse_errors, 0, "{request}");
            for response in responses.output {
                if let Some(error) = response.get("error") {
                    panic!("{}: {error}", request["method"]);
                }
                let frame = line(&response).unwrap();
                match request["method"].as_str() {
                    Some("history.read") => {
                        let read: HistoryRead =
                            serde_json::from_value(request["params"].clone()).unwrap();
                        let page: AttributionHistoryPage =
                            serde_json::from_value(response["result"].clone()).unwrap();
                        let after = read.after.unwrap();
                        assert_eq!(after.journal_id, ID);
                        assert_eq!(page.next.journal_id, ID);
                        assert!(page.missing.is_empty());
                        pages.push((after.position.0, page.next.position.0, frame.len() - 1));
                    }
                    Some("history.ack") => {
                        let ack: HistoryAck =
                            serde_json::from_value(request["params"].clone()).unwrap();
                        assert_eq!(response["result"], request["params"]);
                        let durable = Store::open(&db_path, None)
                            .unwrap()
                            .history_cursor(ID)
                            .unwrap();
                        assert_eq!(durable, Some(ack.cursor.clone()));
                        acks.push(ack.cursor.position.0);
                    }
                    _ => (),
                }
                stream.get_mut().write_all(&frame).unwrap();
                if acks.last() == Some(&4) && request["method"] == "history.ack" {
                    let _ = done_tx.send(());
                }
            }
            bytes.clear();
        }
        (pages, acks)
    });
    let worker_stop = stop.clone();
    let worker_status = status.clone();
    let archive = Store::open(db, None).unwrap();
    let worker = thread::spawn(move || {
        let (_diagnostics, writer) = history_sync::diagnostics(worker_stop.clone());
        history_sync::run(
            archive,
            Arc::new(Mutex::new(OneConnection(Some(host)))),
            worker_stop,
            worker_status,
            writer,
        )
        .unwrap();
    });
    let done = done_rx.recv_timeout(Duration::from_secs(8));
    if done.is_ok() {
        let deadline = Instant::now() + Duration::from_secs(2);
        while status.lock().unwrap()["state"] != "caught_up" && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
    }
    stop.store(true, Ordering::Release);
    worker.join().unwrap();
    let observations = server.join().unwrap();
    done.unwrap_or_else(|error| panic!("ACK 4: {error}; status={}", status.lock().unwrap()));
    assert_eq!(status.lock().unwrap()["cursor"], json!(cursor(4)));
    assert_eq!(status.lock().unwrap()["state"], "caught_up");
    observations
}

pub(super) fn run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("journal");
    let db = dir.path().join("history.db");
    let guest = Guest::open(&root);
    let left = "before-950K ".repeat(2_700);
    let right = "after-950K ".repeat(3_000);
    let large: String = "history-950KiB: "
        .bytes()
        .chain((b' '..=b'~').cycle())
        .take(950 * 1024)
        .map(char::from)
        .collect();
    let oversize = "\0".repeat(180 * 1024);
    for (seq, text) in [(1, &left), (2, &large), (3, &right), (4, &oversize)] {
        let record = serde_json::from_value(json!({
            "stream_id":"large-history", "seq":seq.to_string(), "observed_at":TIMES[0],
            "level":"info", "raw_level":"info", "source":"dbwin", "component":"fixture", "text":text,
        })).unwrap();
        guest
            .0
            .append_durable(JournalInput {
                boot_id: BOOT.to_string(),
                recorded_at_ms: MS,
                startup: true,
                event: HistoryEvent::Log { record },
            })
            .unwrap();
    }
    guest.close();
    let guest = Guest::open(&root);
    let (pages, acks) = transfer(&db, &guest.0);
    assert_eq!(
        pages
            .iter()
            .map(|&(after, next, _)| (after, next))
            .collect::<Vec<_>>(),
        [(0, 2), (2, 4)]
    );
    assert_eq!(acks, [2, 4]);
    println!("initial pages (after, next, frame bytes)={pages:?}; durable ACKs={acks:?}");

    let (pages, acks) = transfer(&db, &guest.0);
    assert_eq!(
        pages
            .iter()
            .map(|&(after, next, _)| (after, next))
            .collect::<Vec<_>>(),
        [(4, 4)]
    );
    assert_eq!(acks, [4]);
    println!("resume pages (after, next, frame bytes)={pages:?}; durable ACKs={acks:?}");

    let mut exported = Vec::new();
    store::export(&db, &mut exported, &ExportRange::parse(None, None).unwrap()).unwrap();
    let rows: Vec<AttributionHistoryRecord> = lines(&exported)
        .into_iter()
        .map(|row| serde_json::from_value(row["message"].clone()).unwrap())
        .collect();
    assert_eq!(
        rows.iter()
            .map(|row| row.cursor.clone())
            .collect::<Vec<_>>(),
        [cursor(1), cursor(2), cursor(3), cursor(4)]
    );
    for (i, expected) in [left, large, right].iter().enumerate() {
        let AttributionHistoryEvent::Log { record } = &rows[i].event else {
            panic!("expected log at position {}", i + 1)
        };
        assert_eq!(record.seq, i as u64 + 1);
        assert!(
            record.text.as_bytes() == expected.as_bytes(),
            "body differs at position {}",
            i + 1
        );
    }
    assert_eq!(
        serde_json::to_value(&rows[3].event).unwrap(),
        json!({
            "type":"gap", "gap":{"stream_id":"large-history","reason":"record_size_exceeded",
            "count":"1","first_seq":"4","last_seq":"4"}
        })
    );
    println!("reopened SQLite export: positions 1–4, 972800 matching body bytes at position 2, one size gap at position 4");
    guest.close();
}
