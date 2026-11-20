//! Durable history in bounded, sequential JSONL segments.
//!
//! The service owner serializes access. Successful appends sync the entry, then
//! replace and sync metadata before returning its cursor. Pruning syncs removals;
//! metadata preserves the high water after the final entry expires. Every public
//! mutation completes its persistence work; `close` consumes the validated state.
//! After a mutation encounters an I/O error, reopen to recover the durable state.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use venera_debug_protocol::v1::{
    DecimalU64, HistoryAck, HistoryCursor, HistoryEvent, HistoryMissing, HistoryPage, HistoryRange,
    HistoryRead, HistoryRecord,
};

const MAX_ENTRY_BYTES: u64 = venera_debug_protocol::DEFAULT_MAX_LINE_BYTES as u64;
const MIN_BUDGET: u64 = 1024;
const SEGMENT_BYTES: u64 = 64 * 1024;
const SEGMENT_RECORDS: usize = 256;
const MAX_BATCH_BYTES: usize = 1024 * 1024;
const MAX_IMPORTS: usize = 128;

#[path = "journal_space.rs"]
mod space;

/// Identity assigned to an immutable capture artifact, with its system-header boot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSource {
    pub id: String,
    pub boot_filetime: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReceipt {
    pub source: ImportSource,
    pub through: u64,
    pub complete: bool,
    #[serde(default)]
    pub process_context_lost: u64,
}

pub struct ImportEntry {
    pub ordinal: u64,
    pub source_filetime: u64,
    pub event: HistoryEvent,
}

