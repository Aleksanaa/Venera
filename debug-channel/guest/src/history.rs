use crate::journal::{ImportBatch, ImportReceipt, ImportSource, Journal, JournalInput};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use venera_debug_protocol::v1::{HistoryAck, HistoryPage, HistoryRange, HistoryRead};

enum Query {
    ProcessCapture(u64, u64, Option<u64>, SyncSender<io::Result<()>>),
    Processes(u64, SyncSender<io::Result<crate::ancestry::Checkpoint>>),
    Checkpoint(
        ImportSource,
        Vec<crate::ancestry::Lifecycle>,
        u64,
        u64,
        SyncSender<io::Result<u64>>,
    ),
    Sources(SyncSender<io::Result<Vec<ImportSource>>>),
    Complete(
        ImportSource,
        Option<(String, Option<u64>)>,
        u64,
        u64,
        SyncSender<io::Result<ImportReceipt>>,
    ),
    Reconcile(Vec<String>, u64, u64, SyncSender<io::Result<()>>),
    Durable(JournalInput, SyncSender<io::Result<()>>),
    Discover(
        Vec<ImportSource>,
        SyncSender<io::Result<Vec<ImportReceipt>>>,
    ),
    Import(ImportBatch, SyncSender<io::Result<ImportReceipt>>),
    Receipt(ImportSource, SyncSender<io::Result<ImportReceipt>>),
    Forget(ImportSource, SyncSender<io::Result<()>>),
    Range(SyncSender<io::Result<HistoryRange>>),
    Read(HistoryRead, SyncSender<io::Result<HistoryPage>>),
    Ack(HistoryAck, SyncSender<io::Result<HistoryAck>>),
    Forwarding {
        enabled: bool,
        state: Arc<Mutex<crate::state::State>>,
        persist: fn(bool) -> io::Result<()>,
        response: SyncSender<io::Result<venera_debug_protocol::Status>>,
    },
}

#[derive(Clone)]
pub struct History {
    input: SyncSender<JournalInput>,
    queries: SyncSender<Query>,
    stop: Arc<AtomicBool>,
}

