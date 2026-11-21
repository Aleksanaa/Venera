use super::*;
use crate::{ancestry::Lifecycle, history::History, journal::*};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{atomic::AtomicU64, OnceLock},
    time::Duration,
};
use venera_debug_protocol::v1::{HistoryEvent, LogRecord, ProcessIdentity};

static EVIDENCE: OnceLock<(PathBuf, Mutex<File>)> = OnceLock::new();
static VSOCK_ACK: AtomicU64 = AtomicU64::new(0);
static PIPE_ACK: AtomicU64 = AtomicU64::new(0);
static PANEL_READY: AtomicBool = AtomicBool::new(false);

fn note(value: serde_json::Value) -> io::Result<()> {
    let mut file = EVIDENCE.get().unwrap().1.lock().unwrap();
    writeln!(file, "{value}")?;
    file.flush()
}

pub(super) fn ack(local: bool, value: &serde_json::Value) {
    if EVIDENCE.get().is_none() {
        return;
    }
    if let Some(position) = value["cursor"]["position"]
        .as_str()
        .and_then(|p| p.parse::<u64>().ok())
    {
        if !local && position <= VSOCK_ACK.fetch_max(position, Ordering::AcqRel) {
            return;
        }
        let _ = note(
            serde_json::json!({"transport": if local { "pipe" } else { "vsock" }, "ack": value}),
        );
    }
}

pub(super) fn observe(
    position: u64,
    snapshot: impl FnOnce() -> Result<Vec<u8>, Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if position == 0 && !PANEL_READY.swap(true, Ordering::AcqRel) {
        note(serde_json::json!({"transport":"pipe", "phase":"panel-ready", "position":"0"}))?;
    }
    if matches!(position, 2 | 3) && position > PIPE_ACK.load(Ordering::Acquire) {
        let data = snapshot()?;
        let dir = &EVIDENCE.get().unwrap().0;
        let pending = dir.join(format!("pipe-{position}.pending"));
        fs::write(&pending, data)?;
        fs::rename(pending, dir.join(format!("pipe-{position}.jsonl")))?;
        PIPE_ACK.store(position, Ordering::Release);
    }
    Ok(())
}

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let directory =
        PathBuf::from(std::env::var_os("VENERA_FIELD_DIR").ok_or("Set VENERA_FIELD_DIR")?);
    let mode = std::env::var("VENERA_FIELD_MODE")?;
    let result = run_in(&directory, &mode);
    if let Err(error) = &result {
        if EVIDENCE.get().is_some() {
            let _ = note(serde_json::json!({"error":error.to_string()}));
        }
    }
    result
}

fn run_in(directory: &Path, mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    let sealed = match mode {
        "live" => false,
        "sealed" => true,
        _ => return Err("fixture mode: live or sealed".into()),
    };
    std::fs::create_dir(directory)?;
    EVIDENCE
        .set((
            directory.to_owned(),
            File::create(directory.join("transport.jsonl"))?.into(),
        ))
        .map_err(|_| io::Error::other("fixture already started"))?;
    let boot = clock::filetime() - 10_000_000;
    let id = format!("field-attribution-{boot}");
    let ms = (boot - 116_444_736_000_000_000) / 10_000;
    let journal_stop = Arc::new(AtomicBool::new(false));
    let (history, owner) = History::start(
        Journal::open(directory.join("journal"), &id, RetentionPolicy::default())?,
        journal_stop.clone(),
    )?;
    let state = Arc::new(Mutex::new(State::new(id.clone(), true)));
    state.lock().unwrap().history = Some(history.clone());
    let remote = state.clone();
    let vsock = std::thread::spawn(move || transport::serve(remote));
    let local = state.clone();
    let driver = std::thread::spawn(move || {
        let outcome = drive(&history, &local, &id, boot, ms, sealed);
        STOP.store(true, Ordering::Release);
        outcome.map_err(|error| error.to_string())
    });
    let result = ipc::field_follow(&state);
    STOP.store(true, Ordering::Release);
    vsock.join().map_err(|_| "vsock worker panicked")?;
    let drive_result = driver.join().map_err(|_| "fixture driver panicked")?;
    crate::history::finish(Vec::new(), &journal_stop, owner)?;
    drive_result?;
    if result.is_ok() {
        note(serde_json::json!({"phase":"finished"}))?;
    }
    result
}