pub struct ImportBatch {
    pub source: ImportSource,
    /// Last examined event, including events outside the selected set.
    pub through: u64,
    pub complete: bool,
    pub entries: Vec<ImportEntry>,
    pub now_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Provenance {
    source: ImportSource,
    ordinal: u64,
    source_filetime: u64,
    #[serde(default)]
    complete: bool,
}

#[derive(Serialize, Deserialize)]
struct StoredRecord {
    #[serde(flatten)]
    record: HistoryRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<Provenance>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EventIdentity {
    Producer(u64, String, u64, u64),
    UserProcess(u64, String, u16, u32, u64, u64),
}

fn event_identity(event: &HistoryEvent) -> Option<EventIdentity> {
    let HistoryEvent::Log { record } = event else {
        return None;
    };
    let event = record.structured.as_ref()?;
    if event.provider == crate::kernel_events::PROVIDER && matches!(event.event_id, 5 | 6) {
        let venera_debug_protocol::v1::EventValue::Uint32(pid) = event.fields.get("process_id")?
        else {
            return None;
        };
        return Some(EventIdentity::UserProcess(
            event.boot_time_filetime.0,
            event.provider.clone(),
            event.event_id,
            *pid,
            event.producer_instance.0,
            event.producer_sequence.0,
        ));
    }
    Some(EventIdentity::Producer(
        event.boot_time_filetime.0,
        event.provider.clone(),
        event.producer_instance.0,
        event.producer_sequence.0,
    ))
}

/// A captured event with its boot and retention context.
pub struct JournalInput {
    pub boot_id: String,
    pub recorded_at_ms: u64,
    pub startup: bool,
    pub event: HistoryEvent,
}

struct BatchEncoding(Vec<u8>);

impl Write for BatchEncoding {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BATCH_BYTES - self.0.len() {
            return Err(request_error("journal batch exceeds 1048576 encoded bytes"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Entry {
    position: u64,
    offset: u64,
    bytes: u64,
    time: u64,
    identity: Option<EventIdentity>,
}

struct Segment {
    charge: u64,
    entries: Vec<Entry>,
    boot: String,
    startup: bool,
    bytes: u64,
}

impl Segment {
    fn expiry(&self) -> Option<u64> {
        (!self.startup)
            .then(|| self.entries.iter().map(|entry| entry.time).min())
            .flatten()
    }
}

#[derive(Clone, Debug)]
pub struct RetentionPolicy {
    pub startup_boots: usize,
    pub runtime_ms: u64,
    pub max_bytes: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            startup_boots: 3,
            runtime_ms: 5 * 60 * 60 * 1000,
            max_bytes: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Metadata {
    journal_id: String,
    high_water: u64,
    recent_boots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    imports: Vec<ImportReceipt>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    processes: BTreeMap<String, crate::ancestry::Checkpoint>,
}

impl Metadata {
    fn observe_boot(&mut self, boot: &str, retain: usize) {
        self.recent_boots.retain(|item| item != boot);
        self.recent_boots.push(boot.to_owned());
        // Windows boot identities are the system's boot FILETIME.
        if self
            .recent_boots
            .iter()
            .all(|item| item.parse::<u64>().is_ok())
        {
            self.recent_boots
                .sort_by_key(|item| item.parse::<u64>().unwrap());
        }
        let discard = self.recent_boots.len().saturating_sub(retain);
        self.recent_boots.drain(..discard);
        self.processes
            .retain(|boot, _| self.recent_boots.contains(boot));
    }
}

pub struct Journal {
    root: PathBuf,
    metadata: Metadata,
    policy: RetentionPolicy,
    healthy: bool,
    segments: BTreeMap<u64, Segment>,
    expiry: BTreeSet<(u64, u64)>,
    startup_segments: BTreeMap<String, BTreeSet<u64>>,
    disk_bytes: u64,
    unit: u64,
    metadata_size: u64,
    boots_dirty: bool,
    identities: BTreeMap<EventIdentity, usize>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn request_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl Journal {
    pub fn process_capture(
        &mut self,
        boot: u64,
        captured_through: u64,
        resumed_at: Option<u64>,
    ) -> io::Result<()> {
        self.check()?;
        let mut metadata = self.metadata.clone();
        metadata.observe_boot(&boot.to_string(), self.policy.startup_boots);
        if !metadata.recent_boots.contains(&boot.to_string()) {
            return Ok(());
        }
        let checkpoint = metadata.processes.entry(boot.to_string()).or_default();
        match resumed_at {
            Some(at) if at >= boot => checkpoint.processes.resume(at),
            Some(_) => return Err(request_error("invalid capture resume time")),
            None => checkpoint.processes.interrupt(
                checkpoint.through_filetime.max(boot).max(captured_through),
                u64::MAX,
            ),
        }
        if metadata != self.metadata {
            self.commit_metadata(metadata)?;
        }
        Ok(())
    }

    pub fn process_checkpoint(&self, boot: u64) -> io::Result<crate::ancestry::Checkpoint> {
        self.check()?;
        Ok(self
            .metadata
            .processes
            .get(&boot.to_string())
            .cloned()
            .unwrap_or_default())
    }

    pub fn checkpoint_processes(
        &mut self,
        source: &ImportSource,
        changes: Vec<crate::ancestry::Lifecycle>,
        through_filetime: u64,
        now_ms: u64,
    ) -> io::Result<u64> {
        self.import_receipt(source)?;
        let boot = source.boot_filetime.to_string();
        if !self.metadata.recent_boots.contains(&boot) {
            return Ok(0);
        }
        if changes.len() > 128 {
            return Err(request_error("process batch exceeds capacity"));
        }
        let mut metadata = self.metadata.clone();
        let checkpoint = metadata.processes.entry(boot).or_default();
        let previous_through = checkpoint.through_filetime.max(source.boot_filetime);
        checkpoint.through_filetime = checkpoint.through_filetime.max(through_filetime);
        let processes = &mut checkpoint.processes;
        let mut retired = 0;
        for change in changes {
            match &change {
                crate::ancestry::Lifecycle::Start { identity, .. } => {
                    if identity.boot_time_filetime.as_deref()
                        != Some(&source.boot_filetime.to_string())
                        || identity
                            .start_time_filetime
                            .as_ref()
                            .and_then(|v| v.parse::<u64>().ok())
                            .is_none_or(|start| start < source.boot_filetime)
                        || identity
                            .exe_path
                            .as_ref()
                            .is_some_and(|path| path.len() > 32768)
                    {
                        return Err(request_error("invalid process lifecycle identity"));
                    }
                }
                crate::ancestry::Lifecycle::End { start, end, .. }
                    if *start < source.boot_filetime || end < start =>
                {
                    return Err(request_error("invalid process lifecycle interval"));
                }
                crate::ancestry::Lifecycle::Context { time, .. }
                    if *time < source.boot_filetime =>
                {
                    return Err(request_error("invalid process context time"));
                }
                crate::ancestry::Lifecycle::Gap { from, through }
                    if *from < source.boot_filetime || through < from =>
                {
                    return Err(request_error("invalid process interruption interval"));
                }
                _ => (),
            }
            if matches!(change, crate::ancestry::Lifecycle::Discontinuity) {
                processes.interrupt(previous_through, through_filetime.max(previous_through));
            } else if !processes.apply(change) {
                retired += 1;
            }
        }
        let boundary = now_ms
            .saturating_sub(self.policy.runtime_ms)
            .saturating_mul(10_000)
            .saturating_add(116_444_736_000_000_000);
        // Captured startup ancestry remains available with the retained boot history.
        for (boot, processes) in &mut metadata.processes {
            let boot = boot.parse::<u64>().unwrap_or(0);
            processes
                .processes
                .retire_history(boot.saturating_add(120 * 10_000_000), boundary);
        }
        let limit = (MAX_ENTRY_BYTES / 2).min(self.policy.max_bytes / 4);
        while serde_json::to_vec(&metadata.processes)?.len() as u64 > limit {
            let Some((_, checkpoint)) = metadata
                .processes
                .iter_mut()
                .find(|(_, checkpoint)| checkpoint.processes.has_history())
            else {
                break;
            };
            if checkpoint.processes.evict_oldest() {
                retired += 1;
            } else {
                return Err(request_error(
                    "process interruption history exceeds capacity",
                ));
            }
        }
        if retired != 0 {
            let receipt = metadata
                .imports
                .iter_mut()
                .find(|receipt| receipt.source == *source)
                .unwrap();
            receipt.process_context_lost = receipt.process_context_lost.saturating_add(retired);
        }
        if metadata != self.metadata {
            self.commit_metadata(metadata)?;
        }
        Ok(retired)
    }
    pub fn open(
        root: impl AsRef<Path>,
        initial_journal_id: impl Into<String>,
        policy: RetentionPolicy,
    ) -> io::Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        Self::open_with_unit(
            root.as_ref(),
            initial_journal_id,
            policy,
            space::unit(root.as_ref())?,
        )
    }

    fn open_with_unit(
        root: &Path,
        initial_journal_id: impl Into<String>,
        policy: RetentionPolicy,
        unit: u64,
    ) -> io::Result<Self> {
        if unit == 0 {
            return Err(invalid("invalid allocation unit"));
        }
        if policy.max_bytes < MIN_BUDGET {
            return Err(request_error(
                "journal capacity must be at least 1024 bytes",
            ));
        }
        let root = root.to_path_buf();
        let metadata_path = root.join("metadata.json");
        let existing = metadata_path.exists();
        let metadata = if existing {
            let file = File::open(&metadata_path)?;
            if file.metadata()?.len() > MAX_ENTRY_BYTES {
                return Err(invalid("journal metadata exceeds capacity"));
            }
            serde_json::from_reader(file)?
        } else {
            Metadata {
                journal_id: initial_journal_id.into(),
                high_water: 0,
                recent_boots: Vec::new(),
                imports: Vec::new(),
                processes: BTreeMap::new(),
            }
        };
        let mut journal = Self {
            root,
            metadata,
            policy,
            healthy: true,
            segments: BTreeMap::new(),
            expiry: BTreeSet::new(),
            startup_segments: BTreeMap::new(),
            disk_bytes: 0,
            unit,
            metadata_size: if existing {
                space::charge(unit, &File::open(&metadata_path)?)?
            } else {
                0
            },
            boots_dirty: true,
            identities: BTreeMap::new(),
        };
        if journal.metadata.journal_id.is_empty() || journal.metadata.high_water == u64::MAX {
            return Err(invalid("invalid journal metadata"));
        }
        if journal.metadata.imports.len() > MAX_IMPORTS {
            return Err(invalid("journal import receipt capacity exceeded"));
        }
        let mut source_ids = BTreeSet::new();
        for receipt in &journal.metadata.imports {
            if receipt.source.id.is_empty()
                || receipt.source.id.len() > 1024
                || !source_ids.insert(&receipt.source.id)
            {
                return Err(invalid("invalid journal import receipt"));
            }
        }
        let segments = journal.segment_files()?;
        if !existing && !segments.is_empty() {
            return Err(invalid("journal segments require metadata"));
        }
        // Recovery starts with committed metadata and reconstructs complete entries.
        match fs::remove_file(journal.root.join("metadata.pending")) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        let committed = journal.metadata.high_water;
        let original_metadata = journal.metadata.clone();
        let final_segment = segments.last().map(|(position, _)| *position);
        let mut previous_position = 0;
        for (key, path) in segments {
            let mut reader = BufReader::new(File::open(&path)?);
            let mut segment = Segment {
                charge: 0,
                entries: Vec::new(),
                boot: String::new(),
                startup: false,
                bytes: 0,
            };
            loop {
                let mut bytes = Vec::new();
                let count = reader
                    .by_ref()
                    .take(MAX_ENTRY_BYTES + 1)
                    .read_until(b'\n', &mut bytes)?;
                if count == 0 {
                    break;
                }
                if count as u64 > MAX_ENTRY_BYTES {
                    return Err(invalid("journal entry exceeds frame capacity"));
                }
                if !bytes.ends_with(b"\n") {
                    if Some(key) != final_segment {
                        return Err(invalid("interrupted entry precedes the final segment"));
                    }
                    let file = OpenOptions::new().write(true).open(&path)?;
                    file.set_len(segment.bytes)?;
                    file.sync_all()?;
                    break;
                }
                let stored: StoredRecord = serde_json::from_slice(&bytes)?;
                let record = stored.record;
                let position = record.cursor.position.0;
                journal.decode_entry(position, &bytes)?;
                if position < key || position <= previous_position || position == u64::MAX {
                    return Err(invalid("recovered entries are out of sequence"));
                }
                if segment.entries.is_empty() {
                    segment.boot = record.boot_id.clone();
                    segment.startup = record.startup;
                } else if segment.boot != record.boot_id || segment.startup != record.startup {
                    return Err(invalid("segment retention group differs"));
                }
                if segment.entries.len() >= SEGMENT_RECORDS
                    || (segment.bytes > 0 && segment.bytes + count as u64 > SEGMENT_BYTES)
                {
                    return Err(invalid("segment exceeds capacity"));
                }
                segment.entries.push(Entry {
                    position,
                    offset: segment.bytes,
                    bytes: count as u64,
                    time: record.recorded_at_ms.0,
                    identity: event_identity(&record.event),
                });
                segment.bytes += count as u64;
                previous_position = position;
                if position > journal.metadata.high_water {
                    if position != journal.metadata.high_water + 1 {
                        return Err(invalid("recovered entries are out of sequence"));
                    }
                    journal.metadata.high_water = position;
                    journal.observe_boot(&record.boot_id);
                }
                if let Some(provenance) = stored.provenance {
                    if !provenance.complete
                        && (provenance.ordinal == 0
                            || provenance.source.boot_filetime.to_string() != record.boot_id
                            || provenance.source_filetime < provenance.source.boot_filetime
                            || provenance
                                .source_filetime
                                .checked_sub(116_444_736_000_000_000)
                                .map(|time| time / 10_000)
                                != Some(record.recorded_at_ms.0))
                    {
                        return Err(invalid(
                            "recovered capture provenance differs from its record",
                        ));
                    }
                    if let Some(receipt) = journal
                        .metadata
                        .imports
                        .iter_mut()
                        .find(|receipt| receipt.source == provenance.source)
                    {
                        receipt.through = receipt.through.max(provenance.ordinal);
                        receipt.complete |= provenance.complete;
                    } else if position > committed {
                        return Err(invalid("recovered import source is missing"));
                    }
                }
            }
            if segment.entries.is_empty() {
                if key <= committed {
                    return Err(invalid("durable journal entry is incomplete"));
                }
                fs::remove_file(path)?;
            } else {
                if segment.entries.last().unwrap().position > committed {
                    OpenOptions::new().write(true).open(&path)?.sync_all()?;
                }
                segment.charge = space::charge(unit, reader.get_ref())?;
                journal.insert_segment(key, segment);
            }
        }
        // A compaction becomes visible through replacement of its original segment.
        match fs::remove_file(journal.root.join("segment.pending")) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        journal.disk_bytes =
            journal.metadata_size + journal.segments.values().map(|s| s.charge).sum::<u64>();
        // Retain the original directory-wide accounting for other files in the journal root.
        for entry in fs::read_dir(&journal.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.file_name() != "metadata.json"
                && entry.path().extension().is_none_or(|ext| ext != "jsonl")
            {
                journal.disk_bytes = journal
                    .disk_bytes
                    .saturating_add(space::charge(unit, &File::open(entry.path())?)?);
            }
        }
        let discard = journal
            .metadata
            .recent_boots
            .len()
            .saturating_sub(journal.policy.startup_boots);
        journal.metadata.recent_boots.drain(..discard);
        if !existing || journal.metadata != original_metadata {
            let reserve = journal.metadata_reserve(journal.metadata_bytes()?.len() as u64)?;
            journal.make_room(reserve, committed)?;
            journal.persist_metadata()?;
        }
        journal.prune_inner(None)?;
        journal.sync_directory()?;
        Ok(journal)
    }

    fn check(&self) -> io::Result<()> {
        if self.healthy {
            Ok(())
        } else {
            Err(invalid("reopen journal after a persistence error"))
        }
    }

    fn segment_path(&self, position: u64) -> PathBuf {
        self.root.join(format!("{position:020}.jsonl"))
    }

    fn segment_files(&self) -> io::Result<Vec<(u64, PathBuf)>> {
        let mut segments = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".jsonl") {
                let position = stem
                    .parse::<u64>()
                    .map_err(|_| invalid("invalid segment name"))?;
                if position == 0 || name != format!("{position:020}.jsonl") {
                    return Err(invalid("invalid segment position"));
                }
                segments.push((position, entry.path()));
            }
        }
        segments.sort_unstable_by_key(|(position, _)| *position);
        Ok(segments)
    }

    fn entry_bytes(&self, key: u64, entry: &Entry) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut file = File::open(self.segment_path(key))?;
        file.seek(SeekFrom::Start(entry.offset))?;
        file.take(entry.bytes).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != entry.bytes {
            return Err(invalid("journal entry is incomplete"));
        }
        Ok(bytes)
    }

    fn decode_entry(&self, position: u64, bytes: &[u8]) -> io::Result<HistoryRecord> {
        if !bytes.ends_with(b"\n") || bytes[..bytes.len() - 1].contains(&b'\n') {
            return Err(invalid("invalid JSONL segment"));
        }
        let record: HistoryRecord = serde_json::from_slice(bytes)?;
        if record.cursor.position.0 != position
            || record.cursor.journal_id != self.metadata.journal_id
        {
            return Err(invalid("journal entry identity differs from its segment"));
        }
        Ok(record)
    }

    fn observe_boot(&mut self, boot: &str) {
        let previous = self.metadata.recent_boots.clone();
        self.metadata.observe_boot(boot, self.policy.startup_boots);
        self.boots_dirty |= previous != self.metadata.recent_boots;
    }

    /// Register the complete discovered header set before importing any file.
    pub fn discover_imports(
        &mut self,
        sources: Vec<ImportSource>,
    ) -> io::Result<Vec<ImportReceipt>> {
        self.check()?;
        if sources.len() > MAX_IMPORTS {
            return Err(request_error("capture inventory exceeds capacity"));
        }
        let mut metadata = self.metadata.clone();
        for source in &sources {
            if source.id.is_empty() || source.id.len() > 1024 {
                return Err(request_error("invalid capture source identity"));
            }
            if let Some(receipt) = metadata
                .imports
                .iter()
                .find(|receipt| receipt.source.id == source.id)
            {
                if receipt.source != *source {
                    return Err(request_error("capture source identity changed"));
                }
            } else {
                if metadata.imports.len() == MAX_IMPORTS {
                    return Err(request_error("capture import receipts are full"));
                }
                metadata.imports.push(ImportReceipt {
                    source: source.clone(),
                    through: 0,
                    complete: false,
                    process_context_lost: 0,
                });
            }
            if source.boot_filetime != 0 {
                metadata.observe_boot(&source.boot_filetime.to_string(), self.policy.startup_boots);
            }
        }
        self.commit_metadata(metadata)?;
        let result = self.prune_inner(None);
        if result.is_err() {
            self.healthy = false;
        }
        result?;
        sources
            .iter()
            .map(|source| self.import_receipt(source))
            .collect()
    }

    pub fn import_receipt(&self, source: &ImportSource) -> io::Result<ImportReceipt> {
        self.check()?;
        self.metadata
            .imports
            .iter()
            .find(|receipt| receipt.source == *source)
            .cloned()
            .ok_or_else(|| request_error("capture source has not been discovered"))
    }

    pub fn import_sources(&self) -> io::Result<Vec<ImportSource>> {
        self.check()?;
        Ok(self
            .metadata
            .imports
            .iter()
            .map(|receipt| receipt.source.clone())
            .collect())
    }

    pub fn complete_import(
        &mut self,
        source: &ImportSource,
        diagnostic: Option<(String, Option<u64>)>,
        boot_filetime: u64,
        now_ms: u64,
    ) -> io::Result<ImportReceipt> {
        let previous = self.import_receipt(source)?;
        if previous.complete {
            return Ok(previous);
        }
        if let Some((reason, count)) = diagnostic {
            let source_filetime = now_ms
                .checked_mul(10_000)
                .and_then(|time| time.checked_add(116_444_736_000_000_000))
                .ok_or_else(|| request_error("invalid diagnostic time"))?;
            self.append_stored(vec![(
                JournalInput {
                    boot_id: boot_filetime.to_string(),
                    recorded_at_ms: now_ms,
                    startup: source_filetime >= boot_filetime
                        && source_filetime - boot_filetime < 120 * 10_000_000,
                    event: HistoryEvent::Gap {
                        gap: venera_debug_protocol::v1::Gap {
                            stream_id: self.metadata.journal_id.clone(),
                            reason: format!("{reason}; source={}", source.id),
                            count: count.map(DecimalU64),
                            first_seq: None,
                            last_seq: None,
                        },
                    },
                },
                Some(Provenance {
                    source: source.clone(),
                    ordinal: previous.through,
                    source_filetime,
                    complete: true,
                }),
            )])?;
        }
        let mut metadata = self.metadata.clone();
        let receipt = metadata
            .imports
            .iter_mut()
            .find(|receipt| receipt.source == *source)
            .unwrap();
        receipt.complete = true;
        let result = receipt.clone();
        self.commit_metadata(metadata)?;
        Ok(result)
    }

    /// The caller supplies file identities from a complete directory scan, including active outputs.
    pub fn reconcile_imports(
        &mut self,
        present: &[String],
        boot_filetime: u64,
        now_ms: u64,
    ) -> io::Result<()> {
        self.check()?;
        let missing: Vec<_> = self
            .metadata
            .imports
            .iter()
            .filter(|receipt| {
                !present.iter().any(|id| {
                    receipt.source.id == *id || receipt.source.id.starts_with(&format!("{id}:"))
                })
            })
            .map(|receipt| receipt.source.clone())
            .collect();
        for source in missing {
            if !self.import_receipt(&source)?.complete {
                self.checkpoint_processes(
                    &source,
                    vec![crate::ancestry::Lifecycle::Discontinuity],
                    0,
                    now_ms,
                )?;
            }
            self.complete_import(
                &source,
                Some(("capture_source_missing".into(), None)),
                boot_filetime,
                now_ms,
            )?;
            self.forget_import(&source)?;
        }
        Ok(())
    }

    /// The returned progress includes synced journal records and metadata.
    pub fn import_batch(&mut self, batch: ImportBatch) -> io::Result<ImportReceipt> {
        let previous = self.import_receipt(&batch.source)?;
        if batch.entries.len() > 256 || batch.through < previous.through {
            return Err(request_error("invalid capture import progress"));
        }
        if previous.complete {
            if batch.through != previous.through {
                return Err(request_error("completed capture changed"));
            }
            return Ok(previous);
        }
        let mut last = 0;
        let mut inputs = Vec::new();
        for entry in batch.entries {
            if entry.ordinal == 0
                || entry.ordinal <= last
                || entry.ordinal > batch.through
                || entry.source_filetime < batch.source.boot_filetime
            {
                return Err(request_error("invalid capture event position or time"));
            }
            last = entry.ordinal;
            if let HistoryEvent::Log { record } = &entry.event {
                if let Some(event) = &record.structured {
                    if event.boot_time_filetime.0 != batch.source.boot_filetime
                        || event.source_time_filetime.0 != entry.source_filetime
                    {
                        return Err(request_error("capture header and event identity differ"));
                    }
                }
            }
            if entry.ordinal <= previous.through {
                continue;
            }
            let startup = entry.source_filetime - batch.source.boot_filetime < 120 * 10_000_000;
            let recorded_at_ms = entry
                .source_filetime
                .checked_sub(116_444_736_000_000_000)
                .ok_or_else(|| request_error("capture time precedes Unix epoch"))?
                / 10_000;
            let boot_id = batch.source.boot_filetime.to_string();
            if (startup && !self.metadata.recent_boots.contains(&boot_id))
                || (!startup
                    && batch.now_ms.saturating_sub(recorded_at_ms) > self.policy.runtime_ms)
            {
                continue;
            }
            inputs.push((
                JournalInput {
                    boot_id,
                    recorded_at_ms,
                    startup,
                    event: entry.event,
                },
                Some(Provenance {
                    source: batch.source.clone(),
                    ordinal: entry.ordinal,
                    source_filetime: entry.source_filetime,
                    complete: false,
                }),
            ));
        }
        // All input validation precedes the first durable mutation.
        self.append_stored(inputs)?;
        let mut metadata = self.metadata.clone();
        let receipt = metadata
            .imports
            .iter_mut()
            .find(|receipt| receipt.source == batch.source)
            .unwrap();
        receipt.through = batch.through;
        receipt.complete = batch.complete;
        let receipt = receipt.clone();
        self.commit_metadata(metadata)?;
        Ok(receipt)
    }

    /// Release a completed receipt after its source artifact has been removed.
    pub fn forget_import(&mut self, source: &ImportSource) -> io::Result<()> {
        if !self.import_receipt(source)?.complete {
            return Err(request_error("capture import is still in progress"));
        }
        let mut metadata = self.metadata.clone();
        metadata.imports.retain(|receipt| receipt.source != *source);
        self.commit_metadata(metadata)
    }

    fn commit_metadata(&mut self, metadata: Metadata) -> io::Result<()> {
        self.check()?;
        let reserve = self.metadata_reserve(serde_json::to_vec(&metadata)?.len() as u64)?;
        self.boots_dirty |= self.metadata.recent_boots != metadata.recent_boots;
        self.metadata = metadata;
        let result = (|| {
            self.make_room(reserve, self.metadata.high_water)?;
            self.persist_metadata()
        })();
        if result.is_err() {
            self.healthy = false;
        }
        result
    }

    fn metadata_bytes(&self) -> io::Result<Vec<u8>> {
        Ok(serde_json::to_vec(&self.metadata)?)
    }

    fn metadata_reserve(&self, len: u64) -> io::Result<u64> {
        let reserve = space::rounded(self.unit, len);
        if len > MAX_ENTRY_BYTES
            || reserve + self.metadata_size.max(reserve) > self.policy.max_bytes
        {
            return Err(request_error(
                "journal capacity is too small for metadata replacement",
            ));
        }
        Ok(reserve)
    }

    fn persist_metadata(&mut self) -> io::Result<()> {
        let bytes = self.metadata_bytes()?;
        let reserve = self.metadata_reserve(bytes.len() as u64)?;
        if self.disk_bytes.saturating_add(reserve) > self.policy.max_bytes {
            return Err(request_error(
                "journal capacity is too small for metadata replacement",
            ));
        }
        let pending = self.root.join("metadata.pending");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&pending)?;
        space::prepare(&file, self.unit, bytes.len() as u64)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let charge = space::charge(self.unit, &file)?;
        drop(file);
        #[cfg(test)]
        tests::shared_budget::observe(&pending, &self.root.join("metadata.json"));
        replace_metadata(&pending, &self.root.join("metadata.json"))?;
        self.sync_directory()?;
        self.disk_bytes = self.disk_bytes - self.metadata_size + charge;
        self.metadata_size = charge;
        Ok(())
    }