impl History {
    pub fn process_capture(
        &self,
        boot: u64,
        captured_through: u64,
        resumed_at: Option<u64>,
    ) -> io::Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(
            &self.queries,
            Query::ProcessCapture(boot, captured_through, resumed_at, tx),
        )?;
        self.response(rx)
    }
    pub fn process_checkpoint(&self, boot: u64) -> io::Result<crate::ancestry::Checkpoint> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Processes(boot, tx))?;
        self.response(rx)
    }

    pub fn checkpoint_processes(
        &self,
        source: ImportSource,
        changes: Vec<crate::ancestry::Lifecycle>,
        through_filetime: u64,
        now_ms: u64,
    ) -> io::Result<u64> {
        if changes.len() > 128 || serde_json::to_vec(&changes)?.len() > 900 * 1024 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(
            &self.queries,
            Query::Checkpoint(source, changes, through_filetime, now_ms, tx),
        )?;
        self.response(rx)
    }
    pub fn import_sources(&self) -> io::Result<Vec<ImportSource>> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Sources(tx))?;
        self.response(rx)
    }
    pub fn complete_import(
        &self,
        source: ImportSource,
        diagnostic: Option<(String, Option<u64>)>,
        boot: u64,
        now_ms: u64,
    ) -> io::Result<ImportReceipt> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(
            &self.queries,
            Query::Complete(source, diagnostic, boot, now_ms, tx),
        )?;
        self.response(rx)
    }

    pub fn reconcile_imports(
        &self,
        present: Vec<String>,
        boot: u64,
        now_ms: u64,
    ) -> io::Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Reconcile(present, boot, now_ms, tx))?;
        self.response(rx)
    }

    pub fn append_durable(&self, entry: JournalInput) -> io::Result<()> {
        if entry_size(&entry)? > 900 * 1024 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Durable(entry, tx))?;
        self.response(rx)
    }
    pub fn start(
        mut journal: Journal,
        stop: Arc<AtomicBool>,
    ) -> io::Result<(Self, thread::JoinHandle<io::Result<()>>)> {
        let (input, entries) = mpsc::sync_channel(2048);
        let (queries, requests) = mpsc::sync_channel(64);
        let owner_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("guest-history".into())
            .spawn(move || {
                let mut pending = None;
                let mut next_prune = Instant::now();
                while !owner_stop.load(Ordering::Acquire) {
                    if Instant::now() >= next_prune {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        journal.prune(now)?;
                        next_prune = Instant::now() + Duration::from_secs(1);
                    }
                    for _ in 0..4 {
                        let Ok(query) = requests.try_recv() else {
                            break;
                        };
                        answer(&mut journal, query);
                    }
                    let first = match pending
                        .take()
                        .map(Ok)
                        .unwrap_or_else(|| entries.recv_timeout(Duration::from_millis(10)))
                    {
                        Ok(entry) => entry,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let mut bytes = entry_size(&first)?;
                    let mut batch = vec![first];
                    let deadline = Instant::now() + Duration::from_millis(10);
                    while batch.len() < 256 {
                        let next = match entries.try_recv() {
                            Ok(entry) => entry,
                            Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                                match entries.recv_timeout(
                                    deadline.saturating_duration_since(Instant::now()),
                                ) {
                                    Ok(entry) => entry,
                                    Err(_) => break,
                                }
                            }
                            Err(_) => break,
                        };
                        let size = entry_size(&next)?;
                        if bytes + size > 900 * 1024 {
                            pending = Some(next);
                            break;
                        }
                        bytes += size;
                        batch.push(next);
                    }
                    journal.append_batch(batch)?;
                }
                // Finish captures already accepted by the service before closing its journal.
                loop {
                    let mut batch = Vec::new();
                    let mut bytes = 0;
                    while batch.len() < 256 {
                        let next = pending.take().or_else(|| entries.try_recv().ok());
                        let Some(next) = next else { break };
                        let size = entry_size(&next)?;
                        if !batch.is_empty() && bytes + size > 900 * 1024 {
                            pending = Some(next);
                            break;
                        }
                        bytes += size;
                        batch.push(next);
                    }
                    if batch.is_empty() {
                        break;
                    }
                    journal.append_batch(batch)?;
                }
                journal.close()
            })?;
        Ok((
            Self {
                input,
                queries,
                stop,
            },
            worker,
        ))
    }
    pub fn append(&self, mut entry: JournalInput) -> io::Result<()> {
        if entry_size(&entry)? > 900 * 1024 {
            use venera_debug_protocol::v1::{DecimalU64, Gap, HistoryEvent};
            let HistoryEvent::Log { record } = entry.event else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Capture gap exceeds history budget",
                ));
            };
            entry.event = HistoryEvent::Gap {
                gap: Gap {
                    stream_id: record.stream_id,
                    reason: "record_size_exceeded".into(),
                    count: Some(DecimalU64(1)),
                    first_seq: Some(DecimalU64(record.seq)),
                    last_seq: Some(DecimalU64(record.seq)),
                },
            };
        }
        self.send(&self.input, entry)
    }
    fn send<T>(&self, sender: &SyncSender<T>, mut value: T) -> io::Result<()> {
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            match sender.try_send(value) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Full(returned)) => value = returned,
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(io::ErrorKind::BrokenPipe.into())
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn response<T>(&self, receiver: Receiver<io::Result<T>>) -> io::Result<T> {
        loop {
            match receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::ErrorKind::BrokenPipe.into())
                }
                Err(mpsc::RecvTimeoutError::Timeout) if self.stop.load(Ordering::Acquire) => {
                    return Err(io::ErrorKind::Interrupted.into())
                }
                Err(_) => (),
            }
        }
    }
    pub fn range(&self) -> io::Result<HistoryRange> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Range(tx))?;
        self.response(rx)
    }

    pub fn discover_imports(&self, sources: Vec<ImportSource>) -> io::Result<Vec<ImportReceipt>> {
        if sources.len() > 128 || sources.iter().any(|source| source.id.len() > 1024) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Capture inventory exceeds capacity",
            ));
        }
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Discover(sources, tx))?;
        self.response(rx)
    }

    pub fn import_batch(&self, batch: ImportBatch) -> io::Result<ImportReceipt> {
        let mut bytes = 0usize;
        if batch.entries.len() > 256 || batch.source.id.len() > 1024 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        for entry in &batch.entries {
            bytes += serde_json::to_vec(&entry.event)?.len() + 2048;
            if bytes > 900 * 1024 {
                return Err(io::ErrorKind::InvalidInput.into());
            }
        }
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Import(batch, tx))?;
        self.response(rx)
    }

    pub fn import_receipt(&self, source: ImportSource) -> io::Result<ImportReceipt> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Receipt(source, tx))?;
        self.response(rx)
    }

    /// Call after removing the completed source artifact.
    pub fn forget_import(&self, source: ImportSource) -> io::Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Forget(source, tx))?;
        self.response(rx)
    }
    pub fn read(&self, request: HistoryRead) -> io::Result<HistoryPage> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Read(request, tx))?;
        self.response(rx)
    }
    pub fn ack(&self, request: HistoryAck) -> io::Result<HistoryAck> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Ack(request, tx))?;
        self.response(rx)
    }
    pub fn forwarding(
        &self,
        enabled: bool,
        state: Arc<Mutex<crate::state::State>>,
        persist: fn(bool) -> io::Result<()>,
    ) -> io::Result<venera_debug_protocol::Status> {
        let (response, rx) = mpsc::sync_channel(1);
        self.send(
            &self.queries,
            Query::Forwarding {
                enabled,
                state,
                persist,
                response,
            },
        )?;
        self.response(rx)
    }
    pub fn rpc(
        &self,
        request: venera_debug_protocol::v1::Incoming,
        forwarding: bool,
    ) -> venera_debug_protocol::v1::RpcResult<serde_json::Value> {
        use venera_debug_protocol::v1::{Incoming, RpcError};
        let failure = |error: io::Error| RpcError::new(-32007, error.to_string());
        let value = match request {
            Incoming::HistoryRange { .. } => serde_json::to_value(self.range().map_err(failure)?),
            Incoming::HistoryRead { params, .. } => {
                if !forwarding {
                    return Err(RpcError::new(-32005, "Log forwarding is paused"));
                }
                serde_json::to_value(self.read(params).map_err(failure)?)
            }
            Incoming::HistoryAck { params, .. } => {
                serde_json::to_value(self.ack(params).map_err(failure)?)
            }
            _ => return Err(RpcError::new(-32601, "Method not found")),
        };
        value.map_err(|error| RpcError::new(-32603, error.to_string()))
    }
}

