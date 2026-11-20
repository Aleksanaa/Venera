use super::*;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use venera_debug_protocol::client::{Client, HistoryConsumer, Start};

type Diagnostic = (Value, Option<String>);

pub(super) struct Diagnostics {
    sender: SyncSender<Diagnostic>,
    stop: Arc<AtomicBool>,
    bursts: Bursts,
    pub status: Arc<Mutex<Value>>,
}

pub(super) struct DiagnosticWriter {
    receiver: Receiver<Diagnostic>,
    bursts: Bursts,
    pending: Option<Diagnostic>,
    status: Arc<Mutex<Value>>,
}

type Bursts = Arc<Mutex<std::collections::BTreeMap<String, Value>>>;

pub(super) fn diagnostics(stop: Arc<AtomicBool>) -> (Diagnostics, DiagnosticWriter) {
    let (sender, receiver) = sync_channel(256);
    let bursts = Bursts::default();
    let status = Arc::new(Mutex::new(json!({"state":"ready","dropped":0})));
    (
        Diagnostics {
            sender,
            stop,
            bursts: bursts.clone(),
            status: status.clone(),
        },
        DiagnosticWriter {
            receiver,
            bursts,
            pending: None,
            status,
        },
    )
}

impl Diagnostics {
    pub fn append(&self, message: Value, raw: Option<String>) -> Result<()> {
        let mut item = (message, raw);
        loop {
            match self.sender.try_send(item) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => {
                    return Err("Archive diagnostic writer stopped".into())
                }
                Err(TrySendError::Full(value)) => item = value,
            }
            if let Some(kind @ ("status" | "host.connection")) = item.0["type"].as_str() {
                let at = store::now();
                let mut bursts = self.bursts.lock().unwrap();
                let summary=bursts.entry(kind.to_owned()).or_insert_with(||json!({"major":1,"type":"host.diagnostic_summary","kind":kind,"count":0,"first_at":at}));
                summary["count"] = json!(summary["count"].as_u64().unwrap() + 1);
                summary["last_at"] = json!(at);
                return Ok(());
            }
            if self.stop.load(Ordering::Acquire) {
                return Err("Archive stopped with a waiting diagnostic".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl DiagnosticWriter {
    fn drain(&mut self, store: &Store) -> bool {
        for i in 0..16 {
            if self.pending.is_none() {
                let summary = || {
                    self.bursts
                        .lock()
                        .unwrap()
                        .pop_first()
                        .map(|(_, v)| (v, None))
                };
                self.pending = if i == 15 {
                    summary().or_else(|| self.receiver.try_recv().ok())
                } else {
                    self.receiver.try_recv().ok().or_else(summary)
                };
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
                Err(error) if error.is::<store::DiagnosticCapacity>() => {
                    let mut status = self.status.lock().unwrap();
                    status["dropped"] = json!(status["dropped"].as_u64().unwrap_or(0) + 1);
                    status["last_rejection"] = json!({"at":store::now(),"reason":error.to_string(),"type":message["type"]});
                    self.pending = None;
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
    fn status_burst_exports_each_transition_or_its_count_and_interval() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.db");
        let store = Store::open(&path, 1048576).unwrap();
        let (queue, mut writer) = diagnostics(Arc::default());
        for n in 0..300 {
            queue
                .append(json!({"type":"status","number":n}), None)
                .unwrap();
        }
        assert_eq!(queue.status.lock().unwrap()["dropped"], 0);
        writer.flush(&store).unwrap();
        let mut output = Vec::new();
        store::export(&path, &mut output, &store::ExportRange::default()).unwrap();
        let records: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let singles: Vec<_> = records
            .iter()
            .filter_map(|r| r["message"]["number"].as_u64())
            .collect();
        assert_eq!(singles, (0..256).collect::<Vec<_>>());
        let summary = &records
            .iter()
            .find(|r| r["message"]["type"] == "host.diagnostic_summary")
            .unwrap()["message"];
        assert_eq!(summary["count"], 44);
        assert!(summary["first_at"].as_str().unwrap() <= summary["last_at"].as_str().unwrap());
        assert_eq!(queue.status.lock().unwrap()["dropped"], 0);
    }

    #[test]
    fn oversized_diagnostic_reports_rejection_and_releases_queue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.db");
        let store = Store::open(&path, 131072).unwrap();
        let (queue, mut writer) = diagnostics(Arc::default());
        queue
            .append(json!({"text":"x".repeat(100000)}), None)
            .unwrap();
        queue
            .append(json!({"type":"status","sequence":2}), None)
            .unwrap();
        writer.flush(&store).unwrap();
        let mut bytes = Vec::new();
        store::export(&path, &mut bytes, &Default::default()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["message"]["sequence"],
            2
        );
        let status = queue.status.lock().unwrap();
        assert_eq!(status["dropped"], 1);
        assert_eq!(
            status["last_rejection"]["reason"],
            "storage budget cannot hold this diagnostic"
        );
    }

    #[test]
    fn diagnostic_flush_reports_lock_timeout_and_retains_pending_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.db");
        let store = Store::open(&path, 1048576).unwrap();
        store.nonblocking_writes().unwrap();
        let lock = rusqlite::Connection::open(&path).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (queue, mut writer) = diagnostics(Arc::default());
        queue
            .append(json!({"type":"host.connection","connected":true}), None)
            .unwrap();
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