    fn sync_directory(&self) -> io::Result<()> {
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    pub fn append(
        &mut self,
        boot_id: &str,
        recorded_at_ms: u64,
        startup: bool,
        event: HistoryEvent,
    ) -> io::Result<HistoryRecord> {
        Ok(self
            .append_batch(vec![JournalInput {
                boot_id: boot_id.to_owned(),
                recorded_at_ms,
                startup,
                event,
            }])?
            .into_iter()
            .next()
            .ok_or_else(|| request_error("event already retained"))?)
    }

    /// Commits a capture batch and returns its durable records in input order.
    pub fn append_batch(&mut self, entries: Vec<JournalInput>) -> io::Result<Vec<HistoryRecord>> {
        self.append_stored(entries.into_iter().map(|entry| (entry, None)).collect())
    }

    fn append_stored(
        &mut self,
        entries: Vec<(JournalInput, Option<Provenance>)>,
    ) -> io::Result<Vec<HistoryRecord>> {
        self.check()?;
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        if entries.len() > 256 {
            return Err(request_error("journal batch exceeds 256 records"));
        }
        self.metadata
            .high_water
            .checked_add(entries.len() as u64)
            .filter(|value| *value < u64::MAX)
            .ok_or_else(|| request_error("journal positions exhausted"))?;
        let mut records = Vec::with_capacity(entries.len());
        let mut offsets = Vec::with_capacity(entries.len());
        let mut encoded = BatchEncoding(Vec::with_capacity(MAX_BATCH_BYTES));
        let mut metadata = self.metadata.clone();
        let mut now = 0;
        let mut identities = BTreeSet::new();
        for (mut input, provenance) in entries {
            if let HistoryEvent::Log { record } = &input.event {
                if let Some(event) = &record.structured {
                    let boot = event.boot_time_filetime.0;
                    let time = event.source_time_filetime.0;
                    if time < boot || boot == 0 || time < 116_444_736_000_000_000 {
                        return Err(request_error("invalid structured event boot or time"));
                    }
                    input.boot_id = boot.to_string();
                    input.recorded_at_ms = (time - 116_444_736_000_000_000) / 10_000;
                    input.startup = time - boot < 120 * 10_000_000;
                }
            }
            if let Some(identity) = event_identity(&input.event) {
                if self.identities.contains_key(&identity) || !identities.insert(identity) {
                    continue;
                }
            }
            let position = metadata.high_water + 1;
            let record = HistoryRecord {
                cursor: HistoryCursor {
                    journal_id: metadata.journal_id.clone(),
                    position: DecimalU64(position),
                },
                boot_id: input.boot_id,
                recorded_at_ms: DecimalU64(input.recorded_at_ms),
                startup: input.startup,
                event: input.event,
            };
            let offset = encoded.0.len();
            #[derive(Serialize)]
            struct Stored<'a> {
                #[serde(flatten)]
                record: &'a HistoryRecord,
                #[serde(skip_serializing_if = "Option::is_none")]
                provenance: Option<Provenance>,
            }
            serde_json::to_writer(
                &mut encoded,
                &Stored {
                    record: &record,
                    provenance,
                },
            )
            .map_err(|error| request_error(&error.to_string()))?;
            encoded.write_all(b"\n")?;
            let length = encoded.0.len() - offset;
            // Retained records share their page envelope with advancing range cursors.
            let page = HistoryPage {
                range: HistoryRange {
                    journal_id: metadata.journal_id.clone(),
                    oldest: DecimalU64(u64::MAX),
                    high_water: DecimalU64(u64::MAX - 1),
                },
                records: Vec::new(),
                missing: Vec::new(),
                next: HistoryCursor {
                    journal_id: metadata.journal_id.clone(),
                    position: DecimalU64(u64::MAX - 1),
                },
            };
            if length as u64 > MAX_ENTRY_BYTES
                || serde_json::to_vec(&page)?.len() + length - 1 > MAX_ENTRY_BYTES as usize - 256
            {
                return Err(request_error("journal entry exceeds history page capacity"));
            }
            metadata.observe_boot(&record.boot_id, self.policy.startup_boots);
            metadata.high_water = position;
            now = now.max(input.recorded_at_ms);
            offsets.push((offset, length));
            records.push(record);
        }
        if records.is_empty() {
            return Ok(records);
        }
        if records.len() > 1
            && records.iter().any(|record| {
                if record.startup {
                    !metadata.recent_boots.contains(&record.boot_id)
                } else {
                    now.saturating_sub(record.recorded_at_ms.0) > self.policy.runtime_ms
                }
            })
        {
            return Err(request_error(
                "journal batch spans retention windows; split the batch",
            ));
        }
        let metadata_charge = self.metadata_reserve(serde_json::to_vec(&metadata)?.len() as u64)?;
        let limit = SEGMENT_BYTES.min(self.policy.max_bytes / 4);
        let unit = self.unit;
        let plan = |tail: Option<&Segment>| {
            let mut groups = Vec::new();
            let mut first = 0;
            let mut charge = metadata_charge;
            while first < records.len() {
                let base = tail.filter(|_| first == 0);
                let base_bytes = base.map_or(0, |s| s.bytes);
                let count = base.map_or(0, |s| s.entries.len());
                let mut end = first + 1;
                let mut size = offsets[first].1 as u64;
                while end < records.len()
                    && count + end - first < SEGMENT_RECORDS
                    && records[first].boot_id == records[end].boot_id
                    && records[first].startup == records[end].startup
                    && base_bytes + size + offsets[end].1 as u64 <= limit
                {
                    size += offsets[end].1 as u64;
                    end += 1;
                }
                groups.push((first, end, size));
                charge += space::rounded(unit, base_bytes + size)
                    .saturating_sub(base.map_or(0, |s| s.charge));
                first = end;
            }
            (groups, charge)
        };
        let fresh = plan(None);
        if fresh.1 + self.metadata_size > self.policy.max_bytes {
            return Err(request_error("journal batch exceeds available capacity"));
        }
        let committed = self.metadata.high_water;
        self.boots_dirty |= self.metadata.recent_boots != metadata.recent_boots;
        self.metadata = metadata;
        let result = (|| {
            self.prune_inner(Some(now))?;
            let tail = self.segments.last_key_value().filter(|(_, s)| {
                s.boot == records[0].boot_id
                    && s.startup == records[0].startup
                    && s.entries.len() < SEGMENT_RECORDS
                    && s.bytes + offsets[0].1 as u64 <= limit
            });
            let (groups, victims, tail) = if let Some((&key, segment)) = tail {
                let (groups, reserve) = plan(Some(segment));
                match self.evictions(reserve, committed) {
                    Ok(victims) if !victims.contains(&key) => (groups, victims, Some(key)),
                    // Repack the whole batch before retiring its planned append target.
                    _ => (fresh.0, self.evictions(fresh.1, committed)?, None),
                }
            } else {
                (fresh.0, self.evictions(fresh.1, committed)?, None)
            };
            for key in victims {
                self.remove_segment(key)?;
            }
            for (first, end, size) in groups {
                let record = &records[first];
                let position = record.cursor.position.0;
                let key = tail.filter(|_| first == 0).unwrap_or(position);
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(key == position)
                    .open(self.segment_path(key))?;
                let mut segment = self.take_segment(key).unwrap_or(Segment {
                    charge: 0,
                    entries: Vec::new(),
                    boot: record.boot_id.clone(),
                    startup: record.startup,
                    bytes: 0,
                });
                file.seek(SeekFrom::End(0))?;
                space::prepare(&file, self.unit, segment.bytes + size)?;
                let offset = offsets[first].0;
                file.write_all(&encoded.0[offset..offset + size as usize])?;
                file.sync_all()?;
                self.sync_directory()?;
                self.disk_bytes -= segment.charge;
                segment.charge = space::charge(self.unit, &file)?;
                self.disk_bytes += segment.charge;
                for (record, &(_, length)) in records[first..end].iter().zip(&offsets[first..end]) {
                    segment.entries.push(Entry {
                        position: record.cursor.position.0,
                        offset: segment.bytes,
                        bytes: length as u64,
                        time: record.recorded_at_ms.0,
                        identity: event_identity(&record.event),
                    });
                    segment.bytes += length as u64;
                }
                self.insert_segment(key, segment);
            }
            self.persist_metadata()?;
            Ok(())
        })();
        if result.is_err() {
            self.healthy = false;
        }
        result.map(|()| records)
    }

