use super::*;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use venera_debug_protocol::client::{Client, HistoryConsumer, Start};

type Diagnostic = (Value, Option<String>);

pub(super) struct Diagnostics {
    sender: SyncSender<Diagnostic>,
    pub status: Arc<Mutex<Value>>,
}

pub(super) struct DiagnosticWriter {
    receiver: Receiver<Diagnostic>,
    pending: Option<Diagnostic>,
    status: Arc<Mutex<Value>>,
}

pub(super) fn diagnostics() -> (Diagnostics, DiagnosticWriter) {
    let (sender, receiver) = sync_channel(256);
    let status = Arc::new(Mutex::new(json!({"state":"ready","dropped":0})));
    (
        Diagnostics {
            sender,
            status: status.clone(),
        },
        DiagnosticWriter {
            receiver,
            pending: None,
            status,
        },
    )
}

impl Diagnostics {
    pub fn append(&self, message: Value, raw: Option<String>) {
        if let Err(error) = self.sender.try_send((message, raw)) {
            let mut status = self.status.lock().unwrap();
            let dropped = status["dropped"].as_u64().unwrap_or(0) + 1;
            status["dropped"] = json!(dropped);
            status["queue_error"] = json!(error.to_string());
        }
    }
}

impl DiagnosticWriter {
    fn drain(&mut self, store: &Store) -> bool {
        for _ in 0..16 {
            if self.pending.is_none() {
                self.pending = self.receiver.try_recv().ok();
            }
            let Some((message, raw)) = &self.pending else {
                return true;
            };
            match store.append_with_raw(message, raw.as_deref()) {
                Ok(()) => {
                    self.pending = None;
                    let mut status = self.status.lock().unwrap();
                    status["state"] = json!("ready");
                    status.as_object_mut().unwrap().remove("error");
                }
                Err(error) => {
                    let mut status = self.status.lock().unwrap();
                    status["state"] = json!(if store::is_busy(error.as_ref()) {
                        "waiting"
                    } else {
                        "error"
                    });
                    status["error"] = json!(error.to_string());
                    return false;
                }
            }
        }
        false
    }