fn drive(
    h: &History,
    state: &Shared,
    id: &str,
    boot: u64,
    ms: u64,
    sealed: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = |id: &str| ImportSource {
        id: id.into(),
        boot_filetime: boot,
    };
    let (logs, early, late) = (
        source("sealed-logs"),
        source("exit-and-reuse"),
        source("late-a-start"),
    );
    h.discover_imports(vec![logs.clone(), early.clone(), late.clone()])?;
    let identity = |pid, offset: u64, key: u64, name| ProcessIdentity {
        pid,
        start_time_filetime: Some((boot + offset).to_string()),
        boot_time_filetime: Some(boot.to_string()),
        start_key: Some(key.to_string()),
        exe_path: Some(format!(r"C:\fixture\{name}.exe")),
    };
    let (a, b, pa, pb) = (
        identity(42, 200, 4201, "A"),
        identity(42, 500, 4202, "B"),
        identity(10, 100, 1001, "ParentA"),
        identity(11, 110, 1101, "ParentB"),
    );
    let start = |identity: &ProcessIdentity, parent: Option<&ProcessIdentity>| Lifecycle::Start {
        identity: identity.clone(),
        parent: parent.map_or(0, |p| p.pid),
        parent_key: parent.map(|p| p.start_key.as_deref().unwrap().parse().unwrap()),
        session: None,
    };
    let row = |seq, offset, text: &str, process| LogRecord {
        stream_id: id.into(),
        seq,
        observed_at: clock::iso_filetime(boot + 600),
        source_time: Some(clock::iso_filetime(boot + offset)),
        source: "etw".into(),
        component: "guest".into(),
        level: "info".into(),
        raw_level: "info".into(),
        session_id: None,
        process: Some(process),
        ancestors: vec![],
        tid: None,
        text: text.into(),
        structured: None,
    };
    note(
        serde_json::json!({"journal_id":id, "phase":"empty", "mode":if sealed { "sealed" } else { "live" }}),
    )?;
    let dir = &EVIDENCE.get().unwrap().0;
    let mut phase = 0;
    let mut last_port = serde_json::Value::Null;
    while !stopped() && !dir.join("finish").exists() {
        let port = state.lock().unwrap().status.health["port"].clone();
        if port != last_port {
            note(serde_json::json!({"transport":"vsock", "port":port}))?;
            last_port = port.clone();
        }
        match phase {
            0 if PANEL_READY.load(Ordering::Acquire)
                && port == "present"
                && dir.join("publish-logs").exists() =>
            {
                let mut a_log = row(
                    1,
                    300,
                    "A: before PID reuse",
                    ProcessIdentity {
                        pid: 42,
                        ..Default::default()
                    },
                );
                let append = |record| {
                    h.append_durable(JournalInput {
                        boot_id: boot.to_string(),
                        recorded_at_ms: ms,
                        startup: true,
                        event: HistoryEvent::Log { record },
                    })
                };
                if !sealed {
                    append(a_log.clone())?;
                }
                h.checkpoint_processes(
                    early.clone(),
                    vec![
                        start(&pa, None),
                        start(&pb, None),
                        Lifecycle::End {
                            pid: 42,
                            start: boot + 200,
                            end: boot + 400,
                            key: Some(4201),
                        },
                        start(&b, Some(&pb)),
                    ],
                    boot + 600,
                    ms,
                )?;
                let cp = h.process_checkpoint(boot)?;
                assert_eq!(
                    cp.attribute_record(&mut a_log, boot, boot + 300, true),
                    crate::ancestry::Attribution::Missing(42)
                );
                let mut b_log = row(2, 550, "B: after PID reuse", b.clone());
                assert_eq!(
                    cp.attribute_record(&mut b_log, boot, boot + 550, true),
                    crate::ancestry::Attribution::Ready
                );
                if sealed {
                    let receipt = h.import_batch(ImportBatch {
                        source: logs.clone(),
                        through: 2,
                        complete: true,
                        now_ms: ms,
                        entries: [(300, a_log), (550, b_log)]
                            .into_iter()
                            .enumerate()
                            .map(|(i, (offset, record))| ImportEntry {
                                ordinal: i as u64 + 1,
                                source_filetime: boot + offset,
                                event: HistoryEvent::Log { record },
                            })
                            .collect(),
                    })?;
                    assert!(receipt.complete && receipt.through == 2);
                } else {
                    append(b_log)?;
                }
                assert_eq!(h.range()?.high_water.0, 2);
                phase = 2;
                note(
                    serde_json::json!({"journal_id":id, "phase":"originals", "positions":["1","2"]}),
                )?;
            }
            2 if PIPE_ACK.load(Ordering::Acquire) >= 2
                && VSOCK_ACK.load(Ordering::Acquire) >= 2
                && dir.join("publish-revision").exists() =>
            {
                for _ in 0..2 {
                    h.checkpoint_processes(
                        late.clone(),
                        vec![start(&a, Some(&pa))],
                        boot + 200,
                        ms,
                    )?;
                }
                assert_eq!(h.range()?.high_water.0, 3);
                phase = 3;
                note(
                    serde_json::json!({"journal_id":id, "phase":"revised", "position":"3", "target":"1"}),
                )?;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