    pub fn range(&self) -> io::Result<HistoryRange> {
        self.check()?;
        let oldest = self
            .segments
            .first_key_value()
            .map(|(_, segment)| segment.entries[0].position)
            .unwrap_or(self.metadata.high_water + 1);
        Ok(HistoryRange {
            journal_id: self.metadata.journal_id.clone(),
            oldest: DecimalU64(oldest),
            high_water: DecimalU64(self.metadata.high_water),
        })
    }

    fn validate_cursor(&self, cursor: &HistoryCursor) -> io::Result<()> {
        self.check()?;
        if cursor.journal_id != self.metadata.journal_id
            || cursor.position.0 > self.metadata.high_water
        {
            return Err(request_error(
                "cursor is outside this journal's durable history",
            ));
        }
        Ok(())
    }

    pub fn ack(&self, ack: HistoryAck) -> io::Result<HistoryAck> {
        self.validate_cursor(&ack.cursor)?;
        Ok(ack)
    }

    pub fn read(&self, request: HistoryRead, byte_budget: usize) -> io::Result<HistoryPage> {
        self.check()?;
        if !(1..=256).contains(&request.limit) {
            return Err(request_error("history limit must be between 1 and 256"));
        }
        if let Some(after) = &request.after {
            self.validate_cursor(after)?;
        }
        let after = request.after.as_ref().map_or(0, |cursor| cursor.position.0);
        let through = request
            .through
            .map_or(self.metadata.high_water, |position| position.0);
        if through < after || through > self.metadata.high_water {
            return Err(request_error("invalid history range"));
        }
        let budget = byte_budget.min(MAX_ENTRY_BYTES as usize - 256);
        let mut page = HistoryPage {
            range: self.range()?,
            records: Vec::new(),
            missing: Vec::new(),
            next: HistoryCursor {
                journal_id: self.metadata.journal_id.clone(),
                position: DecimalU64(after),
            },
        };
        if serde_json::to_vec(&page)?.len() > budget {
            return Err(request_error("history byte budget is too small for a page"));
        }
        'segments: for (&key, segment) in &self.segments {
            for entry in &segment.entries {
                let position = entry.position;
                if position <= after {
                    continue;
                }
                if position > through {
                    break 'segments;
                }
                if position > page.next.position.0 + 1 {
                    let mut candidate = page.clone();
                    Self::push_missing(&mut candidate, position - 1);
                    if !Self::fits(&candidate, budget)? {
                        return self.finish_page(page, after);
                    }
                    page = candidate;
                }
                if page.records.len() == request.limit as usize {
                    return Ok(page);
                }
                // Inspect length before allocating a record, then decode at most one frame.
                if entry.bytes as usize > budget {
                    return self.finish_page(page, after);
                }
                let record = self.decode_entry(position, &self.entry_bytes(key, entry)?)?;
                let mut candidate = page.clone();
                candidate.records.push(record);
                candidate.next.position = DecimalU64(position);
                if !Self::fits(&candidate, budget)? {
                    return self.finish_page(page, after);
                }
                page = candidate;
            }
        }
        if page.next.position.0 < through {
            let mut candidate = page.clone();
            Self::push_missing(&mut candidate, through);
            if Self::fits(&candidate, budget)? {
                page = candidate;
            } else {
                return self.finish_page(page, after);
            }
        }
        Ok(page)
    }

    fn push_missing(page: &mut HistoryPage, last: u64) {
        page.missing.push(HistoryMissing {
            first: DecimalU64(page.next.position.0 + 1),
            last: DecimalU64(last),
            reason: "retention_expired".to_owned(),
        });
        page.next.position = DecimalU64(last);
    }

    fn fits(page: &HistoryPage, budget: usize) -> io::Result<bool> {
        Ok(serde_json::to_vec(page)?.len() <= budget)
    }

    fn finish_page(&self, page: HistoryPage, after: u64) -> io::Result<HistoryPage> {
        if page.next.position.0 == after {
            Err(request_error(
                "history byte budget is too small for the next entry",
            ))
        } else {
            Ok(page)
        }
    }

    pub fn prune(&mut self, now_ms: u64) -> io::Result<()> {
        self.check()?;
        let result = self.prune_inner(Some(now_ms));
        if result.is_err() {
            self.healthy = false;
        }
        result
    }

    fn insert_segment(&mut self, key: u64, segment: Segment) {
        for entry in &segment.entries {
            if let Some(identity) = &entry.identity {
                *self.identities.entry(identity.clone()).or_default() += 1;
            }
        }
        if segment.startup {
            self.boots_dirty |= !self.metadata.recent_boots.contains(&segment.boot);
            self.startup_segments
                .entry(segment.boot.clone())
                .or_default()
                .insert(key);
        }
        if let Some(time) = segment.expiry() {
            self.expiry.insert((time, key));
        }
        self.segments.insert(key, segment);
    }

    fn take_segment(&mut self, key: u64) -> Option<Segment> {
        let segment = self.segments.remove(&key)?;
        for entry in &segment.entries {
            if let Some(identity) = &entry.identity {
                let count = self.identities.get_mut(identity).unwrap();
                *count -= 1;
                if *count == 0 {
                    self.identities.remove(identity);
                }
            }
        }
        if segment.startup {
            let keys = self.startup_segments.get_mut(&segment.boot).unwrap();
            keys.remove(&key);
            if keys.is_empty() {
                self.startup_segments.remove(&segment.boot);
            }
        }
        if let Some(time) = segment.expiry() {
            self.expiry.remove(&(time, key));
        }
        Some(segment)
    }

    fn remove_segment(&mut self, key: u64) -> io::Result<()> {
        fs::remove_file(self.segment_path(key))?;
        let segment = self
            .take_segment(key)
            .ok_or_else(|| invalid("segment index missing"))?;
        self.disk_bytes -= segment.charge;
        self.sync_directory()
    }

    fn make_room(&mut self, reserve: u64, committed: u64) -> io::Result<()> {
        for key in self.evictions(reserve, committed)? {
            self.remove_segment(key)?;
        }
        Ok(())
    }

    fn evictions(&self, reserve: u64, committed: u64) -> io::Result<Vec<u64>> {
        let mut need = self
            .disk_bytes
            .saturating_add(reserve)
            .saturating_sub(self.policy.max_bytes);
        let mut victims = Vec::new();
        if need == 0 {
            return Ok(victims);
        }
        let retained = |boot: &String| self.metadata.recent_boots.contains(boot);
        let expired = self
            .startup_segments
            .iter()
            .filter(|(b, _)| !retained(b))
            .flat_map(|(_, keys)| keys.iter().copied());
        let runtime = self.expiry.iter().map(|&(_, key)| key);
        let startup = self
            .segments
            .iter()
            .filter(|(_, s)| s.startup && retained(&s.boot))
            .map(|(&key, _)| key);
        for key in expired.chain(runtime).chain(startup) {
            let s = &self.segments[&key];
            if s.entries.last().unwrap().position <= committed {
                victims.push(key);
                need = need.saturating_sub(s.charge);
                if need == 0 {
                    return Ok(victims);
                }
            }
        }
        Err(request_error(
            "journal capacity is too small for pending data",
        ))
    }