    fn flush(&mut self, store: &Store) -> Result<()> {
        let dropped = self.status.lock().unwrap()["dropped"].as_u64().unwrap_or(0);
        if dropped > 0 {
            eprintln!("Archive diagnostic queue dropped {dropped} records");
        }
        let mut deadline = Instant::now() + Duration::from_secs(2);
        while !self.drain(store) {
            if self.pending.is_none() {
                deadline = Instant::now() + Duration::from_secs(2);
            }
            if Instant::now() >= deadline {
                self.status.lock().unwrap()["state"] = json!("flush_failed");
                return Err("Archive diagnostic flush timed out with queued records".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

pub(super) fn run(
    store: Store,
    connector: Arc<Mutex<impl transport::Connector>>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Value>>,
    mut diagnostics: DiagnosticWriter,
) -> Result<()> {
    store.nonblocking_writes()?;
    let mut consumer = HistoryConsumer::new(Start::Beginning);
    let mut limit = 256;
    while !stop.load(Ordering::Acquire) {
        diagnostics.drain(&store);
        let connected = connector
            .lock()
            .unwrap()
            .connect(Duration::from_millis(200));
        match connected {
            Ok(mut stream) => {
                if let Err(error) = session(
                    &store,
                    stream.as_mut(),
                    &mut consumer,
                    &stop,
                    &status,
                    &mut limit,
                    &mut diagnostics,
                ) {
                    *status.lock().unwrap() = json!({"state":"disconnected","cursor":consumer.cursor(),"error":error.to_string()});
                }
            }
            Err(error) => {
                *status.lock().unwrap() = json!({"state":"disconnected","cursor":consumer.cursor(),"error":error.to_string()})
            }
        }
        for _ in 0..20 {
            diagnostics.drain(&store);
            if stop.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    diagnostics.flush(&store)
}

// Split at a coverage boundary so a missing interval stays intact.
fn split_tail(page: &mut v1::HistoryPage) -> Option<v1::HistoryPage> {
    let mut ends: Vec<_> = page
        .records
        .iter()
        .map(|r| r.cursor.position)
        .chain(page.missing.iter().map(|m| m.last))
        .collect();
    if ends.len() < 2 {
        return None;
    }
    let mid = (ends.len() - 1) / 2;
    let cut = *ends.select_nth_unstable(mid).1;
    Some(v1::HistoryPage {
        range: page.range.clone(),
        records: page
            .records
            .extract_if(.., |r| r.cursor.position > cut)
            .collect(),
        missing: page.missing.extract_if(.., |m| m.last > cut).collect(),
        next: std::mem::replace(
            &mut page.next,
            v1::HistoryCursor {
                journal_id: page.range.journal_id.clone(),
                position: cut,
            },
        ),
    })
}

fn session(
    store: &Store,
    stream: &mut dyn transport::Stream,
    consumer: &mut HistoryConsumer,
    stop: &AtomicBool,
    status: &Mutex<Value>,
    limit: &mut u32,
    diagnostics: &mut DiagnosticWriter,
) -> Result<()> {
    let mut client = Client::default();
    client.connect();
    let mut pending = None;
    let mut since = Instant::now();
    let mut next = v1::Method::HistoryRange;
    let mut ack = None;
    let mut pages = Vec::new();
    let mut next_poll = Instant::now();
    *status.lock().unwrap() = json!({"state":"negotiating","cursor":consumer.cursor()});
    while !stop.load(std::sync::atomic::Ordering::Acquire) {
        diagnostics.drain(store);
        if (pending.is_some() || client.require(next).is_err())
            && since.elapsed() > Duration::from_secs(5)
        {
            return Err("History request timed out".into());
        }
        if pending.is_none() && Instant::now() >= next_poll && client.require(next).is_ok() {
            if next == v1::Method::HistoryRead && !pages.is_empty() {
                let mut page = pages.pop().unwrap();
                match store.append_history_page(&page) {
                    Ok(_) => {
                        // The shared checkpoint can be ahead of this reader's prefix.
                        consumer.page(&page).map_err(|e| e.message)?;
                        ack = Some(consumer.consumed().map_err(|e| e.message)?);
                        next = v1::Method::HistoryAck;
                    }
                    Err(error) if error.is::<store::HistoryCapacity>() => {
                        if let Some(tail) = split_tail(&mut page) {
                            *limit = (*limit).min(page.records.len().max(1) as u32);
                            pages.push(tail);
                        } else {
                            *status.lock().unwrap() = json!({"state":"storage_error","cursor":consumer.cursor(),"error":error.to_string(),"limit":1});
                            next_poll = Instant::now() + Duration::from_millis(200);
                        }
                        pages.push(page);
                    }
                    Err(error) if store::is_busy(error.as_ref()) => {
                        *status.lock().unwrap() = json!({"state":"waiting","cursor":consumer.cursor(),"error":error.to_string()});
                        pages.push(page);
                        next_poll = Instant::now() + Duration::from_millis(10);
                    }
                    Err(error) => return Err(error),
                }
                continue;
            }
            let params = match next {
                v1::Method::HistoryRead => {
                    let mut request = consumer.read_request();
                    request.limit = *limit;
                    serde_json::to_value(request)?
                }
                v1::Method::HistoryAck => serde_json::to_value(ack.as_ref().unwrap())?,
                _ => json!({}),
            };
            pending = Some(client.request(next, params).map_err(|e| e.message)?);
            since = Instant::now();
        }
        client.write(stream)?;
        for reply in client.read(stream)? {
            if pending.as_ref() != Some(&reply.id) {
                continue;
            }
            pending = None;
            let value = match reply.result {
                Ok(value) => value,
                Err(error) if error.code == -32005 => {
                    *status.lock().unwrap() = json!({"state":"paused","cursor":consumer.cursor()});
                    next_poll = Instant::now() + Duration::from_millis(200);
                    since = Instant::now();
                    continue;
                }
                Err(error) => return Err(error.message.into()),
            };
            match reply.method {
                v1::Method::HistoryRange => {
                    let range: v1::HistoryRange = serde_json::from_value(value)?;
                    let durable = store.history_cursor(&range.journal_id)?;
                    consumer.range(range, durable).map_err(|e| e.message)?;
                    next = v1::Method::HistoryRead;
                }
                v1::Method::HistoryRead => {
                    let page: v1::HistoryPage = serde_json::from_value(value)?;
                    consumer.page(&page).map_err(|e| e.message)?;
                    pages.push(page);
                }
                v1::Method::HistoryAck => {
                    let received: v1::HistoryAck = serde_json::from_value(value)?;
                    if ack.as_ref() != Some(&received) {
                        return Err("History acknowledgement cursor mismatch".into());
                    }
                    next = if consumer.caught_up() {
                        next_poll = Instant::now() + Duration::from_millis(200);
                        v1::Method::HistoryRange
                    } else {
                        v1::Method::HistoryRead
                    };
                }
                _ => unreachable!(),
            }
            *status.lock().unwrap() = json!({"state":if consumer.caught_up() {"caught_up"} else {"syncing"},"cursor":consumer.cursor()});
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_queue_reports_overflow_and_flushes_accepted_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.db");
        let store = Store::open(&path, 1048576).unwrap();
        let (queue, mut writer) = diagnostics();
        for n in 0..300 {
            queue.append(json!({"number":n}), None);
        }
        assert_eq!(queue.status.lock().unwrap()["dropped"], 44);
        writer.flush(&store).unwrap();
        let mut output = Vec::new();
        store::export(&path, &mut output, &store::ExportRange::default()).unwrap();
        let records: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 256);
        assert_eq!(records[0]["message"], json!({"number":0}));
        assert_eq!(records[255]["message"], json!({"number":255}));
        assert_eq!(queue.status.lock().unwrap()["dropped"], 44);
    }

    #[test]
    fn diagnostic_flush_reports_lock_timeout_and_retains_pending_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.db");
        let store = Store::open(&path, 1048576).unwrap();
        store.nonblocking_writes().unwrap();
        let lock = rusqlite::Connection::open(&path).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (queue, mut writer) = diagnostics();
        queue.append(json!({"type":"host.connection","connected":true}), None);
        assert_eq!(
            writer.flush(&store).unwrap_err().to_string(),
            "Archive diagnostic flush timed out with queued records"
        );
        assert_eq!(queue.status.lock().unwrap()["state"], "flush_failed");
        lock.execute_batch("COMMIT").unwrap();
        writer.flush(&store).unwrap();
        let mut output = Vec::new();
        store::export(&path, &mut output, &store::ExportRange::default()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap()["message"],
            json!({"type":"host.connection","connected":true})
        );
    }
}
