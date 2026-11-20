use crate::journal::{
    single_record_page_fits, ImportBatch, ImportReceipt, ImportSource, Journal, JournalInput,
    MAX_IMPORT_SOURCE_ID_BYTES,
};
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
use venera_debug_protocol::v1::{
    DecimalU64, Gap, HistoryAck, HistoryCursor, HistoryEncoding, HistoryEvent, HistoryPage,
    HistoryRange, HistoryRead, HistoryRecord, MAX_FRAME_BYTES,
};

const BATCH_TARGET_BYTES: usize = 900 * 1024;
const HISTORY_PAGE_BYTES: usize = MAX_FRAME_BYTES - 1024;
const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;

#[cfg(test)]
#[path = "history_attribution_tests.rs"]
mod attribution_tests;

enum Query {
    ProcessCapture(u64, u64, Option<u64>, SyncSender<io::Result<()>>),
    Processes(u64, SyncSender<io::Result<crate::ancestry::Checkpoint>>),
    AttributeProcess(
        u64,
        u64,
        bool,
        venera_debug_protocol::LogRecord,
        SyncSender<
            io::Result<(
                crate::ancestry::Attribution,
                venera_debug_protocol::LogRecord,
            )>,
        >,
    ),
    #[cfg(test)]
    ProcessColdReads(SyncSender<io::Result<u64>>),
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
        Option<crate::engine_events::GraphicsUpdate>,
        SyncSender<io::Result<ImportReceipt>>,
    ),
    GraphicsFragments(u64, SyncSender<io::Result<Vec<u8>>>),
    GraphicsBoots(SyncSender<io::Result<Vec<u64>>>),
    FinishGraphicsBoot(u64, u64, SyncSender<io::Result<()>>),
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
    Read(
        HistoryRead,
        HistoryEncoding,
        SyncSender<io::Result<serde_json::Value>>,
    ),
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
    journal_id: Arc<str>,
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

    pub(crate) fn attribute_process_record(
        &self,
        boot: u64,
        time: u64,
        wait: bool,
        record: venera_debug_protocol::LogRecord,
    ) -> io::Result<(
        crate::ancestry::Attribution,
        venera_debug_protocol::LogRecord,
    )> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(
            &self.queries,
            Query::AttributeProcess(boot, time, wait, record, tx),
        )?;
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
        self.complete_graphics_import(source, diagnostic, boot, now_ms, None)
    }

    pub fn complete_graphics_import(
        &self,
        source: ImportSource,
        diagnostic: Option<(String, Option<u64>)>,
        boot: u64,
        now_ms: u64,
        graphics: Option<crate::engine_events::GraphicsUpdate>,
    ) -> io::Result<ImportReceipt> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(
            &self.queries,
            Query::Complete(source, diagnostic, boot, now_ms, graphics, tx),
        )?;
        self.response(rx)
    }

    pub fn graphics_fragments(&self, boot: u64) -> io::Result<Vec<u8>> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::GraphicsFragments(boot, tx))?;
        self.response(rx)
    }

    pub fn graphics_boots(&self) -> io::Result<Vec<u64>> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::GraphicsBoots(tx))?;
        self.response(rx)
    }

    pub fn finish_graphics_boot(&self, boot: u64, now_ms: u64) -> io::Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::FinishGraphicsBoot(boot, now_ms, tx))?;
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
        let entry = self.prepare_input(entry)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Durable(entry, tx))?;
        self.response(rx)
    }
    pub fn start(
        mut journal: Journal,
        stop: Arc<AtomicBool>,
    ) -> io::Result<(Self, thread::JoinHandle<io::Result<()>>)> {
        let journal_id = journal.range()?.journal_id.into();
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
                        if bytes + size > BATCH_TARGET_BYTES {
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
                        if !batch.is_empty() && bytes + size > BATCH_TARGET_BYTES {
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
                journal_id,
            },
            worker,
        ))
    }
    pub fn append(&self, entry: JournalInput) -> io::Result<()> {
        self.send(&self.input, self.prepare_input(entry)?)
    }

    fn prepare_input(&self, mut entry: JournalInput) -> io::Result<JournalInput> {
        // Use the same retention context as Journal for structured captures.
        if let HistoryEvent::Log { record } = &entry.event {
            if let Some(event) = &record.structured {
                let boot = event.boot_time_filetime.0;
                let time = event.source_time_filetime.0;
                if time < boot || boot == 0 || time < UNIX_EPOCH_FILETIME {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid structured event boot or time",
                    ));
                }
                entry.boot_id = boot.to_string();
                entry.recorded_at_ms = (time - UNIX_EPOCH_FILETIME) / 10_000;
                entry.startup = time - boot < 120 * 10_000_000;
            }
        }
        entry.event = fit_history_event(
            &self.journal_id,
            &entry.boot_id,
            entry.recorded_at_ms,
            entry.startup,
            entry.event,
        )?;
        Ok(entry)
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
        if sources.len() > 128
            || sources
                .iter()
                .any(|source| source.id.len() > MAX_IMPORT_SOURCE_ID_BYTES)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Capture inventory exceeds capacity",
            ));
        }
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Discover(sources, tx))?;
        self.response(rx)
    }

    pub fn import_batch(&self, mut batch: ImportBatch) -> io::Result<ImportReceipt> {
        let mut bytes = 0usize;
        if batch.entries.len() > 256 || batch.source.id.len() > MAX_IMPORT_SOURCE_ID_BYTES {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let single = batch.entries.len() == 1;
        let boot = batch.source.boot_filetime;
        let boot_id = boot.to_string();
        let mut entries = Vec::with_capacity(batch.entries.len());
        for mut entry in std::mem::take(&mut batch.entries) {
            let time = entry.source_filetime;
            let elapsed = time.checked_sub(boot).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid capture event time")
            })?;
            let recorded_at_ms = time.checked_sub(UNIX_EPOCH_FILETIME).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "capture time precedes Unix epoch",
                )
            })? / 10_000;
            // Check the original identity before a size diagnostic can replace the log.
            if let HistoryEvent::Log { record } = &entry.event {
                if let Some(event) = &record.structured {
                    if boot == 0
                        || event.boot_time_filetime.0 != boot
                        || event.source_time_filetime.0 != time
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "capture header and event identity differ",
                        ));
                    }
                }
            }
            entry.event = fit_history_event(
                &self.journal_id,
                &boot_id,
                recorded_at_ms,
                elapsed < 120 * 10_000_000,
                entry.event,
            )?;
            bytes += serde_json::to_vec(&entry.event)?.len() + 2048;
            if !single && bytes > BATCH_TARGET_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "capture import batch exceeds its byte target; split the batch",
                ));
            }
            entries.push(entry);
        }
        batch.entries = entries;
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
        Ok(serde_json::from_value(
            self.read_encoded(request, HistoryEncoding::Compatibility)?,
        )?)
    }
    fn read_encoded(
        &self,
        request: HistoryRead,
        encoding: HistoryEncoding,
    ) -> io::Result<serde_json::Value> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(&self.queries, Query::Read(request, encoding, tx))?;
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
        self.rpc_encoded(request, forwarding, HistoryEncoding::Compatibility)
    }
    pub fn rpc_encoded(
        &self,
        request: venera_debug_protocol::v1::Incoming,
        forwarding: bool,
        encoding: HistoryEncoding,
    ) -> venera_debug_protocol::v1::RpcResult<serde_json::Value> {
        use venera_debug_protocol::v1::{Incoming, RpcError};
        let failure = |error: io::Error| RpcError::new(-32007, error.to_string());
        let value = match request {
            Incoming::HistoryRange { .. } => serde_json::to_value(self.range().map_err(failure)?),
            Incoming::HistoryRead { params, .. } => {
                if !forwarding {
                    return Err(RpcError::new(-32005, "Log forwarding is paused"));
                }
                return self.read_encoded(params, encoding).map_err(failure);
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

fn fit_history_event(
    journal_id: &str,
    boot_id: &str,
    recorded_at_ms: u64,
    startup: bool,
    event: HistoryEvent,
) -> io::Result<HistoryEvent> {
    let mut entry = HistoryRecord {
        cursor: HistoryCursor {
            journal_id: journal_id.to_owned(),
            position: DecimalU64(u64::MAX),
        },
        boot_id: boot_id.to_owned(),
        recorded_at_ms: DecimalU64(recorded_at_ms),
        startup,
        event,
    };
    if single_record_page_fits(&entry, HISTORY_PAGE_BYTES)? {
        return Ok(entry.event);
    }
    if let HistoryEvent::Log { record } = entry.event {
        entry.event = HistoryEvent::Gap {
            gap: Gap {
                stream_id: record.stream_id,
                reason: "record_size_exceeded".into(),
                count: Some(DecimalU64(1)),
                first_seq: Some(DecimalU64(record.seq)),
                last_seq: Some(DecimalU64(record.seq)),
            },
        };
        if single_record_page_fits(&entry, HISTORY_PAGE_BYTES)? {
            return Ok(entry.event);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Capture gap exceeds history page capacity",
    ))
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
        Query::AttributeProcess(boot, time, wait, mut record, tx) => {
            let _ = tx.send(
                journal
                    .attribute_process_record(&mut record, boot, time, wait)
                    .map(|status| (status, record)),
            );
        }
        #[cfg(test)]
        Query::ProcessColdReads(tx) => {
            let _ = tx.send(Ok(journal.process_cold_reads()));
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
        Query::Complete(source, diagnostic, boot, now_ms, graphics, tx) => {
            let _ = tx.send(
                journal.complete_graphics_import(&source, diagnostic, boot, now_ms, graphics),
            );
        }
        Query::GraphicsFragments(boot, tx) => {
            let _ = tx.send(journal.graphics_fragments(boot));
        }
        Query::GraphicsBoots(tx) => {
            let _ = tx.send(journal.graphics_boots());
        }
        Query::FinishGraphicsBoot(boot, now_ms, tx) => {
            let _ = tx.send(journal.finish_graphics_boot(boot, now_ms));
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
        Query::Read(request, encoding, tx) => {
            let _ = tx.send(journal.read_encoded(request, HISTORY_PAGE_BYTES, encoding));
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
    fn process_owner_small_batches_with_a_cold_checkpoint() {
        use crate::ancestry::{Attribution, Lifecycle};
        use venera_debug_protocol::ProcessIdentity;
        const BOOT: u64 = 134_393_633_875_000_000;
        const NOW: u64 = 1_794_889_800_000;
        let root = std::env::temp_dir().join(format!(
            "venera-owner-process-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let source = ImportSource {
            id: "process-etl".into(),
            boot_filetime: BOOT,
        };
        let identity = |pid: u32, at: u64| ProcessIdentity {
            pid,
            start_time_filetime: Some(at.to_string()),
            boot_time_filetime: Some(BOOT.to_string()),
            start_key: Some(at.to_string()),
            ..Default::default()
        };
        let start = |pid, at, parent| Lifecycle::Start {
            identity: identity(pid, at),
            parent,
            parent_key: None,
            session: Some(1),
        };
        let make_owner = || {
            let journal =
                Journal::open(&root, "process-pages", RetentionPolicy::default()).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (history, worker) = History::start(journal, stop.clone()).unwrap();
            (history, stop, worker)
        };
        let reads = |history: &History| {
            let (tx, rx) = mpsc::sync_channel(1);
            history
                .send(&history.queries, Query::ProcessColdReads(tx))
                .unwrap();
            history.response(rx).unwrap()
        };
        let (history, stop, worker) = make_owner();
        history.discover_imports(vec![source.clone()]).unwrap();
        let time = Instant::now();
        for base in (256..65_792).step_by(128) {
            let changes = (base..base + 128)
                .map(|pid| start(pid, BOOT + 100, if pid == 1024 { 0 } else { 1024 }))
                .collect();
            history
                .checkpoint_processes(source.clone(), changes, BOOT + 200, NOW)
                .unwrap();
        }
        let seed = time.elapsed();
        stop.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
        let (history, stop, worker) = make_owner();
        assert_eq!(reads(&history), 0);
        let record: venera_debug_protocol::LogRecord = serde_json::from_value(serde_json::json!({"stream_id":"s","seq":"1","observed_at":"original","source":"etw","component":"app","level":"info","raw_level":"info","text":"runtime","process":{"pid":65791}})).unwrap();
        let (status, record) = history
            .attribute_process_record(BOOT, BOOT + 150, true, record)
            .unwrap();
        assert_eq!(status, Attribution::Ready);
        assert_eq!(record.process, Some(identity(65_791, BOOT + 100)));
        assert_eq!(record.ancestors, [identity(1024, BOOT + 100)]);
        let query_reads = reads(&history);
        assert_eq!(query_reads, 3);
        let time = Instant::now();
        history
            .checkpoint_processes(
                source.clone(),
                vec![start(512, BOOT + 300, 1024)],
                BOOT + 400,
                NOW,
            )
            .unwrap();
        let first = time.elapsed();
        let first_reads = reads(&history) - query_reads;
        let baseline = reads(&history);
        let time = Instant::now();
        for batch in 0..128 {
            let at = BOOT + 500 + batch;
            history
                .checkpoint_processes(
                    source.clone(),
                    (512..520).map(|pid| start(pid, at, 1024)).collect(),
                    at,
                    NOW,
                )
                .unwrap();
        }
        let warm = time.elapsed();
        let warm_reads = reads(&history) - baseline;
        assert_eq!(warm_reads, 0);
        #[cfg(target_os = "linux")]
        let peak = std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|line| line.starts_with("VmHWM:"))
            .unwrap()
            .to_owned();
        #[cfg(not(target_os = "linux"))]
        let peak = "VmHWM unavailable";
        eprintln!("process-owner: 65536 instances, seed={seed:?}, query cold buckets={query_reads}, first update={first:?}/{first_reads} cold buckets, 128×8 hot={warm:?}/{warm_reads} cold buckets, {peak}");
        stop.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn history_pages_preserve_large_records_and_report_oversized_records() {
        let printable: String = (b' '..=b'~')
            .cycle()
            .take(950 * 1024)
            .map(char::from)
            .collect();
        check_large_record_round_trip(&printable, "sealed.etl".into());
    }

    #[test]
    fn history_read_round_trips_near_frame_limit_with_long_import_id() {
        check_large_record_round_trip(&"x".repeat(1_046_952), "\0".repeat(1024));
    }

    fn check_large_record_round_trip(printable: &str, source_id: String) {
        use crate::journal::ImportEntry;
        use venera_debug_protocol::v1::{PacketItem, Receiver, RpcPacket, RpcResponse};

        const BOOT: u64 = 116_444_736_000_000_000;
        let oversized = "\0".repeat(180 * 1024);
        let input = |seq: u64, text: &str| JournalInput {
            boot_id: BOOT.to_string(),
            recorded_at_ms: 1,
            startup: true,
            event: serde_json::from_value(serde_json::json!({"type":"log","record":{
                "stream_id":"large-records","seq":seq.to_string(),"observed_at":"original",
                "source":"dbwin","component":"app","level":"info","raw_level":"info","text":text
            }}))
            .unwrap(),
        };

        for mode in ["append", "durable", "import"] {
            let root = std::env::temp_dir().join(format!(
                "venera-page-{mode}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut journal =
                Journal::open(&root, "page-test", RetentionPolicy::default()).unwrap();
            journal.process_capture(BOOT, BOOT, None).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (history, worker) = History::start(journal, stop.clone()).unwrap();

            let source = ImportSource {
                id: source_id.clone(),
                boot_filetime: BOOT,
            };
            history.discover_imports(vec![source.clone()]).unwrap();
            let import = |first: u64, entries: &[(u64, &str)]| {
                let through = entries.last().unwrap().0;
                let receipt = history
                    .import_batch(ImportBatch {
                        source: source.clone(),
                        through,
                        complete: false,
                        now_ms: 1,
                        entries: entries
                            .iter()
                            .map(|&(ordinal, text)| ImportEntry {
                                ordinal,
                                source_filetime: BOOT + 10_000 * first,
                                event: input(ordinal, text).event,
                            })
                            .collect(),
                    })
                    .unwrap();
                assert_eq!(receipt.through, through);
            };
            let items = [
                (1, "before"),
                (2, printable),
                (3, oversized.as_str()),
                (4, "after"),
            ];
            if mode == "import" {
                import(1, &items[..1]);
                import(2, &items[1..2]);
                import(3, &items[2..]);
                import(3, &items[2..]); // A retried source receipt does not emit another gap.
            } else {
                for (seq, text) in items {
                    if mode == "append" {
                        history.append(input(seq, text)).unwrap();
                    } else {
                        history.append_durable(input(seq, text)).unwrap();
                    }
                }
            }

            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let high_water = history.range().unwrap().high_water.0;
                if high_water >= 4 {
                    assert_eq!(high_water, 4, "{mode}");
                    break;
                }
                assert!(Instant::now() < deadline, "{mode} commit deadline");
                thread::sleep(Duration::from_millis(10));
            }
            let read_records = |history: &History| {
                let mut records = Vec::new();
                let mut after = None;
                while records.len() < 4 {
                    let request = serde_json::json!({
                        "jsonrpc":"2.0", "id":"page", "method":"history.read",
                        "params": HistoryRead { after, through: None, limit: 256 },
                    });
                    let mut packet =
                        RpcPacket::decode(&serde_json::to_vec(&request).unwrap(), Receiver::Guest);
                    let Some(PacketItem::Request(request)) = packet.items.pop() else {
                        panic!("expected guest history request");
                    };
                    packet
                        .batch
                        .complete_bounded(
                            request.correlation.unwrap(),
                            history.rpc(request.incoming, true),
                            MAX_FRAME_BYTES,
                        )
                        .unwrap();
                    let frame = serde_json::to_vec(&packet.batch.take_response().unwrap()).unwrap();
                    let mut response = RpcPacket::decode(&frame, Receiver::Host);
                    let Some(PacketItem::Response(RpcResponse::Success { result, .. })) =
                        response.items.pop()
                    else {
                        panic!("{mode}: expected a successful host response");
                    };
                    let page: HistoryPage = serde_json::from_value(result).unwrap();
                    assert!(!page.records.is_empty(), "{mode} read made no progress");
                    assert!(page.missing.is_empty(), "{mode}");
                    records.extend(page.records);
                    after = Some(page.next);
                }
                assert_eq!(records.len(), 4, "{mode}");
                records
            };
            let records = read_records(&history);
            for (index, expected) in [(0, "before"), (1, printable), (3, "after")] {
                let HistoryEvent::Log { record } = &records[index].event else {
                    panic!(
                        "{mode}: position {} must contain the original log",
                        index + 1
                    );
                };
                assert!(
                    record.text.as_bytes() == expected.as_bytes(),
                    "{mode}: position {} differs",
                    index + 1
                );
            }
            assert_eq!(
                serde_json::to_value(&records[2].event).unwrap(),
                serde_json::json!({
                    "type":"gap","gap":{"stream_id":"large-records","reason":"record_size_exceeded",
                    "count":"1","first_seq":"3","last_seq":"3"}
                }),
                "{mode}"
            );

            stop.store(true, Ordering::Release);
            worker.join().unwrap().unwrap();
            let journal = Journal::open(&root, "page-test", RetentionPolicy::default()).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (history, worker) = History::start(journal, stop.clone()).unwrap();
            assert!(
                read_records(&history) == records,
                "{mode}: reopened history differs"
            );
            if mode == "import" {
                assert_eq!(history.import_receipt(source).unwrap().through, 4);
            }
            stop.store(true, Ordering::Release);
            worker.join().unwrap().unwrap();
            std::fs::remove_dir_all(root).unwrap();
        }
    }

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