    fn prune_inner(&mut self, now_ms: Option<u64>) -> io::Result<()> {
        if self.boots_dirty {
            let expired: Vec<_> = self
                .startup_segments
                .iter()
                .filter(|(boot, _)| !self.metadata.recent_boots.contains(boot))
                .flat_map(|(_, keys)| keys.iter().copied())
                .collect();
            for key in expired {
                self.remove_segment(key)?;
            }
            self.boots_dirty = false;
        }
        if let Some(now) = now_ms {
            while let Some(&(time, key)) = self.expiry.first() {
                if now.saturating_sub(time) <= self.policy.runtime_ms {
                    break;
                }
                let segment = &self.segments[&key];
                let survivors: Vec<_> = segment
                    .entries
                    .iter()
                    .filter(|entry| now.saturating_sub(entry.time) <= self.policy.runtime_ms)
                    .collect();
                if survivors.is_empty() {
                    self.remove_segment(key)?;
                    continue;
                }
                let mut bytes = Vec::new();
                let mut replacement = Segment {
                    charge: 0,
                    entries: Vec::new(),
                    boot: segment.boot.clone(),
                    startup: false,
                    bytes: 0,
                };
                for entry in survivors {
                    let data = self.entry_bytes(key, entry)?;
                    replacement.entries.push(Entry {
                        position: entry.position,
                        offset: replacement.bytes,
                        bytes: entry.bytes,
                        time: entry.time,
                        identity: entry.identity.clone(),
                    });
                    replacement.bytes += entry.bytes;
                    bytes.extend_from_slice(&data);
                }
                // Reserve compaction alongside the original before writing its replacement.
                self.make_room(
                    space::rounded(self.unit, replacement.bytes),
                    self.metadata.high_water,
                )?;
                if !self.segments.contains_key(&key) {
                    continue;
                }
                let pending = self.root.join("segment.pending");
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&pending)?;
                space::prepare(&file, self.unit, replacement.bytes)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                replacement.charge = space::charge(self.unit, &file)?;
                drop(file);
                #[cfg(test)]
                tests::shared_budget::observe(&pending, &self.segment_path(key));
                replace_metadata(&pending, &self.segment_path(key))?;
                self.sync_directory()?;
                let original = self.take_segment(key).unwrap();
                self.disk_bytes = self.disk_bytes - original.charge + replacement.charge;
                self.insert_segment(key, replacement);
            }
        }
        self.make_room(0, self.metadata.high_water)
    }

    #[cfg(test)]
    fn measure_disk_bytes(&self) -> io::Result<u64> {
        let mut total = 0u64;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
        Ok(total)
    }

    pub fn close(self) -> io::Result<()> {
        self.check()
    }
}

#[cfg(not(windows))]
fn replace_metadata(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace_metadata(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // Both buffers remain terminated and alive throughout the synchronous call.
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicU64, Ordering};
    use venera_debug_protocol::v1::Gap;

    mod imports {
        use super::*;
        include!("journal_import_tests.rs");
    }

    pub(super) mod shared_budget {
        use super::*;
        include!("journal_shared_budget_tests.rs");
    }

    mod processes {
        use super::*;
        include!("journal_process_tests.rs");
    }

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "venera-journal-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        // Byte-sized fixtures exercise retention transitions with a one-byte allocation unit.
        fn open_logical(&self, policy: RetentionPolicy) -> Journal {
            Journal::open_with_unit(&self.0, "journal-a", policy, 1).unwrap()
        }
        fn open(&self, policy: RetentionPolicy) -> Journal {
            Journal::open(&self.0, "journal-a", policy).unwrap()
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn event(text: &str) -> HistoryEvent {
        HistoryEvent::Gap {
            gap: Gap {
                stream_id: "capture-a".into(),
                reason: text.into(),
                count: Some(DecimalU64(2)),
                first_seq: Some(DecimalU64(7)),
                last_seq: Some(DecimalU64(8)),
            },
        }
    }

    fn input(boot: &str, time: u64, startup: bool, text: &str) -> JournalInput {
        JournalInput {
            boot_id: boot.into(),
            recorded_at_ms: time,
            startup,
            event: event(text),
        }
    }

    fn read(
        journal: &Journal,
        after: u64,
        through: Option<u64>,
        limit: u32,
        budget: usize,
    ) -> io::Result<HistoryPage> {
        journal.read(
            HistoryRead {
                after: Some(HistoryCursor {
                    journal_id: "journal-a".into(),
                    position: DecimalU64(after),
                }),
                through: through.map(DecimalU64),
                limit,
            },
            budget,
        )
    }

    fn summary(page: HistoryPage) -> Value {
        json!({
            "oldest": page.range.oldest.0, "high": page.range.high_water.0,
            "positions": page.records.iter().map(|record| record.cursor.position.0).collect::<Vec<_>>(),
            "missing": page.missing, "next": page.next.position.0,
        })
    }

    #[test]
    fn capture_survives_close_and_service_identity_change() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        let record = journal
            .append("boot-a", 100, true, event("capture_gap"))
            .unwrap();
        assert_eq!(
            serde_json::to_value(record).unwrap(),
            json!({
                "cursor": {"journal_id":"journal-a", "position":"1"},
                "boot_id":"boot-a", "recorded_at_ms":"100", "startup":true,
                "event":{"type":"gap", "gap":{"stream_id":"capture-a", "reason":"capture_gap",
                    "count":"2", "first_seq":"7", "last_seq":"8"}}
            })
        );
        journal.close().unwrap();
        let mut reopened =
            Journal::open(&directory.0, "service-b", RetentionPolicy::default()).unwrap();
        let page = read(&reopened, 0, None, 256, 4096).unwrap();
        assert_eq!(page.records[0].boot_id, "boot-a");
        assert_eq!(page.records[0].cursor.journal_id, "journal-a");
        assert_eq!(
            summary(page),
            json!({"oldest":1,"high":1,"positions":[1],"missing":[],"next":1})
        );
        assert_eq!(
            reopened
                .append("boot-a", 110, false, event("next"))
                .unwrap()
                .cursor
                .position
                .0,
            2
        );
    }

    #[test]
    fn boot_groups_and_runtime_expiry_preserve_startup_and_interior_holes() {
        let directory = Directory::new();
        let policy = RetentionPolicy {
            startup_boots: 2,
            runtime_ms: 10,
            max_bytes: 100_000,
        };
        let mut journal = directory.open(policy.clone());
        journal.append("a", 0, true, event("startup-a")).unwrap();
        journal.append("a", 1, false, event("runtime-a")).unwrap();
        journal.close().unwrap();
        let mut journal = directory.open(policy);
        journal.append("a", 2, true, event("same-boot")).unwrap();
        journal.append("b", 3, true, event("startup-b")).unwrap();
        journal.prune(12).unwrap();
        assert_eq!(
            summary(read(&journal, 0, None, 256, 8192).unwrap()),
            json!({
                "oldest":1,"high":4,"positions":[1,3,4],
                "missing":[{"first":"2","last":"2","reason":"retention_expired"}],"next":4
            })
        );
        journal.append("c", 13, true, event("startup-c")).unwrap();
        assert_eq!(
            summary(read(&journal, 0, None, 256, 8192).unwrap()),
            json!({
                "oldest":4,"high":5,"positions":[4,5],
                "missing":[{"first":"1","last":"3","reason":"retention_expired"}],"next":5
            })
        );
    }

    #[test]
    fn late_boot_import_keeps_system_boot_order_across_recovery() {
        for recover_tail in [false, true] {
            let directory = Directory::new();
            let mut journal = directory.open(RetentionPolicy::default());
            for boot in ["90", "100", "110", "90"] {
                journal.append(boot, 0, true, event("startup")).unwrap();
            }
            journal.close().unwrap();
            if recover_tail {
                fs::write(
                    directory.0.join("metadata.json"),
                    br#"{"journal_id":"journal-a","high_water":3,"recent_boots":["90","100","110"]}"#,
                )
                .unwrap();
            }
            let mut journal = directory.open(RetentionPolicy::default());
            journal.append("120", 0, true, event("next boot")).unwrap();
            let page = read(&journal, 0, None, 256, 8192).unwrap();
            assert_eq!(
                page.records
                    .iter()
                    .map(|record| record.boot_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["100", "110", "120"],
            );
            assert_eq!(
                summary(page),
                json!({"oldest":2,"high":5,"positions":[2,3,5],
                    "missing":[{"first":"1","last":"1","reason":"retention_expired"},
                               {"first":"4","last":"4","reason":"retention_expired"}],"next":5}),
            );
        }
    }

    #[test]
    fn empty_retention_keeps_high_water_and_the_next_append_identity() {
        let directory = Directory::new();
        let policy = RetentionPolicy {
            startup_boots: 0,
            runtime_ms: 10,
            max_bytes: 4096,
        };
        let mut journal = directory.open_logical(policy.clone());
        journal.append("a", 0, false, event("old")).unwrap();
        journal.prune(11).unwrap();
        journal.close().unwrap();
        let mut journal = directory.open_logical(policy);
        assert_eq!(
            summary(read(&journal, 0, None, 1, 1024).unwrap()),
            json!({
                "oldest":2,"high":1,"positions":[],
                "missing":[{"first":"1","last":"1","reason":"retention_expired"}],"next":1
            })
        );
        assert_eq!(
            journal
                .append("a", 12, false, event("new"))
                .unwrap()
                .cursor
                .position
                .0,
            2
        );
        assert_eq!(
            summary(read(&journal, 2, None, 1, 1024).unwrap()),
            json!({
                "oldest":2,"high":2,"positions":[],"missing":[],"next":2
            })
        );
    }

    #[test]
    fn hard_capacity_evicts_old_startup_after_time_cleanup_creates_a_hole() {
        let directory = Directory::new();
        let policy = RetentionPolicy {
            startup_boots: 3,
            runtime_ms: 5,
            max_bytes: 1200,
        };
        let mut journal = directory.open_logical(policy);
        journal
            .append("a", 0, true, event(&"a".repeat(200)))
            .unwrap();
        journal
            .append("a", 1, false, event(&"b".repeat(200)))
            .unwrap();
        journal
            .append("a", 7, true, event(&"c".repeat(200)))
            .unwrap();
        assert_eq!(
            summary(read(&journal, 0, None, 256, 8192).unwrap()),
            json!({
                "oldest":1,"high":3,"positions":[1,3],
                "missing":[{"first":"2","last":"2","reason":"retention_expired"}],"next":3
            })
        );
        journal
            .append("a", 8, true, event(&"d".repeat(200)))
            .unwrap();
        let total: u64 = fs::read_dir(&directory.0)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= 1200, "actual journal bytes: {total}");
        assert_eq!(
            summary(read(&journal, 0, None, 256, 8192).unwrap()),
            json!({
                "oldest":3,"high":4,"positions":[3,4],
                "missing":[{"first":"1","last":"2","reason":"retention_expired"}],"next":4
            })
        );
        assert_eq!(
            journal
                .append("a", 9, false, event(&"x".repeat(2000)))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(journal.range().unwrap().high_water.0, 4);
    }

    #[test]
    fn interrupted_tail_recovers_complete_entries_and_ignores_pending_metadata() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        journal.append("a", 0, true, event("first")).unwrap();
        journal.append("a", 1, true, event("second")).unwrap();
        journal.close().unwrap();
        // Model an entry sync followed by interruption before metadata replacement.
        fs::write(
            directory.0.join("metadata.json"),
            br#"{"journal_id":"journal-a","high_water":1,"recent_boots":["a"]}"#,
        )
        .unwrap();
        fs::write(directory.0.join("metadata.pending"), b"{interrupted").unwrap();
        fs::write(directory.0.join("00000000000000000003.jsonl"), b"{partial").unwrap();
        let mut journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({
                "oldest":1,"high":2,"positions":[1,2],"missing":[],"next":2
            })
        );
        assert_eq!(
            journal
                .append("a", 2, false, event("third"))
                .unwrap()
                .cursor
                .position
                .0,
            3
        );
        journal.close().unwrap();
        assert_eq!(
            directory
                .open(RetentionPolicy::default())
                .range()
                .unwrap()
                .high_water
                .0,
            3
        );
    }