fn entry_size(entry: &JournalInput) -> io::Result<usize> {
    Ok(serde_json::to_vec(&entry.event)?.len() + entry.boot_id.len() + 512)
}

pub fn finish(
    producers: Vec<thread::JoinHandle<()>>,
    stop: &AtomicBool,
    owner: thread::JoinHandle<io::Result<()>>,
) -> io::Result<()> {
    for producer in producers {
        let _ = producer.join();
    }
    stop.store(true, Ordering::Release);
    owner
        .join()
        .map_err(|_| io::Error::other("History worker failed"))?
}

fn answer(journal: &mut Journal, query: Query) {
    match query {
        Query::Processes(boot, tx) => {
            let _ = tx.send(journal.process_checkpoint(boot));
        }
        Query::ProcessCapture(boot, captured_through, resumed_at, tx) => {
            let _ = tx.send(journal.process_capture(boot, captured_through, resumed_at));
        }
        Query::Checkpoint(source, changes, through, now_ms, tx) => {
            let _ = tx.send(journal.checkpoint_processes(&source, changes, through, now_ms));
        }
        Query::Sources(tx) => {
            let _ = tx.send(journal.import_sources());
        }
        Query::Complete(source, diagnostic, boot, now_ms, tx) => {
            let _ = tx.send(journal.complete_import(&source, diagnostic, boot, now_ms));
        }
        Query::Reconcile(present, boot, now_ms, tx) => {
            let _ = tx.send(journal.reconcile_imports(&present, boot, now_ms));
        }
        Query::Durable(entry, tx) => {
            let _ = tx.send(journal.append_batch(vec![entry]).map(|_| ()));
        }
        Query::Discover(sources, tx) => {
            let _ = tx.send(journal.discover_imports(sources));
        }
        Query::Import(batch, tx) => {
            let _ = tx.send(journal.import_batch(batch));
        }
        Query::Receipt(source, tx) => {
            let _ = tx.send(journal.import_receipt(&source));
        }
        Query::Forget(source, tx) => {
            let _ = tx.send(journal.forget_import(&source));
        }
        Query::Range(tx) => {
            let _ = tx.send(journal.range());
        }
        Query::Read(request, tx) => {
            let _ =
                tx.send(journal.read(request, venera_debug_protocol::v1::MAX_FRAME_BYTES - 1024));
        }
        Query::Ack(request, tx) => {
            let _ = tx.send(journal.ack(request));
        }
        Query::Forwarding {
            enabled,
            state,
            persist,
            response,
        } => {
            let result = persist(enabled).map(|()| {
                let mut state = state.lock().unwrap();
                state.set_forwarding(enabled);
                state.current()
            });
            let _ = response.send(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::RetentionPolicy;
    use std::sync::atomic::AtomicU64;
    use venera_debug_protocol::v1::{DecimalU64, HistoryCursor, HistoryEvent};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn forwarding_commands_publish_in_persisted_order_while_state_is_available() {
        use std::sync::{Barrier, OnceLock};
        static PERSISTENCE: OnceLock<(Barrier, Barrier, Mutex<Vec<bool>>)> = OnceLock::new();
        let gates =
            PERSISTENCE.get_or_init(|| (Barrier::new(2), Barrier::new(2), Mutex::new(Vec::new())));
        fn persist(enabled: bool) -> io::Result<()> {
            let gates = PERSISTENCE.get().unwrap();
            gates.2.lock().unwrap().push(enabled);
            if !enabled {
                gates.0.wait();
                gates.1.wait();
            }
            Ok(())
        }
        let root =
            std::env::temp_dir().join(format!("venera-forwarding-owner-{}", std::process::id()));
        let journal = Journal::open(&root, "commands", RetentionPolicy::default()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, worker) = History::start(journal, stop.clone()).unwrap();
        let state = Arc::new(Mutex::new(crate::state::State::new("s".into(), true)));
        let first = {
            let history = history.clone();
            let state = state.clone();
            thread::spawn(move || history.forwarding(false, state, persist).unwrap())
        };
        gates.0.wait();
        assert_eq!(state.try_lock().unwrap().current().forwarding_enabled, true);
        let second = {
            let history = history.clone();
            let state = state.clone();
            thread::spawn(move || history.forwarding(true, state, persist).unwrap())
        };
        gates.1.wait();
        assert_eq!(first.join().unwrap().forwarding_enabled, false);
        assert_eq!(second.join().unwrap().forwarding_enabled, true);
        assert_eq!(*gates.2.lock().unwrap(), [false, true]);
        assert_eq!(state.lock().unwrap().current().forwarding_enabled, true);
        let error = history
            .forwarding(false, state.clone(), |_| {
                Err(io::ErrorKind::PermissionDenied.into())
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(state.lock().unwrap().current().forwarding_enabled, true);
        stop.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn producer_finishes_accepted_capture_before_owner_shutdown_and_flush() {
        use std::sync::Barrier;
        let root =
            std::env::temp_dir().join(format!("venera-producer-stop-{}", std::process::id()));
        let journal = Journal::open(&root, "shutdown", RetentionPolicy::default()).unwrap();
        let producer_stop = Arc::new(AtomicBool::new(false));
        let owner_stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = History::start(journal, owner_stop.clone()).unwrap();
        let observed = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let producer = {
            let history = history.clone();
            let stop = producer_stop.clone();
            let observed = observed.clone();
            let resume = resume.clone();
            thread::spawn(move || {
                assert_eq!(stop.load(Ordering::Acquire), false);
                observed.wait();
                resume.wait();
                history.append(JournalInput { boot_id: "boot".into(), recorded_at_ms: 1, startup: true,
                    event: serde_json::from_value(serde_json::json!({"type":"log","record":{"stream_id":"s","seq":"1","observed_at":"original","source":"dbwin","component":"app","level":"info","raw_level":"info","text":"accepted during stop"}})).unwrap() }).unwrap();
            })
        };
        observed.wait();
        producer_stop.store(true, Ordering::Release);
        let (started, ready) = mpsc::sync_channel(1);
        let shutdown = {
            let owner_stop = owner_stop.clone();
            thread::spawn(move || {
                started.send(()).unwrap();
                finish(vec![producer], &owner_stop, owner)
            })
        };
        ready.recv().unwrap();
        assert_eq!(history.range().unwrap().high_water.0, 0);
        assert_eq!(owner_stop.load(Ordering::Acquire), false);
        resume.wait();
        shutdown.join().unwrap().unwrap();
        let journal = Journal::open(&root, "shutdown", RetentionPolicy::default()).unwrap();
        let page = journal
            .read(
                HistoryRead {
                    after: None,
                    through: None,
                    limit: 256,
                },
                1024 * 1024,
            )
            .unwrap();
        assert_eq!(page.next.position.0, 1);
        assert_eq!(
            serde_json::to_value(&page.records[0].event).unwrap()["record"]["text"],
            "accepted during stop"
        );
        drop(journal);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn etw_loss_samples_persist_incremental_gaps() {
        let root = std::env::temp_dir().join(format!("venera-etw-loss-{}", std::process::id()));
        let journal = Journal::open(&root, "etw-loss", RetentionPolicy::default()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, worker) = History::start(journal, stop.clone()).unwrap();
        let mut previous = 0;
        for total in [0, 3, 3, 7, 7] {
            if let Some(event) = crate::collector::etw_loss("s", &mut previous, total) {
                history
                    .append(JournalInput {
                        boot_id: "boot".into(),
                        recorded_at_ms: 1,
                        startup: true,
                        event,
                    })
                    .unwrap();
            }
        }
        stop.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
        let journal = Journal::open(&root, "etw-loss", RetentionPolicy::default()).unwrap();
        let page = journal
            .read(
                HistoryRead {
                    after: None,
                    through: None,
                    limit: 256,
                },
                1024 * 1024,
            )
            .unwrap();
        assert_eq!(page.next.position.0, 2);
        let gaps: Vec<_> = page
            .records
            .iter()
            .map(|entry| serde_json::to_value(&entry.event).unwrap()["gap"].clone())
            .collect();
        assert_eq!(
            gaps,
            vec![
                serde_json::json!({"stream_id":"s","reason":"etw_events_lost","count":"3","first_seq":null,"last_seq":null}),
                serde_json::json!({"stream_id":"s","reason":"etw_events_lost","count":"4","first_seq":null,"last_seq":null})
            ]
        );
        drop(journal);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quiet_history_expires_runtime_records_before_serving_reads() {
        let root = std::env::temp_dir().join(format!(
            "venera-owner-expiry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut journal = Journal::open(&root, "expiry", RetentionPolicy::default()).unwrap();
        for startup in [true, false] {
            let record = serde_json::from_value(serde_json::json!({"stream_id":"s","seq":"1","observed_at":"original","source":"dbwin","component":"app","level":"info","raw_level":"info","text":if startup {"startup"} else {"runtime"}})).unwrap();
            journal
                .append("boot", 100, startup, HistoryEvent::Log { record })
                .unwrap();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (history, worker) = History::start(journal, stop.clone()).unwrap();
        let page = history
            .read(HistoryRead {
                after: None,
                through: None,
                limit: 256,
            })
            .unwrap();
        assert_eq!(page.records.len(), 1);
        let HistoryEvent::Log { record } = &page.records[0].event else {
            panic!("log")
        };
        assert_eq!(record.text, "startup");
        assert_eq!(
            serde_json::to_value(page.missing).unwrap(),
            serde_json::json!([{"first":"2","last":"2","reason":"retention_expired"}])
        );
        assert_eq!(page.next.position.0, 2);
        stop.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn capture_batches_persist_without_connections_and_reopen_for_independent_readers() {
        let root = std::env::temp_dir().join(format!(
            "venera-owner-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let journal = Journal::open(&root, "owner-journal", RetentionPolicy::default()).unwrap();
        let (history, worker) = History::start(journal, stop.clone()).unwrap();
        for position in 1..=300 {
            let record = serde_json::from_value(serde_json::json!({"stream_id":"service","seq":position.to_string(),"observed_at":"original","source_time":"source ticks","source":"dbwin","component":"app","level":"info","raw_level":"info","text":format!("record {position}")})).unwrap();
            history
                .append(JournalInput {
                    boot_id: "boot".into(),
                    recorded_at_ms: 100,
                    startup: true,
                    event: HistoryEvent::Log { record },
                })
                .unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while history.range().unwrap().high_water.0 != 300 {
            assert!(Instant::now() < deadline, "capture commit deadline");
            thread::sleep(Duration::from_millis(10));
        }
        let request = HistoryRead {
            after: None,
            through: Some(DecimalU64(300)),
            limit: 256,
        };
        let paused = history
            .rpc(
                venera_debug_protocol::v1::Incoming::HistoryRead {
                    id: "paused".into(),
                    params: request.clone(),
                },
                false,
            )
            .unwrap_err();
        assert_eq!(
            (paused.code, paused.message.as_str()),
            (-32005, "Log forwarding is paused")
        );
        let resumed = history
            .rpc(
                venera_debug_protocol::v1::Incoming::HistoryRead {
                    id: "resumed".into(),
                    params: request.clone(),
                },
                true,
            )
            .unwrap();
        assert_eq!(resumed["records"][0]["event"]["record"]["text"], "record 1");
        let page = history.read(request.clone()).unwrap();
        assert_eq!((page.records.len(), page.next.position.0), (256, 256));
        let HistoryEvent::Log { record } = &page.records[0].event else {
            panic!("log")
        };
        assert_eq!(
            (
                record.text.as_str(),
                record.observed_at.as_str(),
                record.source_time.as_deref()
            ),
            ("record 1", "original", Some("source ticks"))
        );
        history
            .ack(HistoryAck {
                cursor: page.next.clone(),
            })
            .unwrap();
        assert_eq!(
            history.read(request).unwrap().records[0].cursor.position.0,
            1
        );
        let mut oversized = record.clone();
        oversized.seq = 301;
        oversized.text = "x".repeat(1024 * 1024);
        history
            .append(JournalInput {
                boot_id: "boot".into(),
                recorded_at_ms: 101,
                startup: true,
                event: HistoryEvent::Log { record: oversized },
            })
            .unwrap();
        let mut following = record.clone();
        following.seq = 302;
        following.text = "after oversized capture".into();
        history
            .append(JournalInput {
                boot_id: "boot".into(),
                recorded_at_ms: 102,
                startup: true,
                event: HistoryEvent::Log { record: following },
            })
            .unwrap();
        stop.store(true, Ordering::Release);
        assert_eq!(
            history.range().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        worker.join().unwrap().unwrap();
        let reopened = Journal::open(&root, "replacement", RetentionPolicy::default()).unwrap();
        assert_eq!(reopened.range().unwrap().journal_id, "owner-journal");
        let page = reopened
            .read(
                HistoryRead {
                    after: Some(HistoryCursor {
                        journal_id: "owner-journal".into(),
                        position: DecimalU64(256),
                    }),
                    through: None,
                    limit: 256,
                },
                1024 * 1024,
            )
            .unwrap();
        assert_eq!((page.records.len(), page.next.position.0), (46, 302));
        assert_eq!(
            serde_json::to_value(&page.records[44].event).unwrap(),
            serde_json::json!({"type":"gap","gap":{"stream_id":"service","reason":"record_size_exceeded","count":"1","first_seq":"301","last_seq":"301"}})
        );
        let HistoryEvent::Log { record } = &page.records[45].event else {
            panic!("log")
        };
        assert_eq!(record.text, "after oversized capture");
        reopened.close().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