    #[test]
    fn earlier_malformed_content_is_reported() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        journal.append("a", 0, true, event("first")).unwrap();
        journal.append("a", 1, true, event("second")).unwrap();
        journal.close().unwrap();
        fs::write(
            directory.0.join("00000000000000000001.jsonl"),
            b"{broken}\n",
        )
        .unwrap();
        let result = Journal::open(&directory.0, "other", RetentionPolicy::default());
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn final_complete_line_is_preserved_when_an_interrupted_suffix_is_truncated() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        journal.append("a", 0, true, event("first")).unwrap();
        journal.close().unwrap();
        let tail = directory.0.join("00000000000000000001.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&tail)
            .unwrap()
            .write_all(b"{partial")
            .unwrap();
        let journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({
                "oldest":1,"high":1,"positions":[1],"missing":[],"next":1
            })
        );
        assert!(fs::read(tail).unwrap().ends_with(b"\n"));
    }

    #[test]
    fn metadata_write_failure_requires_recovery_and_preserves_the_synced_entry() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        fs::create_dir(directory.0.join("metadata.pending")).unwrap();
        assert!(journal.append("a", 0, true, event("synced")).is_err());
        assert_eq!(
            journal.range().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            journal.close().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_dir(directory.0.join("metadata.pending")).unwrap();
        let mut journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            summary(
                journal
                    .read(
                        HistoryRead {
                            after: None,
                            through: None,
                            limit: 256
                        },
                        4096
                    )
                    .unwrap()
            ),
            json!({
                "oldest":1,"high":1,"positions":[1],"missing":[],"next":1
            })
        );
        assert_eq!(
            journal
                .append("a", 1, false, event("next"))
                .unwrap()
                .cursor
                .position
                .0,
            2
        );
    }

    #[test]
    fn pagination_is_bounded_and_readers_replay_the_same_envelope() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        for timestamp in 0..3 {
            journal
                .append("a", timestamp, true, event("entry"))
                .unwrap();
        }
        let first = read(&journal, 0, Some(3), 1, 4096).unwrap();
        assert_eq!(
            summary(first.clone()),
            json!({"oldest":1,"high":3,"positions":[1],"missing":[],"next":1})
        );
        assert_eq!(
            summary(read(&journal, 1, Some(3), 1, 4096).unwrap()),
            json!({
                "oldest":1,"high":3,"positions":[2],"missing":[],"next":2
            })
        );
        let bytes = serde_json::to_vec(&first).unwrap().len();
        assert_eq!(
            summary(read(&journal, 0, None, 256, bytes).unwrap()),
            json!({
                "oldest":1,"high":3,"positions":[1],"missing":[],"next":1
            })
        );
        assert_eq!(
            serde_json::to_value(read(&journal, 0, Some(3), 1, 4096).unwrap()).unwrap(),
            json!({
                "range":{"journal_id":"journal-a","oldest":"1","high_water":"3"},
                "records":[{"cursor":{"journal_id":"journal-a","position":"1"},"boot_id":"a",
                    "recorded_at_ms":"0","startup":true,"event":{"type":"gap","gap":{
                        "stream_id":"capture-a","reason":"entry","count":"2","first_seq":"7","last_seq":"8"}}}],
                "missing":[],"next":{"journal_id":"journal-a","position":"1"}
            })
        );
        assert_eq!(
            read(&journal, 0, None, 1, bytes - 1).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            read(&journal, 0, None, 0, 4096).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            read(&journal, 0, None, 257, 4096).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            read(&journal, 4, None, 1, 4096).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            read(&journal, 2, Some(1), 1, 4096).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let ack = HistoryAck {
            cursor: HistoryCursor {
                journal_id: "journal-a".into(),
                position: DecimalU64(1),
            },
        };
        assert_eq!(journal.ack(ack).unwrap().cursor.position.0, 1);
        let other = HistoryAck {
            cursor: HistoryCursor {
                journal_id: "journal-b".into(),
                position: DecimalU64(1),
            },
        };
        assert_eq!(
            journal.ack(other).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(journal.range().unwrap().high_water.0, 3);
    }

    #[test]
    fn retention_reopen_reclaims_expired_startup_before_runtime() {
        let dir = Directory::new();
        let mut j = dir.open(RetentionPolicy::default());
        j.append("a", 1, true, event("old boot")).unwrap();
        j.append("b", 2, false, event("valid runtime")).unwrap();
        j.append("b", 3, true, event("new boot")).unwrap();
        j.close().unwrap();
        let policy = RetentionPolicy {
            startup_boots: 1,
            max_bytes: 4 * space::unit(&dir.0).unwrap(),
            ..Default::default()
        };
        let j = dir.open(policy);
        assert_eq!(
            summary(read(&j, 0, None, 10, 8192).unwrap()),
            json!({"oldest":2,"high":3,"positions":[2,3],"missing":[{"first":"1","last":"1","reason":"retention_expired"}],"next":3})
        );
    }

    #[test]
    fn retention_batch_fills_partially_used_tail() {
        let dir = Directory::new();
        let unit = space::unit(&dir.0).unwrap();
        let policy = RetentionPolicy {
            max_bytes: unit * 4,
            ..Default::default()
        };
        let text = "x".repeat(unit as usize / 2 - 250);
        let mut j = dir.open(policy.clone());
        j.append("a", 0, false, event(&text)).unwrap();
        j.append_batch((1..4).map(|t| input("a", t, false, &text)).collect())
            .unwrap();
        j.close().unwrap();
        let j = dir.open(policy);
        assert_eq!(
            summary(read(&j, 0, None, 10, 65536).unwrap()),
            json!({"oldest":1,"high":4,"positions":[1,2,3,4],"missing":[],"next":4})
        );
    }

    #[test]
    fn retention_batch_repacks_after_selecting_runtime_tail_for_eviction() {
        let dir = Directory::new();
        let unit = space::unit(&dir.0).unwrap();
        let policy = RetentionPolicy {
            max_bytes: unit * 5,
            ..Default::default()
        };
        let text = "x".repeat(unit as usize / 2 - 250);
        let mut j = dir.open(policy.clone());
        j.append("a", 0, true, event("boot")).unwrap();
        j.append("a", 1, false, event(&text)).unwrap();
        j.append_batch((2..6).map(|t| input("a", t, false, &text)).collect())
            .unwrap();
        j.close().unwrap();
        let j = dir.open(policy);
        assert_eq!(
            summary(read(&j, 0, None, 10, 65536).unwrap()),
            json!({"oldest":1,"high":6,"positions":[1,3,4,5,6],"missing":[{"first":"2","last":"2","reason":"retention_expired"}],"next":6})
        );
    }

    #[test]
    fn retention_failed_contraction_preserves_every_record() {
        let dir = Directory::new();
        let unit = space::unit(&dir.0).unwrap();
        let policy = RetentionPolicy::default();
        let mut j = dir.open(policy.clone());
        let mut expected = Vec::new();
        for (boot, startup, text) in [
            ("a", true, "first"),
            ("b", false, "second"),
            ("b", true, "third"),
        ] {
            expected.push(j.append(boot, 1, startup, event(text)).unwrap());
        }
        j.close().unwrap();
        fs::write(dir.0.join("inventory.bin"), vec![1; (unit * 5) as usize]).unwrap();
        let result = Journal::open(
            &dir.0,
            "other",
            RetentionPolicy {
                max_bytes: unit * 4,
                ..policy.clone()
            },
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidInput);
        let j = dir.open(policy);
        assert_eq!(read(&j, 0, None, 10, 8192).unwrap().records, expected);
    }

    #[test]
    fn physical_reopen_reserves_a_mixed_batch_and_preserves_startup() {
        let dir = Directory::new();
        let unit = space::unit(&dir.0).unwrap();
        let policy = RetentionPolicy {
            max_bytes: 5 * unit,
            ..Default::default()
        };
        let mut j = dir.open(policy.clone());
        j.append("a", 1, true, event("boot a")).unwrap();
        j.append("a", 2, false, event("runtime a")).unwrap();
        j.close().unwrap();
        fs::write(dir.0.join("metadata.pending"), b"interrupted").unwrap();
        fs::write(dir.0.join("segment.pending"), b"interrupted").unwrap();
        let mut j = dir.open(policy.clone());
        j.append_batch(vec![
            input("b", 3, true, "boot b"),
            input("b", 4, false, "runtime b"),
        ])
        .unwrap();
        j.close().unwrap();
        let j = dir.open(policy);
        assert_eq!(
            summary(read(&j, 0, None, 256, 4096).unwrap()),
            json!({
                "oldest":1,"high":4,"positions":[1,3,4],"missing":[{"first":"2","last":"2","reason":"retention_expired"}],"next":4
            })
        );
    }

    #[test]
    fn reopening_at_the_existing_byte_budget_allows_expiry_cleanup() {
        let directory = Directory::new();
        let policy = RetentionPolicy {
            runtime_ms: 10,
            ..RetentionPolicy::default()
        };
        let mut journal = directory.open_logical(policy.clone());
        for time in 0..5 {
            journal.append("a", time, false, event("retained")).unwrap();
        }
        journal.close().unwrap();
        let bytes: u64 = fs::read_dir(&directory.0)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(bytes >= 1024);
        let mut journal = directory.open_logical(RetentionPolicy {
            max_bytes: bytes,
            ..policy
        });
        journal.prune(20).unwrap();
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({
                "oldest":6,"high":5,"positions":[],"missing":[{"first":"1","last":"5","reason":"retention_expired"}],"next":5
            })
        );
        assert_eq!(
            journal
                .append("b", 21, false, event("new"))
                .unwrap()
                .cursor
                .position
                .0,
            6
        );
    }

    #[test]
    fn compaction_recovers_nonmonotonic_times_and_preserves_page_holes() {
        let directory = Directory::new();
        let policy = RetentionPolicy {
            runtime_ms: 10,
            ..RetentionPolicy::default()
        };
        let mut journal = directory.open(policy.clone());
        for time in [10, 0, 9, 1, 8] {
            journal.append("a", time, false, event("entry")).unwrap();
        }
        journal.prune(12).unwrap();
        journal.close().unwrap();
        let journal = directory.open(policy);
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({
                "oldest":1,"high":5,"positions":[1,3,5],"missing":[
                    {"first":"2","last":"2","reason":"retention_expired"},
                    {"first":"4","last":"4","reason":"retention_expired"}],"next":5
            })
        );
        assert_eq!(
            summary(read(&journal, 1, Some(2), 256, 4096).unwrap()),
            json!({
                "oldest":1,"high":5,"positions":[],"missing":[{"first":"2","last":"2","reason":"retention_expired"}],"next":2
            })
        );
    }

    #[test]
    fn compaction_failure_recovers_original_records() {
        let directory = Directory::new();
        let policy = RetentionPolicy {
            runtime_ms: 10,
            ..RetentionPolicy::default()
        };
        let mut journal = directory.open(policy.clone());
        journal.append("a", 0, false, event("old")).unwrap();
        journal.append("a", 10, false, event("young")).unwrap();
        fs::create_dir(directory.0.join("segment.pending")).unwrap();
        assert!(journal.prune(11).is_err());
        assert_eq!(
            journal.range().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        drop(journal);
        fs::remove_dir(directory.0.join("segment.pending")).unwrap();
        let mut journal = directory.open(policy);
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({
                "oldest":1,"high":2,"positions":[1,2],"missing":[],"next":2
            })
        );
        journal.prune(11).unwrap();
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({
                "oldest":2,"high":2,"positions":[2],"missing":[{"first":"1","last":"1","reason":"retention_expired"}],"next":2
            })
        );
    }

    fn measurement_event(count: u64) -> HistoryEvent {
        serde_json::from_value(json!({
            "type":"log", "record": {
                "stream_id":"capture-a", "seq":count.to_string(),
                "observed_at":"2026-11-17T00:00:00.000Z", "source_time":null,
                "source":"kmd", "component":"venera", "level":"info", "raw_level":"4",
                "session_id":null, "process":null, "tid":42, "text":"journal evaluation record"
            }
        }))
        .unwrap()
    }

    #[test]
    #[ignore = "filesystem throughput measurement"]
    fn measure_durable_append_throughput() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        let start = std::time::Instant::now();
        let mut window = std::time::Instant::now();
        for count in 1..=2000 {
            if count % 100 == 1 {
                window = std::time::Instant::now();
            }
            journal
                .append("a", count, false, measurement_event(count))
                .unwrap();
            if [100, 1000, 2000].contains(&count) {
                let elapsed = start.elapsed().as_secs_f64();
                eprintln!(
                    "{}",
                    json!({"batch_size":1,"records":count,"total_seconds":elapsed,"records_per_second":count as f64 / elapsed,"last_100_ms_per_append":window.elapsed().as_secs_f64() * 10.0,
                    "files":fs::read_dir(&directory.0).unwrap().count(), "logical_bytes":journal.measure_disk_bytes().unwrap()})
                );
            }
        }
        journal.close().unwrap();
        let journal = directory.open(RetentionPolicy::default());
        assert_eq!(journal.range().unwrap().high_water.0, 2000);
        assert_eq!(
            read(&journal, 1998, None, 256, 4096)
                .unwrap()
                .records
                .iter()
                .map(|record| record.cursor.position.0)
                .collect::<Vec<_>>(),
            vec![1999, 2000]
        );
    }

    #[test]
    #[ignore = "filesystem throughput measurement"]
    fn measure_durable_batch_throughput() {
        for batch_size in [64, 256] {
            let directory = Directory::new();
            let mut journal = directory.open(RetentionPolicy::default());
            let start = std::time::Instant::now();
            let mut max_commit_ms = 0.0f64;
            for first in (1..=2000).step_by(batch_size) {
                let inputs = (first..(first + batch_size as u64).min(2001))
                    .map(|count| JournalInput {
                        boot_id: "a".into(),
                        recorded_at_ms: count,
                        startup: false,
                        event: measurement_event(count),
                    })
                    .collect();
                let commit = std::time::Instant::now();
                journal.append_batch(inputs).unwrap();
                max_commit_ms = max_commit_ms.max(commit.elapsed().as_secs_f64() * 1000.0);
            }
            let elapsed = start.elapsed().as_secs_f64();
            eprintln!(
                "{}",
                json!({"batch_size":batch_size,"records":2000,"total_seconds":elapsed,"records_per_second":2000.0 / elapsed,
                "max_commit_ms":max_commit_ms,"files":fs::read_dir(&directory.0).unwrap().count(),"logical_bytes":journal.measure_disk_bytes().unwrap()})
            );
            drop(journal);
            let journal = directory.open(RetentionPolicy::default());
            assert_eq!(
                summary(read(&journal, 1998, None, 256, 4096).unwrap()),
                json!({"oldest":1,"high":2000,"positions":[1999,2000],"missing":[],"next":2000})
            );
        }
    }

    #[test]
    fn acknowledged_positions_survive_process_exit() {
        const CHILD_ROOT: &str = "VENERA_JOURNAL_EXIT_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let mut journal = Journal::open(root, "journal-a", RetentionPolicy::default()).unwrap();
            for first in (0..300).step_by(64) {
                journal
                    .append_batch(
                        (first..(first + 64).min(300))
                            .map(|time| input("a", time, false, "committed"))
                            .collect(),
                    )
                    .unwrap();
            }
            assert_eq!(journal.range().unwrap().high_water.0, 300);
            std::process::exit(31);
        }
        let directory = Directory::new();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "journal::tests::acknowledged_positions_survive_process_exit",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &directory.0)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(31));
        let mut journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            summary(read(&journal, 298, None, 256, 4096).unwrap()),
            json!({
                "oldest":1,"high":300,"positions":[299,300],"missing":[],"next":300
            })
        );
        journal.prune(5 * 60 * 60 * 1000 + 300).unwrap();
        drop(journal);
        let mut journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            journal
                .append("b", 20_000_000, true, event("next"))
                .unwrap()
                .cursor
                .position
                .0,
            301
        );
    }

    #[test]
    fn default_retention_spans_three_boots_and_five_hours() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        for (time, boot) in ["a", "b", "c", "d"].into_iter().enumerate() {
            journal
                .append(boot, time as u64, true, event("startup"))
                .unwrap();
        }
        journal.append("d", 10, false, event("runtime")).unwrap();
        journal.prune(18_000_010).unwrap();
        assert_eq!(
            read(&journal, 0, None, 256, 4096)
                .unwrap()
                .records
                .iter()
                .map(|record| record.cursor.position.0)
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 5]
        );
        journal.prune(18_000_011).unwrap();
        journal.close().unwrap();
        let journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({
                "oldest":2,"high":5,"positions":[2,3,4],"missing":[
                    {"first":"1","last":"1","reason":"retention_expired"},
                    {"first":"5","last":"5","reason":"retention_expired"}],"next":5
            })
        );
    }

    #[test]
    fn batch_returns_continuous_identities_and_durable_ack_cursor() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            journal
                .append("a", 0, false, event("single"))
                .unwrap()
                .cursor
                .position
                .0,
            1
        );
        let records = journal
            .append_batch(vec![
                input("a", 1, false, "second"),
                input("a", 2, false, "third"),
            ])
            .unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| (&*record.cursor.journal_id, record.cursor.position.0))
                .collect::<Vec<_>>(),
            vec![("journal-a", 2), ("journal-a", 3)]
        );
        assert_eq!(
            journal
                .ack(HistoryAck {
                    cursor: records[1].cursor.clone()
                })
                .unwrap()
                .cursor
                .position
                .0,
            3
        );
        drop(journal);
        let journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({"oldest":1,"high":3,"positions":[1,2,3],"missing":[],"next":3})
        );
        assert_eq!(
            serde_json::to_value(
                read(&journal, 1, None, 256, 4096).unwrap().records[0]
                    .event
                    .clone()
            )
            .unwrap(),
            json!({"type":"gap","gap":{"stream_id":"capture-a","reason":"second","count":"2","first_seq":"7","last_seq":"8"}})
        );
    }

    #[test]
    fn batch_crosses_segments_and_recovers_every_record() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        let records = journal
            .append_batch(
                (0..256)
                    .map(|time| input("a", time, false, &"x".repeat(2000)))
                    .collect(),
            )
            .unwrap();
        assert_eq!(records.len(), 256);
        assert_eq!(records[255].cursor.position.0, 256);
        drop(journal);
        let journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            summary(read(&journal, 253, None, 256, 16384).unwrap()),
            json!({"oldest":1,"high":256,"positions":[254,255,256],"missing":[],"next":256})
        );
        assert_eq!(
            read(&journal, 0, None, 256, 1024 * 1024)
                .unwrap()
                .records
                .len(),
            256
        );
    }

    #[test]
    fn batch_validation_preserves_history_and_disk_contents() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        journal.append("a", 0, true, event("original")).unwrap();
        let snapshot = || {
            let mut files: Vec<_> = fs::read_dir(&directory.0)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), fs::read(entry.path()).unwrap())
                })
                .collect();
            files.sort();
            files
        };
        let before = snapshot();
        assert_eq!(
            journal
                .append_batch(
                    (0..257)
                        .map(|time| input("a", time, false, "count"))
                        .collect()
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            journal
                .append_batch(
                    (0..3)
                        .map(|time| input("a", time, false, &"x".repeat(400_000)))
                        .collect()
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            journal
                .append_batch(vec![
                    input("a", 1, false, "valid"),
                    input("a", 2, false, &"x".repeat(1024 * 1024))
                ])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(snapshot(), before);
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({"oldest":1,"high":1,"positions":[1],"missing":[],"next":1})
        );
        assert_eq!(
            journal
                .append("a", 3, false, event("next"))
                .unwrap()
                .cursor
                .position
                .0,
            2
        );
    }

    #[test]
    fn empty_batch_preserves_a_usable_journal() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        fs::create_dir(directory.0.join("metadata.pending")).unwrap();
        assert!(journal.append_batch(Vec::new()).unwrap().is_empty());
        assert_eq!(journal.range().unwrap().high_water.0, 0);
        fs::remove_dir(directory.0.join("metadata.pending")).unwrap();
        assert_eq!(
            journal
                .append("a", 0, false, event("first"))
                .unwrap()
                .cursor
                .position
                .0,
            1
        );
    }

    #[test]
    fn batch_capacity_is_reserved_for_every_returned_record() {
        let directory = Directory::new();
        let policy = RetentionPolicy {
            max_bytes: 1200,
            ..RetentionPolicy::default()
        };
        let mut journal = directory.open_logical(policy.clone());
        journal.append("a", 0, true, event("original")).unwrap();
        assert_eq!(
            journal
                .append_batch(vec![
                    input("a", 1, true, &"x".repeat(500)),
                    input("a", 2, true, &"x".repeat(500))
                ])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({"oldest":1,"high":1,"positions":[1],"missing":[],"next":1})
        );
        journal
            .append_batch(vec![
                input("a", 1, true, &"x".repeat(250)),
                input("a", 2, true, &"y".repeat(250)),
            ])
            .unwrap();
        let total: u64 = fs::read_dir(&directory.0)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= 1200, "journal bytes: {total}");
        drop(journal);
        let journal = directory.open_logical(policy);
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({"oldest":2,"high":3,"positions":[2,3],"missing":[{"first":"1","last":"1","reason":"retention_expired"}],"next":3})
        );
    }

    #[test]
    fn batch_applies_boot_and_runtime_retention_to_prior_history() {
        let directory = Directory::new();
        let policy = RetentionPolicy {
            startup_boots: 2,
            runtime_ms: 10,
            ..RetentionPolicy::default()
        };
        let mut journal = directory.open(policy.clone());
        journal
            .append_batch(vec![
                input("a", 0, true, "startup"),
                input("a", 0, false, "runtime"),
            ])
            .unwrap();
        journal
            .append_batch(vec![input("b", 11, true, "b"), input("c", 12, true, "c")])
            .unwrap();
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({"oldest":3,"high":4,"positions":[3,4],"missing":[{"first":"1","last":"2","reason":"retention_expired"}],"next":4})
        );
        assert_eq!(
            journal
                .append_batch(vec![
                    input("d", 13, true, "d"),
                    input("e", 14, true, "e"),
                    input("f", 15, true, "f")
                ])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            journal
                .append_batch(vec![
                    input("c", 13, false, "old"),
                    input("c", 24, false, "new")
                ])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(journal.range().unwrap().high_water.0, 4);
        drop(journal);
        let journal = directory.open(policy);
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({"oldest":3,"high":4,"positions":[3,4],"missing":[{"first":"1","last":"2","reason":"retention_expired"}],"next":4})
        );
    }

    #[test]
    fn batch_segment_creation_failure_recovers_the_written_prefix() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        let obstruction = directory.0.join("00000000000000000002.jsonl");
        fs::create_dir(&obstruction).unwrap();
        assert!(journal
            .append_batch(vec![
                input("a", 0, true, "first"),
                input("b", 1, true, "second"),
                input("b", 2, true, "third")
            ])
            .is_err());
        assert_eq!(
            journal.range().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            journal
                .append_batch(vec![input("b", 3, true, "retry")])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        drop(journal);
        fs::remove_dir(obstruction).unwrap();
        let mut journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({"oldest":1,"high":1,"positions":[1],"missing":[],"next":1})
        );
        assert_eq!(
            journal
                .append_batch(vec![
                    input("b", 1, true, "second"),
                    input("b", 2, true, "third")
                ])
                .unwrap()
                .iter()
                .map(|record| record.cursor.position.0)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn batch_metadata_failure_recovers_the_complete_written_batch() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        fs::create_dir(directory.0.join("metadata.pending")).unwrap();
        assert!(journal
            .append_batch(vec![
                input("a", 0, false, "first"),
                input("a", 1, false, "second")
            ])
            .is_err());
        assert_eq!(
            journal.range().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        drop(journal);
        fs::remove_dir(directory.0.join("metadata.pending")).unwrap();
        let mut journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            summary(read(&journal, 0, None, 256, 4096).unwrap()),
            json!({"oldest":1,"high":2,"positions":[1,2],"missing":[],"next":2})
        );
        assert_eq!(
            journal
                .append("a", 2, false, event("next"))
                .unwrap()
                .cursor
                .position
                .0,
            3
        );
    }

    #[test]
    fn large_record_remains_pageable_as_high_water_gains_digits() {
        let directory = Directory::new();
        let mut journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            journal
                .append("a", 0, true, event(&"x".repeat(1_047_967)))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(journal.range().unwrap().high_water.0, 0);
        assert_eq!(
            journal
                .append("a", 0, true, event(&"x".repeat(1_047_910)))
                .unwrap()
                .cursor
                .position
                .0,
            1
        );
        for time in 2..=9 {
            journal.append("a", time, true, event("small")).unwrap();
        }
        let page = read(&journal, 0, None, 1, 1_048_576).unwrap();
        assert_eq!(serde_json::to_vec(&page).unwrap().len(), 1_048_263);
        assert_eq!(
            summary(page),
            json!({"oldest":1,"high":9,"positions":[1],"missing":[],"next":1})
        );
        journal.append("a", 10, true, event("small")).unwrap();
        let page = read(&journal, 0, None, 1, 1_048_576).unwrap();
        assert_eq!(serde_json::to_vec(&page).unwrap().len(), 1_048_264);
        assert_eq!(
            summary(page),
            json!({"oldest":1,"high":10,"positions":[1],"missing":[],"next":1})
        );
        journal.close().unwrap();
        fs::write(
            directory.0.join("metadata.json"),
            br#"{"journal_id":"journal-a","high_water":18446744073709551614,"recent_boots":["a"]}"#,
        )
        .unwrap();
        let journal = directory.open(RetentionPolicy::default());
        let page = read(&journal, 0, None, 1, 1_048_576).unwrap();
        assert_eq!(serde_json::to_vec(&page).unwrap().len(), 1_048_282);
        assert_eq!(
            summary(page),
            json!({"oldest":1,"high":18446744073709551614u64,"positions":[1],"missing":[],"next":1})
        );
        assert_eq!(
            summary(read(&journal, 1, None, 256, 4096).unwrap()),
            json!({
                "oldest":1,"high":18446744073709551614u64,"positions":[2,3,4,5,6,7,8,9,10],
                "missing":[{"first":"11","last":"18446744073709551614","reason":"retention_expired"}],"next":18446744073709551614u64
            })
        );
    }

    #[test]
    fn large_record_pages_with_twenty_digit_range_and_record_cursors() {
        let directory = Directory::new();
        directory.open(RetentionPolicy::default()).close().unwrap();
        fs::write(
            directory.0.join("metadata.json"),
            br#"{"journal_id":"journal-a","high_water":18446744073709551612,"recent_boots":[]}"#,
        )
        .unwrap();
        let mut journal = directory.open(RetentionPolicy::default());
        assert_eq!(
            journal
                .append("a", 0, true, event(&"x".repeat(1_047_891)))
                .unwrap()
                .cursor
                .position
                .0,
            18446744073709551613
        );
        journal.append("a", 1, true, event("final")).unwrap();
        journal.close().unwrap();
        let journal = directory.open(RetentionPolicy::default());
        let page = read(&journal, 18446744073709551612, None, 1, 1_048_576).unwrap();
        assert_eq!(serde_json::to_vec(&page).unwrap().len(), 1_048_320);
        assert_eq!(
            summary(page),
            json!({
                "oldest":18446744073709551613u64,"high":18446744073709551614u64,
                "positions":[18446744073709551613u64],"missing":[],"next":18446744073709551613u64
            })
        );
        assert_eq!(
            summary(read(&journal, 18446744073709551613, None, 1, 4096).unwrap()),
            json!({
                "oldest":18446744073709551613u64,"high":18446744073709551614u64,
                "positions":[18446744073709551614u64],"missing":[],"next":18446744073709551614u64
            })
        );
    }

    #[test]
    fn metadata_contracts_to_a_smaller_budget_and_preserves_identity() {
        let directory = Directory::new();
        let mut journal = Journal::open(&directory.0, "j", RetentionPolicy::default()).unwrap();
        for boot in ["a".repeat(400), "b".repeat(400), "c".repeat(400)] {
            journal.append(&boot, 0, true, event("small")).unwrap();
        }
        journal.close().unwrap();
        let original = fs::read(directory.0.join("metadata.json")).unwrap();
        assert_eq!(original.len(), 1259);
        let failed = Journal::open(
            &directory.0,
            "ignored",
            RetentionPolicy {
                startup_boots: 2,
                runtime_ms: 10,
                max_bytes: 1024,
            },
        );
        assert_eq!(failed.err().unwrap().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            fs::read(directory.0.join("metadata.json")).unwrap(),
            original
        );
        let journal = Journal::open(&directory.0, "ignored", RetentionPolicy::default()).unwrap();
        assert_eq!(journal.range().unwrap().journal_id, "j");
        assert_eq!(
            summary(
                journal
                    .read(
                        HistoryRead {
                            after: None,
                            through: None,
                            limit: 256
                        },
                        4096
                    )
                    .unwrap()
            ),
            json!({"oldest":1,"high":3,"positions":[1,2,3],"missing":[],"next":3})
        );
        journal.close().unwrap();
        fs::write(
            directory.0.join("metadata.pending"),
            b"{interrupted replacement",
        )
        .unwrap();
        let policy = RetentionPolicy {
            startup_boots: 1,
            runtime_ms: 10,
            // Both committed and contracted metadata coexist at replacement.
            max_bytes: 1259 + 453,
        };
        let mut journal =
            Journal::open_with_unit(&directory.0, "ignored", policy.clone(), 1).unwrap();
        assert_eq!(journal.range().unwrap().journal_id, "j");
        assert_eq!(
            summary(
                journal
                    .read(
                        HistoryRead {
                            after: None,
                            through: None,
                            limit: 256
                        },
                        4096
                    )
                    .unwrap()
            ),
            json!({"oldest":4,"high":3,"positions":[],"missing":[{"first":"1","last":"3","reason":"retention_expired"}],"next":3})
        );
        let total: u64 = fs::read_dir(&directory.0)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert_eq!(total, 453);
        let record = journal.append("d", 1, true, event("next")).unwrap();
        assert_eq!(
            (record.cursor.journal_id.as_str(), record.cursor.position.0),
            ("j", 4)
        );
        journal.close().unwrap();
        let journal = Journal::open_with_unit(&directory.0, "ignored", policy, 1).unwrap();
        assert_eq!(journal.range().unwrap().journal_id, "j");
        assert_eq!(
            summary(
                journal
                    .read(
                        HistoryRead {
                            after: None,
                            through: None,
                            limit: 256
                        },
                        4096
                    )
                    .unwrap()
            ),
            json!({"oldest":4,"high":4,"positions":[4],"missing":[{"first":"1","last":"3","reason":"retention_expired"}],"next":4})
        );
    }
}
