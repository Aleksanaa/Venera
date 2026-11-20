use super::*;
use crate::{
    capture_storage,
    journal::{ImportBatch, ImportEntry, ImportSource},
};
use etw::files::{self, FileSession};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows_sys::Win32::System::Diagnostics::Etw::EVENT_TRACE_FLAG_DBGPRINT;

const PRIMARY: &str = "VeneraKernelLog";
const LEGACY_KEY: &str = "SYSTEM\\CurrentControlSet\\Control\\WMI\\GlobalLogger";

fn root() -> io::Result<PathBuf> {
    let root = PathBuf::from(std::env::var_os("ProgramData").ok_or(io::ErrorKind::NotFound)?)
        .join("Venera")
        .join("DebugCapture");
    fs::create_dir_all(&root)?;
    Ok(root)
}

fn owns_legacy() -> io::Result<bool> {
    use windows_sys::Win32::System::Registry::*;
    unsafe {
        let mut key = std::ptr::null_mut();
        let code = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(LEGACY_KEY).as_ptr(),
            0,
            KEY_READ,
            &mut key,
        );
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        let mut value = [0u16; 64];
        let mut size = std::mem::size_of_val(&value) as u32;
        let mut kind = 0;
        let code = RegQueryValueExW(
            key,
            wide("VeneraOwner").as_ptr(),
            std::ptr::null(),
            &mut kind,
            value.as_mut_ptr().cast(),
            &mut size,
        );
        RegCloseKey(key);
        Ok(code == 0
            && kind == REG_SZ
            && size as usize <= std::mem::size_of_val(&value)
            && value[..size as usize / 2] == wide("VeneraKernelCapture-v1"))
    }
}

fn traces(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut sizes = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        sizes.push((name, entry.metadata()?.len()));
        paths.push(entry.path());
    }
    capture_storage::validate(&sizes)?;
    paths.sort();
    Ok(paths)
}

fn new_path(directory: &Path, prefix: &str) -> io::Result<PathBuf> {
    let paths = traces(directory)?;
    let group = if prefix.starts_with("recorder-") {
        "recorder-"
    } else {
        prefix
    };
    if paths
        .iter()
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(group)
        })
        .count()
        >= 2
    {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "Capture handoff files are awaiting import",
        ));
    }
    let path = directory.join(format!("{prefix}{}.etl", clock::filetime()));
    if path.exists() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    Ok(path)
}

struct CaptureSession {
    session: FileSession,
    active: PathBuf,
    prefix: &'static str,
}

fn now_ms() -> u64 {
    (clock::filetime() - 116444736000000000) / 10000
}

fn finish_source(
    state: &Shared,
    history: &crate::history::History,
    path: &Path,
    source: ImportSource,
    diagnostic: Option<(String, Option<u64>)>,
) -> io::Result<()> {
    let boot = state.lock().unwrap().boot_id.parse().unwrap_or(0);
    let name = path.file_name().unwrap().to_string_lossy();
    let recorder = name.starts_with("recorder-").then(|| match &diagnostic {
        Some((reason, _)) => serde_json::json!({
            "error": format!("{reason}; source={}", source.id), "file": name
        }),
        None => serde_json::json!("exported"),
    });
    history.complete_import(source, diagnostic, boot, now_ms())?;
    if let Some(result) = recorder {
        state.lock().unwrap().status.health["recorder"] = result;
    }
    Ok(())
}

fn corrupt(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
    ) || matches!(error.raw_os_error(), Some(11 | 13 | 24 | 87 | 1392 | 1500))
}

fn same_file(source: &ImportSource, path: &Path) -> io::Result<bool> {
    let identity = files::file_identity(path)?;
    Ok(source.id == identity || source.id.starts_with(&format!("{identity}:")))
}

fn checkpoint_file(
    history: &crate::history::History,
    path: &Path,
    source: &ImportSource,
) -> io::Result<()> {
    let mut pending = Vec::new();
    let mut bytes = 0;
    let mut last = 0;
    let mut context_loss = false;
    let mut loss_from = source.boot_filetime;
    let summary = files::read(path, &mut |event, boot, _| {
        if stopped() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if boot != source.boot_filetime {
            return Err(io::ErrorKind::InvalidData.into());
        }
        last = last.max(event.EventHeader.TimeStamp.max(0) as u64);
        let context = unsafe { etw::process_start_key(event) }
            .ok()
            .flatten()
            .filter(|_| event.EventHeader.TimeStamp >= boot as i64)
            .map(|key| crate::ancestry::Lifecycle::Context {
                pid: event.EventHeader.ProcessId,
                key,
                time: event.EventHeader.TimeStamp as u64,
            });
        let lifecycle = match unsafe { etw::process_lifecycle(event, boot) } {
            Ok(change) => change,
            Err(_) => {
                context_loss = true;
                None
            }
        };
        for change in [context, lifecycle].into_iter().flatten() {
            let size = serde_json::to_vec(&change)?.len();
            if size > 800 * 1024 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            if pending.len() == 128 || bytes + size > 800 * 1024 {
                history.checkpoint_processes(
                    source.clone(),
                    std::mem::take(&mut pending),
                    0,
                    now_ms(),
                )?;
                bytes = 0;
            }
            pending.push(change);
            bytes += size;
        }
        Ok(())
    });
    let through = match summary {
        Ok(summary) => {
            context_loss |= summary.events_lost != 0 || summary.buffers_lost != 0;
            loss_from = summary
                .start_time_filetime
                .parse::<u64>()
                .unwrap_or(source.boot_filetime)
                .max(source.boot_filetime);
            summary
                .end_time_filetime
                .parse::<u64>()
                .unwrap_or(0)
                .max(last)
        }
        Err(error) if corrupt(&error) => {
            context_loss = true;
            0
        }
        Err(error) => return Err(error),
    };
    if context_loss {
        history.checkpoint_processes(
            source.clone(),
            vec![crate::ancestry::Lifecycle::Gap {
                from: loss_from,
                through: through.max(last).max(loss_from),
            }],
            through,
            now_ms(),
        )?;
    }
    history.checkpoint_processes(source.clone(), pending, through, now_ms())?;
    Ok(())
}

fn primary(directory: &Path) -> io::Result<CaptureSession> {
    let (mut session, active) = match FileSession::attach(PRIMARY, directory) {
        Ok(value) => value,
        Err(error) if error.raw_os_error() == Some(ERROR_WMI_INSTANCE_NOT_FOUND as i32) => {
            let path = new_path(directory, "kernel-live-")?;
            (
                FileSession::start(PRIMARY, &path, 0, capture_storage::TRACE_MEGABYTES)?,
                path,
            )
        }
        Err(error) => return Err(error),
    };
    session.retain();
    session.enable(&etw::KERNEL_PROVIDER, 5, 1)?;
    session.enable(&etw::PROCESS_PROVIDER, 4, 0x10)?;
    Ok(CaptureSession {
        session,
        active,
        prefix: "kernel-live-",
    })
}

fn legacy(directory: &Path) -> io::Result<Option<CaptureSession>> {
    if !owns_legacy()? {
        return Ok(None);
    }
    let (mut session, active) = match FileSession::attach("NT Kernel Logger", directory) {
        Ok(value) => value,
        Err(error)
            if error.raw_os_error() == Some(ERROR_WMI_INSTANCE_NOT_FOUND as i32)
                || (error.kind() == io::ErrorKind::PermissionDenied
                    && error.raw_os_error().is_none()) =>
        {
            match FileSession::attach("VeneraLegacyLog", directory) {
                Ok(value) => value,
                Err(error) if error.raw_os_error() == Some(ERROR_WMI_INSTANCE_NOT_FOUND as i32) => {
                    let path = new_path(directory, "legacy-live-")?;
                    (
                        FileSession::start(
                            "VeneraLegacyLog",
                            &path,
                            EVENT_TRACE_FLAG_DBGPRINT,
                            capture_storage::TRACE_MEGABYTES,
                        )?,
                        path,
                    )
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    session.retain();
    Ok(Some(CaptureSession {
        session,
        active,
        prefix: "legacy-live-",
    }))
}

fn import_file(
    state: &Shared,
    history: &crate::history::History,
    path: &Path,
    source: ImportSource,
    recovered: bool,
    wait_for_processes: bool,
) -> io::Result<()> {
    let mut recorder_guid = etw::recorder_message();
    let name = path.file_name().unwrap().to_string_lossy();
    if let Some(suffix) = name.strip_prefix("recorder-v1-") {
        let guid = suffix.get(..36).ok_or(io::ErrorKind::InvalidData)?;
        recorder_guid = windows_sys::core::GUID::from_u128(
            u128::from_str_radix(&guid.replace('-', ""), 16)
                .map_err(|_| io::ErrorKind::InvalidData)?,
        );
    }
    let receipt = history.import_receipt(source.clone())?;
    if !receipt.complete {
        let mut pending = Vec::new();
        let mut bytes = 0;
        let mut last = receipt.through;
        let mut invalid_events = 0u64;
        let mut missing_process = None;
        use windows_sys::Win32::System::Diagnostics::Etw::EVENT_RECORD;
        let mut visit = |event: &EVENT_RECORD, boot, ordinal| {
            if stopped() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if boot != source.boot_filetime {
                return Err(io::ErrorKind::InvalidData.into());
            }
            last = last.max(ordinal);
            if unsafe { etw::process_lifecycle(event, boot) }.is_err() {
                invalid_events += 1;
            }
            let decoded = match unsafe { etw::decode_record_from(event, boot, recorder_guid) } {
                Ok(capture) => capture,
                Err(_) => {
                    invalid_events += 1;
                    return Ok(());
                }
            };
            if ordinal <= receipt.through {
                return Ok(());
            }
            if let Some(capture) = decoded {
                let time = capture
                    .structured
                    .as_ref()
                    .map_or(event.EventHeader.TimeStamp as u64, |e| {
                        e.source_time_filetime.0
                    });
                if let Some(class) = crate::collector::classify_capture(&capture) {
                    let record = state.lock().unwrap().capture_record(capture, Some(class));
                    if let Some(record) = record {
                        use crate::ancestry::Attribution;
                        let (status, record) = history.attribute_process_record(boot, time, wait_for_processes, record)?;
                        match status {
                            Attribution::Pending => {
                                return Err(io::Error::new(
                                    io::ErrorKind::WouldBlock,
                                    "Process history is being sealed",
                                ))
                            }
                            Attribution::Missing(pid) => missing_process = Some(pid),
                            Attribution::Ready => (),
                        }
                        let event = venera_debug_protocol::v1::HistoryEvent::Log { record };
                        let size = serde_json::to_vec(&event)?.len() + 2048;
                        if bytes + size > 850 * 1024 && !pending.is_empty() {
                            history.import_batch(ImportBatch {
                                source: source.clone(),
                                through: ordinal - 1,
                                complete: false,
                                entries: std::mem::take(&mut pending),
                                now_ms: (clock::filetime() - 116444736000000000) / 10000,
                            })?;
                            bytes = 0;
                        }
                        pending.push(ImportEntry {
                            ordinal,
                            source_filetime: time,
                            event,
                        });
                        bytes += size;
                    }
                }
            }
            if pending.len() == 128 {
                history.import_batch(ImportBatch {
                    source: source.clone(),
                    through: ordinal,
                    complete: false,
                    entries: std::mem::take(&mut pending),
                    now_ms: (clock::filetime() - 116444736000000000) / 10000,
                })?;
                bytes = 0;
            }
            Ok(())
        };
        let mut visit_failed = false;
        let summary = files::read(path, &mut |event, boot, ordinal| {
            let result = visit(event, boot, ordinal);
            visit_failed |= result.is_err();
            result
        });
        let (through, diagnostic) = match summary {
            Ok(summary) => {
                let limit = if name.starts_with("recorder-") { 4 } else { 32 } * 1024 * 1024;
                let capacity = summary.physical_bytes >= limit;
                (
                    summary.events,
                    capture_storage::capture_loss(
                        summary.events_lost,
                        summary.buffers_lost,
                        invalid_events,
                        capacity,
                        recovered,
                    ),
                )
            }
            Err(error) if !visit_failed && corrupt(&error) => {
                (last, Some((format!("capture_read_failed: {error}"), None)))
            }
            Err(error) => return Err(error),
        };
        history.import_batch(ImportBatch {
            source: source.clone(),
            through,
            complete: false,
            entries: pending,
            now_ms: (clock::filetime() - 116444736000000000) / 10000,
        })?;
        if let Some(pid) = missing_process.filter(|_| diagnostic.is_none()) {
            state.lock().unwrap().status.health["process_history"] = serde_json::json!({
                "error": "Process history unavailable for captured record", "pid": pid, "file": name,
            });
        }
        finish_source(state, history, path, source.clone(), diagnostic)?;
    }
    // The file identity is rechecked after the owner has confirmed durable completion.
    if !same_file(&source, path)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Capture file changed during import",
        ));
    }
    fs::remove_file(path)?;
    history.forget_import(source)
}

fn import_sealed(
    state: &Shared,
    history: &crate::history::History,
    directory: &Path,
    sessions: &[CaptureSession],
) -> io::Result<()> {
    let mut inventory = Vec::new();
    let paths = traces(directory)?;
    let mut present = Vec::new();
    let mut trusted = true;
    let known = history.import_sources()?;
    let recovered = if paths.iter().any(|path| {
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("recorder-")
    }) {
        match files::recover_recorders(directory) {
            Ok(paths) => Some(paths),
            Err(error) => {
                state.lock().unwrap().status.health["recorder"] =
                    serde_json::json!({"error":error.to_string()});
                None
            }
        }
    } else {
        Some(Vec::new())
    };
    for path in paths {
        let identity = match files::file_identity(&path) {
            Ok(identity) => identity,
            Err(error) => {
                trusted = false;
                state.lock().unwrap().status.health["capture_inventory"] =
                    serde_json::json!({"error":error.to_string()});
                continue;
            }
        };
        present.push(identity.clone());
        if recovered.is_none()
            && path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("recorder-")
        {
            continue;
        }
        if sessions
            .iter()
            .any(|session| session.active.canonicalize().ok() == path.canonicalize().ok())
        {
            continue;
        }
        match files::header(&path) {
            Ok(source) => inventory.push((path, source, None)),
            Err(error) if corrupt(&error) || path.metadata()?.len() < 4096 => {
                let source = known
                    .iter()
                    .find(|source| {
                        source.id == identity || source.id.starts_with(&format!("{identity}:"))
                    })
                    .cloned()
                    .unwrap_or(ImportSource {
                        id: identity,
                        boot_filetime: 0,
                    });
                inventory.push((
                    path,
                    source,
                    Some(format!("capture_header_failed: {error}")),
                ));
            }
            Err(error) => {
                trusted = false;
                state.lock().unwrap().status.health["capture_inventory"] =
                    serde_json::json!({"error":error.to_string(),"file":path});
            }
        }
    }
    if trusted {
        let boot = state.lock().unwrap().boot_id.parse().unwrap_or(0);
        history.reconcile_imports(present, boot, now_ms())?;
    }
    history.discover_imports(
        inventory
            .iter()
            .map(|(_, source, _)| source.clone())
            .collect(),
    )?;
    inventory.sort_by_key(|(_, source, _)| {
        (
            source.boot_filetime,
            source
                .id
                .rsplit(':')
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0),
        )
    });
    let mut pending_context = std::collections::BTreeSet::new();
    for (path, source, failure) in &inventory {
        if failure.is_none()
            && path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("kernel-")
        {
            if let Err(error) = checkpoint_file(history, path, source) {
                state.lock().unwrap().status.health["process_history"] =
                    serde_json::json!({"error":error.to_string()});
                pending_context.insert(path.clone());
            }
        }
    }
    for (path, source, failure) in inventory {
        if pending_context.contains(&path) {
            continue;
        }
        let result = if let Some(reason) = failure {
            (|| {
                finish_source(state, history, &path, source.clone(), Some((reason, None)))?;
                if !same_file(&source, &path)? {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                fs::remove_file(&path)?;
                history.forget_import(source)
            })()
        } else {
            let interrupted_export = recovered.as_ref().is_some_and(|paths| {
                paths
                    .iter()
                    .any(|p| p.canonicalize().ok() == path.canonicalize().ok())
            });
            let wait = sessions
                .iter()
                .any(|session| session.prefix == "kernel-live-")
                && state.lock().unwrap().boot_id == source.boot_filetime.to_string();
            import_file(state, history, &path, source, interrupted_export, wait)
        };
        if let Err(error) = result {
            let name = path.file_name().unwrap().to_string_lossy();
            let key = if name.starts_with("recorder-") {
                "recorder"
            } else if name.starts_with("legacy-") {
                "legacy_capture"
            } else {
                "kernel_import"
            };
            state.lock().unwrap().status.health[key] =
                serde_json::json!({"error":error.to_string(), "file":name});
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Diagnostics::Etw::*;

    fn replay_source(original: &Path, directory: &Path) -> PathBuf {
        let replay = new_path(directory, "kernel-live-").unwrap();
        fs::copy(original, &replay).unwrap();
        replay
    }

    #[test]
    fn copied_source_gets_a_managed_handoff_and_distinct_file_identity() {
        let directory =
            std::env::temp_dir().join(format!("venera-replay-inventory-{}", clock::filetime()));
        let capture = directory.join("capture");
        fs::create_dir_all(&capture).unwrap();
        let original = directory.join("original.etl");
        fs::write(&original, b"captured event bytes").unwrap();
        let active = capture.join("kernel-live-active.etl");
        fs::File::create(&active).unwrap().set_len(65536).unwrap();
        let replay = replay_source(&original, &capture);
        assert_ne!(
            files::file_identity(&original).unwrap(),
            files::file_identity(&replay).unwrap()
        );
        assert_eq!(fs::read(&replay).unwrap(), b"captured event bytes");
        let found = traces(&capture).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.contains(&active) && found.contains(&replay));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "Imports the November 17 early-kmd.etl.001 from VENERA_TEST_ETL"]
    fn sealed_legacy_boot_marker_survives_rescan() {
        use venera_debug_protocol::v1::{HistoryEvent, HistoryRead};

        const TEXT: &str = "VioGpu: ---> VIOGPU FULL build on on Jan  1 1980 00:00:00\n";
        let original = PathBuf::from(std::env::var_os("VENERA_TEST_ETL").expect("VENERA_TEST_ETL"));
        let directory = std::env::temp_dir().join(format!(
            "venera-sealed-legacy-{}-{}", std::process::id(), clock::filetime()
        ));
        fs::create_dir(&directory).unwrap();
        let capture = directory.join("capture");
        fs::create_dir(&capture).unwrap();
        let replay = capture.join("legacy-boot.etl.001");
        fs::copy(original, &replay).unwrap();
        let bytes = fs::read(&replay).unwrap();
        assert_eq!(&bytes[352..360], &134393633875000000u64.to_le_bytes());
        assert_eq!(&bytes[65624..65632], &134393633912317080u64.to_le_bytes());
        assert_eq!(&bytes[65648..65648 + TEXT.len()], TEXT.as_bytes());

        let scan = || {
            let state = Arc::new(Mutex::new(State::new("sealed-legacy".into(), true)));
            let journal = crate::journal::Journal::open(
                &directory.join("history"), "sealed-legacy", Default::default(),
            ).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
            import_sealed(&state, &history, &capture, &[]).unwrap();
            assert!(traces(&capture).unwrap().is_empty(), "{}", state.lock().unwrap().status.health);
            let mut after = None;
            let mut found = None;
            loop {
                let page = history.read(HistoryRead { after: after.clone(), through: None, limit: 256 }).unwrap();
                for entry in &page.records {
                    let HistoryEvent::Log { record } = &entry.event else { continue };
                    if record.text == TEXT {
                        assert_eq!(record.source_time.as_deref(), Some("2026-11-17T04:29:51.2317080Z"));
                        assert_eq!(record.source, "etw");
                        assert_eq!(record.component, "kernel");
                        assert_eq!(entry.boot_id, "134393633875000000");
                        assert!(entry.startup);
                        assert!(found.replace(page.clone()).is_none(), "duplicate boot marker");
                    }
                }
                if page.next.position == page.range.high_water { break; }
                assert!(page.next.position.0 > after.map_or(0, |cursor| cursor.position.0));
                after = Some(page.next);
            }
            stop.store(true, Ordering::Release);
            owner.join().unwrap().unwrap();
            found.expect("November 17 boot marker in history.read")
        };
        let imported = scan();
        let reopened = scan();
        assert_eq!(reopened, imported);
        let json = serde_json::to_string(&reopened).unwrap();
        if let Some(output) = std::env::var_os("VENERA_TEST_HISTORY_PAGE") {
            fs::write(output, &json).unwrap();
        }
        println!("{json}");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "Imports the preserved 28-event-loss ETL from VENERA_TEST_ETL"]
    fn sealed_trace_loss_persists_on_rescan() {
        use venera_debug_protocol::v1::{DecimalU64, HistoryEvent, HistoryRead};

        let original = PathBuf::from(std::env::var_os("VENERA_TEST_ETL").expect("VENERA_TEST_ETL"));
        let directory = std::env::temp_dir().join(format!(
            "venera-sealed-loss-{}-{}",
            std::process::id(),
            clock::filetime()
        ));
        fs::create_dir(&directory).unwrap();
        let capture = directory.join("capture");
        fs::create_dir(&capture).unwrap();
        let replay = capture.join("recorder-v1-5b4a52f6-c9de-3944-ec13-d1aa98628939-sample.etl");
        fs::copy(&original, &replay).unwrap();
        let source = files::header(&replay).unwrap();
        let summary = files::read(&replay, &mut |_, _, _| Ok(())).unwrap();
        assert_eq!(
            (summary.events, summary.events_lost, summary.buffers_lost),
            (14, 28, 0)
        );

        let open = || {
            let journal = crate::journal::Journal::open(
                &directory.join("history"),
                "sealed-loss",
                Default::default(),
            )
            .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
            (history, owner, stop)
        };
        let page = |history: &crate::history::History| {
            history
                .read(HistoryRead {
                    after: None,
                    through: None,
                    limit: 256,
                })
                .unwrap()
        };
        let state = Arc::new(Mutex::new(State::new("sealed-loss".into(), true)));
        state.lock().unwrap().boot_id = clock::boot_id().unwrap();
        let (history, owner, stop) = open();
        import_sealed(&state, &history, &capture, &[]).unwrap();
        assert!(traces(&capture).unwrap().is_empty());
        let imported = page(&history);
        assert_eq!(imported.range.high_water, DecimalU64(1));
        assert_eq!(imported.next.position, imported.range.high_water);
        assert!(imported.missing.is_empty());
        assert_eq!(imported.records.len(), 1);
        let HistoryEvent::Gap { gap } = &imported.records[0].event else {
            panic!("expected capture loss: {imported:?}")
        };
        assert_eq!(gap.reason, format!("capture_loss events=28 buffers=0 invalid=0 capacity=false recovered_export=false; source={}", source.id));
        assert_eq!(
            (gap.count, gap.first_seq, gap.last_seq),
            (Some(DecimalU64(28)), None, None)
        );
        let recorder = || state.lock().unwrap().status.health["recorder"].clone();
        let expected = serde_json::json!({"error":gap.reason, "file":replay.file_name().unwrap().to_str().unwrap()});
        assert_eq!(recorder(), expected);
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();

        let (history, owner, stop) = open();
        assert_eq!(page(&history), imported);
        import_sealed(&state, &history, &capture, &[]).unwrap();
        let rescanned = page(&history);
        assert_eq!(rescanned, imported);
        assert_eq!(recorder(), expected);
        println!("{}", serde_json::to_string(&rescanned).unwrap());
        for prefix in ["kernel-live-", "legacy-live-"] {
            fs::copy(&original, new_path(&capture, prefix).unwrap()).unwrap();
        }
        import_sealed(&state, &history, &capture, &[]).unwrap();
        assert_eq!(page(&history).records.len(), 3);
        assert_eq!(recorder(), expected);
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "Imports the rcdrlogsave viogpu3d.etl from VENERA_TEST_ETL"]
    fn sealed_recorder_business_records_survive_reopen() {
        use venera_debug_protocol::v1::{DecimalU64, EventValue, HistoryEvent, HistoryRead};

        let original = PathBuf::from(std::env::var_os("VENERA_TEST_ETL").expect("VENERA_TEST_ETL"));
        let directory = std::env::temp_dir().join(format!(
            "venera-sealed-business-{}-{}",
            std::process::id(),
            clock::filetime()
        ));
        fs::create_dir(&directory).unwrap();
        let capture = directory.join("capture");
        fs::create_dir(&capture).unwrap();
        let replay = replay_source(&original, &capture);
        let bytes = fs::read(&replay).unwrap();
        // This export's 64-bit TRACE_LOGFILE_HEADER starts at 0x68.
        assert_eq!(&bytes[0x94..0x98], &8u32.to_le_bytes());
        let boot = u64::from_le_bytes(bytes[0x160..0x168].try_into().unwrap());
        eprintln!(
            "raw BootTime={boot}; OpenTrace header={:?}",
            files::header(&replay)
        );
        let state = Arc::new(Mutex::new(State::new("sealed-business".into(), true)));
        let scan = || {
            let journal = crate::journal::Journal::open(
                &directory.join("history"),
                "sealed-business",
                Default::default(),
            )
            .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
            import_sealed(&state, &history, &capture, &[]).unwrap();
            let page = history
                .read(HistoryRead {
                    after: None,
                    through: None,
                    limit: 256,
                })
                .unwrap();
            stop.store(true, Ordering::Release);
            owner.join().unwrap().unwrap();
            page
        };
        let imported = scan();
        let reopened = scan();
        let output = directory.join("history-page.json");
        fs::write(&output, serde_json::to_vec(&reopened).unwrap()).unwrap();
        eprintln!("HistoryPage: {}", output.display());
        println!("{}", serde_json::to_string(&reopened).unwrap());
        assert_eq!(reopened, imported);
        assert!(reopened.missing.is_empty());
        assert_eq!(reopened.next.position, reopened.range.high_water);
        assert_eq!(
            reopened.records.len(),
            28,
            "rcdrlogsave journal: {reopened:?}"
        );
        assert!(
            boot > 0,
            "rcdrlogsave file header lacks source boot identity"
        );
        let mut sequences = std::collections::BTreeSet::new();
        for entry in &reopened.records {
            let HistoryEvent::Log { record } = &entry.event else {
                panic!("{entry:?}")
            };
            let event = record.structured.as_ref().unwrap();
            assert_eq!(entry.boot_id, boot.to_string());
            assert_eq!(event.boot_time_filetime.0, boot);
            assert_eq!(event.provider, "6c5a2378-22b9-4c10-a91f-69e34c280b81");
            assert_eq!(event.producer_instance.0, 39473446);
            assert_eq!(event.version, 1);
            assert_eq!(record.source, "wpp_recorder");
            assert_eq!(record.component, "kernel");
            assert_eq!(
                event.fields["transport_provider"],
                EventValue::AnsiString("5b4a52f6-c9de-3944-ec13-d1aa98628939".into())
            );
            assert_eq!(event.fields["transport_event_id"], EventValue::Uint16(10));
            assert_eq!(
                event.fields["transport_time_filetime"],
                EventValue::Uint64(DecimalU64(0))
            );
            let seq = event.producer_sequence.0;
            assert_eq!(entry.startup, matches!(seq, 1 | 2), "producer sequence {seq}");
            assert!(sequences.insert(seq), "duplicate producer sequence {seq}");
            let (id, time, iso, function, text) = match seq {
                1 => (
                    2,
                    134395387621709502,
                    "2026-11-19T05:12:42.1709502Z",
                    "DriverEntry",
                    "Venera kernel initialization",
                ),
                2 => (
                    3,
                    134395387621710722,
                    "2026-11-19T05:12:42.1710722Z",
                    "DxgkInitialize",
                    "Display driver registration",
                ),
                _ => continue,
            };
            assert_eq!((event.event_id, event.source_time_filetime.0), (id, time));
            assert_eq!(record.source_time.as_deref(), Some(iso));
            assert_eq!(record.text, text);
            assert_eq!(
                event.fields["function"],
                EventValue::AnsiString(function.into())
            );
            assert_eq!(event.fields["level"], EventValue::Uint8(4));
            assert_eq!(event.fields["line"], EventValue::Uint32(0));
            assert_eq!(event.fields["status"], EventValue::Int32(0));
        }
        assert_eq!(sequences, (1..=2).chain(205..=230).collect());
    }

    fn deferred_paths(directory: &Path) -> [PathBuf; 3] {
        [
            directory.join("kernel-boot.etl"),
            directory.join("kernel-live-2.etl"),
            directory.join("kernel-live-3.etl"),
        ]
    }

    #[test]
    fn deferred_inventory_accounts_for_the_exclusively_open_source() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory =
            std::env::temp_dir().join(format!("venera-deferred-inventory-{}", clock::filetime()));
        fs::create_dir_all(&directory).unwrap();
        let paths = deferred_paths(&directory);
        for (path, length) in paths.iter().zip([262144, 131072, 65536]) {
            fs::File::create(path).unwrap().set_len(length).unwrap();
        }
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&paths[0])
            .unwrap();
        assert_eq!(
            files::file_identity(&paths[0]).unwrap_err().raw_os_error(),
            Some(ERROR_SHARING_VIOLATION as i32)
        );
        let found = traces(&directory).unwrap();
        assert_eq!(found, paths);
        assert_eq!(
            fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().metadata().unwrap().len())
                .sum::<u64>(),
            458752
        );
        drop(locked);
        fs::remove_dir_all(directory).unwrap();
    }

    fn history_records(
        history: &crate::history::History,
    ) -> Vec<venera_debug_protocol::v1::HistoryRecord> {
        let mut records = Vec::new();
        let mut after = None;
        loop {
            let page = history
                .read(venera_debug_protocol::v1::HistoryRead {
                    after,
                    through: None,
                    limit: 256,
                })
                .unwrap();
            records.extend(page.records);
            if page.next.position.0 >= page.range.high_water.0 {
                return records;
            }
            after = Some(page.next);
        }
    }

    fn rotation_records_after(
        history: &crate::history::History,
        cursor: &venera_debug_protocol::v1::HistoryCursor,
    ) -> Vec<venera_debug_protocol::v1::HistoryRecord> {
        let started = Instant::now();
        let through = history.range().unwrap().high_water;
        let mut after = cursor.clone();
        let mut records = Vec::new();
        while after.position.0 < through.0 {
            let page = history
                .read(venera_debug_protocol::v1::HistoryRead {
                    after: Some(after.clone()),
                    through: Some(through),
                    limit: 256,
                })
                .unwrap();
            assert!(
                page.missing.is_empty(),
                "rotation history window: {:?}",
                page.missing
            );
            assert!(page.next.position.0 > after.position.0);
            records.extend(page.records);
            after = page.next;
        }
        println!(
            "history window after={} through={} records={} elapsed_ms={}",
            cursor.position.0,
            through.0,
            records.len(),
            started.elapsed().as_millis()
        );
        records
    }

    fn costs() -> (u64, u64, u64) {
        unsafe {
            let mut created: FILETIME = std::mem::zeroed();
            let mut exited = created;
            let mut kernel = created;
            let mut user = created;
            assert_ne!(
                GetProcessTimes(
                    GetCurrentProcess(),
                    &mut created,
                    &mut exited,
                    &mut kernel,
                    &mut user
                ),
                0
            );
            let ticks =
                |value: FILETIME| (value.dwHighDateTime as u64) << 32 | value.dwLowDateTime as u64;
            let process = ticks(kernel) + ticks(user);
            let mut idle = created;
            assert_ne!(GetSystemTimes(&mut idle, &mut kernel, &mut user), 0);
            let system = ticks(kernel) + ticks(user) - ticks(idle);
            let mut io: IO_COUNTERS = std::mem::zeroed();
            assert_ne!(GetProcessIoCounters(GetCurrentProcess(), &mut io), 0);
            (process, system, io.WriteTransferCount)
        }
    }

    #[test]
    #[ignore = "Measures the native workload baseline"]
    fn process_history_overhead_baseline() {
        measure_process_history(false);
    }

    #[test]
    #[ignore = "Measures native lifecycle capture and import"]
    fn process_history_overhead_capture() {
        measure_process_history(true);
    }

    fn measure_process_history(enabled: bool) {
        let scenario =
            std::env::var("VENERA_HISTORY_LOAD").unwrap_or_else(|_| "short-lived".into());
        let directory =
            std::env::temp_dir().join(format!("venera-process-cost-{}", clock::filetime()));
        let capture = directory.join("capture");
        fs::create_dir_all(&capture).unwrap();
        println!("Performance evidence: {}", directory.display());
        let file = capture.join("kernel-live-measure.etl");
        let session = if enabled {
            let session = FileSession::start(
                &format!("Venera process cost {}", std::process::id()),
                &file,
                0,
                32,
            )
            .unwrap();
            session.enable(&etw::PROCESS_PROVIDER, 4, 0x10).unwrap();
            Some(session)
        } else {
            None
        };
        let before = costs();
        let start = Instant::now();
        let count = std::env::var("VENERA_HISTORY_COUNT")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(200);
        if scenario == "short-lived" {
            for _ in 0..count {
                assert!(std::process::Command::new("C:\\Windows\\System32\\cmd.exe")
                    .args(["/d", "/c", "exit 0"])
                    .status()
                    .unwrap()
                    .success());
            }
        } else {
            assert_eq!(scenario, "desktop");
            let seconds = std::env::var("VENERA_HISTORY_SECONDS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(60);
            println!("Desktop observation window: {seconds} seconds");
            std::thread::sleep(Duration::from_secs(seconds));
        }
        let elapsed = start.elapsed().as_millis();
        let after = costs();
        let mut result = serde_json::json!({"scenario":scenario,"capture":enabled,"requested_processes":count,
            "workload_ms":elapsed,"workload_process_cpu_100ns":after.0-before.0,"workload_system_busy_100ns":after.1-before.1,
            "workload_process_written_bytes":after.2-before.2});
        if let Some(session) = session {
            session.stop().unwrap();
            let mut lifecycle = 0;
            let summary = files::read(&file, &mut |event, boot, _| {
                if unsafe { etw::process_lifecycle(event, boot) }?.is_some() {
                    lifecycle += 1;
                }
                Ok(())
            })
            .unwrap();
            fs::copy(&file, directory.join("original.etl")).unwrap();
            result["lifecycle_events"] = serde_json::json!(lifecycle);
            result["trace"] = serde_json::to_value(&summary).unwrap();
            let journal_path = directory.join("history");
            let journal =
                crate::journal::Journal::open(&journal_path, "process-cost", Default::default())
                    .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
            let state = Arc::new(Mutex::new(State::new("process-cost".into(), true)));
            state.lock().unwrap().boot_id = clock::boot_id().unwrap();
            let before = costs();
            let start = Instant::now();
            import_sealed(&state, &history, &capture, &[]).unwrap();
            let after = costs();
            result["import_ms"] = serde_json::json!(start.elapsed().as_millis());
            result["import_process_cpu_100ns"] = serde_json::json!(after.0 - before.0);
            result["import_process_written_bytes"] = serde_json::json!(after.2 - before.2);
            stop.store(true, Ordering::Release);
            owner.join().unwrap().unwrap();
            result["journal_bytes"] = serde_json::json!(fs::read_dir(&journal_path)
                .unwrap()
                .map(|entry| entry.unwrap().metadata().unwrap().len())
                .sum::<u64>());
            assert!(!file.exists());
            assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
        }
        fs::write(
            directory.join("measurement.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
    }

    #[test]
    #[ignore = "Emits the process-history test workload"]
    fn process_history_child_fixture() {
        let role = std::env::var("VENERA_HISTORY_FIXTURE").unwrap();
        let mut provider = 0;
        unsafe {
            assert_eq!(
                EventRegister(&etw::PROVIDER, None, std::ptr::null(), &mut provider),
                0
            );
        }
        write_process_marker(provider, &role);
        if matches!(role.as_str(), "parent" | "rotation-parent") {
            if let Some(emitter) = std::env::var_os("VENERA_UMD_EMITTER") {
                let emitter = PathBuf::from(emitter);
                assert!(std::process::Command::new(&emitter)
                    .arg("emit")
                    .current_dir(emitter.parent().unwrap())
                    .status()
                    .unwrap()
                    .success());
            } else {
                assert!(std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "windows::capture::tests::process_history_child_fixture",
                        "--ignored",
                        "--exact",
                        "--test-threads=1"
                    ])
                    .env(
                        "VENERA_HISTORY_FIXTURE",
                        if role == "rotation-parent" { "rotation-child" } else { "child" },
                    )
                    .status()
                    .unwrap()
                    .success());
            }
            if let Some(directory) = std::env::var_os("VENERA_HISTORY_BARRIER") {
                let directory = PathBuf::from(directory);
                fs::write(directory.join("ready"), std::process::id().to_string()).unwrap();
                let until = Instant::now() + Duration::from_secs(30);
                while !directory.join("release").exists() {
                    assert!(Instant::now() < until, "process fixture release timed out");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            if role == "rotation-parent" {
                write_process_marker(provider, "rotation-parent-after");
            }
        }
        unsafe {
            assert_eq!(EventUnregister(provider), 0);
        }
    }

    fn write_process_marker(provider: REGHANDLE, role: &str) {
        let mut payload = vec![0u8; 8];
        payload[4..8].copy_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(
            format!(
                "venera_d3d12[{}:1] I offline process {role}\0",
                std::process::id()
            )
            .as_bytes(),
        );
        let descriptor = EVENT_DESCRIPTOR {
            Id: 1,
            Level: 4,
            Keyword: 1,
            ..Default::default()
        };
        let data = EVENT_DATA_DESCRIPTOR {
            Ptr: payload.as_ptr() as u64,
            Size: payload.len() as u32,
            ..Default::default()
        };
        unsafe {
            assert_eq!(EventWrite(provider, &descriptor, 1, &data), 0);
        }
    }

    fn rotation_process_evidence(
        path: &Path,
        boot: u64,
        pid: u32,
        output: &Path,
    ) -> (u64, Vec<(u64, u64, u64, Option<u64>)>) {
        use crate::ancestry::Lifecycle;
        let mut rows = Vec::new();
        let mut stops = Vec::new();
        let summary = files::read(path, &mut |event, event_boot, ordinal| {
            assert_eq!(event_boot, boot);
            let source_time = event.EventHeader.TimeStamp as u64;
            match unsafe { etw::process_lifecycle(event, event_boot) }? {
                Some(Lifecycle::Start { identity, .. }) if identity.pid == pid => {
                    rows.push(serde_json::json!({"ordinal": ordinal, "event_filetime": source_time,
                        "start": identity}));
                }
                Some(Lifecycle::End { pid: stopped, start, end, key }) if stopped == pid => {
                    rows.push(serde_json::json!({"ordinal": ordinal, "event_filetime": source_time,
                        "pid": stopped, "start_filetime": start, "exit_filetime": end, "start_key": key}));
                    stops.push((ordinal, start, end, key));
                }
                _ => (),
            }
            Ok(())
        }).unwrap();
        fs::write(
            output,
            serde_json::to_vec_pretty(&serde_json::json!({
                "trace": summary, "process": rows,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
        assert_eq!(summary.boot_time_filetime, boot.to_string());
        (summary.events, stops)
    }

    #[test]
    #[ignore = "Replays the saved process rotation ETLs from VENERA_PROCESS_ROTATION_ETL"]
    fn replay_saved_process_rotation() {
        let fixture = PathBuf::from(std::env::var_os("VENERA_PROCESS_ROTATION_ETL").unwrap());
        let directory =
            std::env::temp_dir().join(format!("venera-process-replay-{}", clock::filetime()));
        let capture = directory.join("capture");
        fs::create_dir_all(&capture).unwrap();
        println!("Process rotation replay: {}", directory.display());
        let boot = 134396142795000000;
        let pid = 13464;
        let live_at = 134396344449899815;
        let exit = 134396344450044459;
        let path = directory.join("history");
        let open =
            || crate::journal::Journal::open(&path, "process-replay", Default::default()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(open(), stop.clone()).unwrap();
        let state = Arc::new(Mutex::new(State::new("process-replay".into(), true)));
        state.lock().unwrap().boot_id = boot.to_string();
        let first = capture.join("kernel-live-1.etl");
        fs::copy(fixture.join("before.etl"), &first).unwrap();
        assert!(rotation_process_evidence(
            &first,
            boot,
            pid,
            &directory.join("before-process.json")
        )
        .1
        .is_empty());
        import_sealed(&state, &history, &capture, &[]).unwrap();
        assert!(!first.exists());
        let second = capture.join("kernel-live-2.etl");
        fs::copy(fixture.join("after.etl"), &second).unwrap();
        let (events, stops) =
            rotation_process_evidence(&second, boot, pid, &directory.join("after-process.json"));
        assert_eq!(
            stops,
            [(4, 134396344447018506, exit, Some(114841790497963724))]
        );
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&second)
            .unwrap();
        let source = files::header(&second).unwrap();
        import_sealed(&state, &history, &capture, &[]).unwrap();
        let receipt = crate::journal::ImportReceipt {
            source: source.clone(),
            through: events,
            complete: true,
        };
        assert_eq!(history.import_receipt(source.clone()).unwrap(), receipt);
        let check = |snapshot: crate::ancestry::Checkpoint| {
            let (identity, _) = snapshot.processes.observed(pid, live_at).unwrap();
            assert_eq!(identity.start_key.as_deref(), Some("114841790497963724"));
            assert_eq!(
                snapshot.processes.observed(pid, exit - 1).unwrap().0,
                identity
            );
            assert_eq!(snapshot.processes.observed(pid, exit + 1), None);
        };
        check(history.process_checkpoint(boot).unwrap());
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        let journal = open();
        assert_eq!(journal.import_receipt(&source).unwrap(), receipt);
        check(journal.process_checkpoint(boot).unwrap());
        journal.close().unwrap();
        drop(held);
    }

    #[test]
    #[ignore = "Uses an independent elevated Windows ETW file session"]
    fn process_history_survives_etl_rotation_and_reopen() {
        use std::os::windows::fs::OpenOptionsExt;
        use venera_debug_protocol::v1::HistoryEvent;
        use venera_debug_protocol::{matches_filter, Filter, Target};
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let directory =
            std::env::temp_dir().join(format!("venera-process-rotation-{}", clock::filetime()));
        let capture = directory.join("capture");
        fs::create_dir_all(&capture).unwrap();
        println!("Process rotation evidence: {}", directory.display());
        let first = capture.join("kernel-live-1.etl");
        let name = format!("Venera process rotation {}", std::process::id());
        let session = FileSession::start(&name, &first, 0, 32).unwrap();
        session.enable(&etw::PROCESS_PROVIDER, 4, 0x10).unwrap();
        session.enable(&etw::PROVIDER, 4, 1).unwrap();
        let mut sessions = [CaptureSession {
            session,
            active: first.clone(),
            prefix: "kernel-live-",
        }];
        let mut producer = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "windows::capture::tests::process_history_child_fixture",
                "--ignored",
                "--exact",
                "--test-threads=1",
            ])
            .env("VENERA_HISTORY_FIXTURE", "rotation-parent")
            .env("VENERA_HISTORY_BARRIER", &directory)
            .env_remove("VENERA_UMD_EMITTER")
            .spawn()
            .unwrap();
        let producer_pid = producer.id();
        let until = Instant::now() + Duration::from_secs(30);
        while !directory.join("ready").exists() {
            assert!(Instant::now() < until, "process fixture ready timed out");
            assert!(producer.try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(10));
        }
        let live_at = clock::filetime();
        let second = sessions[0]
            .session
            .rotate(&capture.join("kernel-live-2.etl"))
            .unwrap();
        let first_rotated_at = clock::filetime();
        sessions[0].active = second.clone();
        let source = files::header(&first).unwrap();
        let boot = source.boot_filetime;
        let inspect = |path: &Path, label: &str| {
            fs::copy(path, directory.join(format!("{label}.etl"))).unwrap();
            rotation_process_evidence(
                path,
                boot,
                producer_pid,
                &directory.join(format!("{label}-process.json")),
            )
        };
        assert!(inspect(&first, "before").1.is_empty());
        let path = directory.join("history");
        let open = || {
            crate::journal::Journal::open(&path, "process-rotation", Default::default()).unwrap()
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(open(), stop.clone()).unwrap();
        let state = Arc::new(Mutex::new(State::new("process-rotation".into(), true)));
        state.lock().unwrap().boot_id = boot.to_string();
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        assert!(!first.exists() && second.exists());
        assert!(history.import_sources().unwrap().is_empty());
        let before = history_records(&history);
        let logs = |records: &[venera_debug_protocol::v1::HistoryRecord]| {
            records
                .iter()
                .filter_map(|entry| {
                    assert_eq!(entry.boot_id, boot.to_string());
                    match &entry.event {
                        HistoryEvent::Log { record }
                            if record.text.contains(" I offline process rotation-") =>
                        {
                            Some(record.clone())
                        }
                        HistoryEvent::Log { .. } => None,
                        event => panic!("unexpected rotation event: {event:?}"),
                    }
                })
                .collect::<Vec<_>>()
        };
        let initial = logs(&before);
        assert_eq!(initial.len(), 2);
        let parent = &initial[0];
        let child = &initial[1];
        let identity = parent.process.as_ref().unwrap();
        let child_identity = child.process.as_ref().unwrap();
        assert_eq!(identity.pid, producer.id());
        assert_ne!(identity.pid, child_identity.pid);
        for (record, body) in [(parent, "rotation-parent"), (child, "rotation-child")] {
            let process = record.process.as_ref().unwrap();
            assert_eq!(
                record.text,
                format!("venera_d3d12[{}:1] I offline process {body}", process.pid)
            );
            assert_eq!(process.boot_time_filetime, Some(boot.to_string()));
            assert!(process.start_key.is_some());
        }
        assert_eq!(child.ancestors.first(), Some(identity));
        let filter = |include_descendants, start_key| Filter {
            targets: vec![Target::ProcessInstance {
                pid: identity.pid,
                start_time_filetime: identity.start_time_filetime.clone().unwrap(),
                boot_time_filetime: identity.boot_time_filetime.clone(),
                start_key,
                include_descendants,
            }],
            ..Default::default()
        };
        let direct = filter(false, identity.start_key.clone());
        let family = filter(true, identity.start_key.clone());
        let other = filter(
            true,
            Some((identity.start_key.as_ref().unwrap().parse::<u64>().unwrap() ^ 1).to_string()),
        );
        assert!(matches_filter(&direct, parent, &parent.ancestors));
        assert!(!matches_filter(&direct, child, &child.ancestors));
        assert!(matches_filter(&family, child, &child.ancestors));
        assert!(!matches_filter(&other, child, &child.ancestors));
        let owns_parent = |checkpoint: crate::ancestry::Checkpoint| {
            assert_eq!(
                checkpoint
                    .processes
                    .observed(identity.pid, live_at)
                    .unwrap()
                    .0,
                *identity
            );
        };
        owns_parent(history.process_checkpoint(boot).unwrap());
        fs::write(directory.join("release"), b"continue").unwrap();
        assert!(producer.wait().unwrap().success());
        let wait_sample = clock::filetime();
        sessions[0].active = sessions[0]
            .session
            .rotate(&capture.join("kernel-live-3.etl"))
            .unwrap();
        let second_rotated_at = clock::filetime();
        fs::write(directory.join("timing.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "pid": identity.pid, "live_sample_filetime": live_at, "first_rotate_return_filetime": first_rotated_at,
            "wait_sample_filetime": wait_sample, "second_rotate_return_filetime": second_rotated_at,
        })).unwrap()).unwrap();
        let (events, stop_events) = inspect(&second, "after");
        let [(stop_ordinal, start, exit, key)] = stop_events.as_slice() else {
            panic!(
                "expected one sealed process stop for PID {}: {stop_events:?}",
                identity.pid
            );
        };
        assert_eq!(
            *start,
            identity
                .start_time_filetime
                .as_ref()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        );
        assert_eq!(key.map(|n| n.to_string()), identity.start_key);
        assert!(live_at < *exit);
        let after_exit = exit.checked_add(1).unwrap();
        let owns_lifecycle = |checkpoint: crate::ancestry::Checkpoint| {
            assert_eq!(
                checkpoint
                    .processes
                    .observed(identity.pid, exit - 1)
                    .unwrap()
                    .0,
                *identity
            );
            assert_eq!(checkpoint.processes.observed(identity.pid, *exit), None);
            assert_eq!(
                checkpoint.processes.observed(identity.pid, after_exit),
                None
            );
            owns_parent(checkpoint);
        };
        let source = files::header(&second).unwrap();
        let active = sessions[0].active.clone();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&second)
            .unwrap();
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        assert!(second.exists() && active.exists());
        assert_eq!(history.import_sources().unwrap(), [source.clone()]);
        let receipt = crate::journal::ImportReceipt {
            source: source.clone(),
            through: events,
            complete: true,
        };
        assert!((1..=receipt.through).contains(stop_ordinal));
        assert_eq!(history.import_receipt(source.clone()).unwrap(), receipt);
        fs::write(
            directory.join("receipt.json"),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
        owns_lifecycle(history.process_checkpoint(boot).unwrap());
        let after = history_records(&history);
        assert_eq!(&after[..before.len()], &before);
        let observed = logs(&after);
        assert_eq!(observed.len(), 3);
        let last = &observed[2];
        assert_eq!(
            last.text,
            format!(
                "venera_d3d12[{}:1] I offline process rotation-parent-after",
                identity.pid
            )
        );
        assert_eq!(last.process.as_ref(), Some(identity));
        assert!(matches_filter(&direct, last, &last.ancestors));
        assert!(!matches_filter(&other, last, &last.ancestors));

        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        let journal = open();
        assert_eq!(journal.import_receipt(&source).unwrap(), receipt);
        owns_lifecycle(journal.process_checkpoint(boot).unwrap());
        drop(held);
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        assert_eq!(history_records(&history), after);
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        assert!(!second.exists() && active.exists());
        assert!(history.import_sources().unwrap().is_empty());
        assert_eq!(history_records(&history), after);
        owns_lifecycle(history.process_checkpoint(boot).unwrap());
        fs::write(
            directory.join("records.json"),
            serde_json::to_vec_pretty(&after).unwrap(),
        )
        .unwrap();
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        let [session] = sessions;
        session.session.stop().unwrap();
    }

    #[test]
    #[ignore = "Captures native process lifecycles while the history owner is offline"]
    fn offline_process_tree_import_preserves_instance_and_ancestors() {
        let directory =
            std::env::temp_dir().join(format!("venera-process-history-{}", clock::filetime()));
        let capture = directory.join("capture");
        fs::create_dir_all(&capture).unwrap();
        println!("Process history evidence: {}", directory.display());
        let first = capture.join("kernel-live-1.etl");
        let next = capture.join("kernel-live-2.etl");
        let name = format!("Venera process history {}", std::process::id());
        let mut session = FileSession::start(&name, &first, 0, 32).unwrap();
        session.enable(&etw::PROCESS_PROVIDER, 4, 0x10).unwrap();
        session.enable(&etw::PROVIDER, 4, 1).unwrap();
        let started = Instant::now();
        assert!(std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "windows::capture::tests::process_history_child_fixture",
                "--ignored",
                "--exact",
                "--test-threads=1"
            ])
            .env("VENERA_HISTORY_FIXTURE", "parent")
            .status()
            .unwrap()
            .success());
        session.rotate(&next).unwrap();
        fs::copy(&first, directory.join("original.etl")).unwrap();
        let mut lifecycle_count = 0;
        files::inspect(
            &first,
            &mut fs::File::create(directory.join("original.jsonl")).unwrap(),
        )
        .unwrap();
        let mut key_count = 0;
        let summary = files::read(&first, &mut |event, boot, _| {
            if unsafe { etw::process_lifecycle(event, boot) }?.is_some() {
                lifecycle_count += 1;
            }
            if unsafe { etw::process_start_key(event) }?.is_some() {
                key_count += 1;
            }
            Ok(())
        })
        .unwrap();
        println!("Lifecycle events: {lifecycle_count}; extended-key events: {key_count}; trace summary: {summary:?}");
        assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
        let journal_path = directory.join("history");
        let journal =
            crate::journal::Journal::open(&journal_path, "process-history", Default::default())
                .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        let state = Arc::new(Mutex::new(State::new("process-history".into(), true)));
        state.lock().unwrap().boot_id = clock::boot_id().unwrap();
        let sessions = vec![CaptureSession {
            session,
            active: next,
            prefix: "kernel-live-",
        }];
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        assert!(!first.exists());
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        let journal =
            crate::journal::Journal::open(&journal_path, "process-history", Default::default())
                .unwrap();
        let page = journal
            .read(
                venera_debug_protocol::v1::HistoryRead {
                    after: None,
                    through: None,
                    limit: 100,
                },
                100_000,
            )
            .unwrap();
        let records: Vec<_> = page
            .records
            .iter()
            .filter_map(|entry| match &entry.event {
                venera_debug_protocol::v1::HistoryEvent::Log { record } => Some(record),
                _ => None,
            })
            .collect();
        assert_eq!(records.len(), 2);
        let parent = records
            .iter()
            .find(|record| record.text.ends_with("offline process parent"))
            .unwrap();
        let child = records
            .iter()
            .find(|record| record.text.ends_with("offline process child"))
            .unwrap();
        let identity = parent.process.clone().unwrap();
        assert!(identity.start_key.is_some());
        assert_eq!(identity.boot_time_filetime, Some(clock::boot_id().unwrap()));
        assert!(identity
            .exe_path
            .as_ref()
            .unwrap()
            .to_lowercase()
            .ends_with(".exe"));
        assert_eq!(child.ancestors.first(), Some(&identity));
        let filter = venera_debug_protocol::Filter {
            targets: vec![venera_debug_protocol::Target::ProcessInstance {
                pid: identity.pid,
                start_time_filetime: identity.start_time_filetime.unwrap(),
                include_descendants: true,
                boot_time_filetime: identity.boot_time_filetime,
                start_key: identity.start_key,
            }],
            ..Default::default()
        };
        assert!(venera_debug_protocol::matches_filter(
            &filter,
            child,
            &child.ancestors
        ));
        println!(
            "Elapsed ms: {}; journal bytes: {}",
            started.elapsed().as_millis(),
            fs::read_dir(&journal_path)
                .unwrap()
                .map(|entry| entry.unwrap().metadata().unwrap().len())
                .sum::<u64>()
        );
        sessions.into_iter().next().unwrap().session.stop().unwrap();
    }

    #[test]
    #[ignore = "Imports actual x86 and x64 shared logf output after both producers exit"]
    fn offline_user_log_modules_preserve_process_ancestry_and_sequences() {
        let directory =
            std::env::temp_dir().join(format!("venera-user-log-import-{}", clock::filetime()));
        let capture = directory.join("capture");
        fs::create_dir_all(&capture).unwrap();
        let first = capture.join("kernel-live-1.etl");
        let next = capture.join("kernel-live-2.etl");
        let mut session = FileSession::start(
            &format!("Venera user log {}", std::process::id()),
            &first,
            0,
            32,
        )
        .unwrap();
        session.enable(&etw::KERNEL_PROVIDER, 5, 1).unwrap();
        session.enable(&etw::PROCESS_PROVIDER, 4, 0x10).unwrap();
        session.enable(&etw::PROVIDER, 4, 1).unwrap();
        let mut parents = Vec::new();
        for variable in ["VENERA_UMD_X64", "VENERA_UMD_X86"] {
            let emitter = PathBuf::from(std::env::var_os(variable).expect(variable))
                .canonicalize()
                .unwrap();
            let mut parent = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "windows::capture::tests::process_history_child_fixture",
                    "--ignored",
                    "--exact",
                    "--test-threads=1",
                ])
                .env("VENERA_HISTORY_FIXTURE", "parent")
                .env("VENERA_UMD_EMITTER", emitter)
                .env_remove("VENERA_HISTORY_BARRIER")
                .spawn()
                .unwrap();
            parents.push(parent.id());
            assert!(parent.wait().unwrap().success());
        }
        session.rotate(&next).unwrap();
        let after_exit = clock::filetime();
        let summary = files::read(&first, &mut |_, _, _| Ok(())).unwrap();
        assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
        let boot: u64 = summary.boot_time_filetime.parse().unwrap();
        fs::copy(&first, directory.join("original.etl")).unwrap();
        files::inspect(
            &first,
            &mut fs::File::create(directory.join("original.jsonl")).unwrap(),
        )
        .unwrap();
        let original_source = files::header(&first).unwrap();
        let journal_path = directory.join("history");
        let journal =
            crate::journal::Journal::open(&journal_path, "user-log-import", Default::default())
                .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        let state = Arc::new(Mutex::new(State::new("user-log-import".into(), true)));
        state.lock().unwrap().boot_id = boot.to_string();
        let mut sessions = vec![CaptureSession {
            session,
            active: next,
            prefix: "kernel-live-",
        }];
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        assert!(!first.exists());
        assert_eq!(
            history_records(&history)
                .iter()
                .filter(|entry| matches!(&entry.event,
            venera_debug_protocol::v1::HistoryEvent::Log { record }
            if record.structured.as_ref().is_some_and(|event| matches!(event.event_id, 5 | 6))))
                .count(),
            128
        );
        let replay = replay_source(&directory.join("original.etl"), &capture);
        assert_ne!(files::header(&replay).unwrap().id, original_source.id);
        assert_eq!(
            fs::read(&replay).unwrap(),
            fs::read(directory.join("original.etl")).unwrap()
        );
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        assert!(!replay.exists());
        let records = history_records(&history);
        assert!(!records.iter().any(|entry| matches!(
            &entry.event,
            venera_debug_protocol::v1::HistoryEvent::Gap { .. }
        )));
        let logs: Vec<_> = records
            .iter()
            .filter_map(|entry| match &entry.event {
                venera_debug_protocol::v1::HistoryEvent::Log { record }
                    if record
                        .structured
                        .as_ref()
                        .is_some_and(|event| matches!(event.event_id, 5 | 6)) =>
                {
                    Some(record)
                }
                _ => None,
            })
            .collect();
        assert_eq!(logs.len(), 128);
        let mut sequences = std::collections::BTreeMap::<_, std::collections::BTreeSet<_>>::new();
        let checkpoint = history.process_checkpoint(boot).unwrap();
        for record in logs {
            assert_eq!(record.text, "shared logf repeated 42");
            let identity = record.process.as_ref().unwrap();
            assert_eq!(
                identity.boot_time_filetime.as_deref(),
                Some(boot.to_string().as_str())
            );
            assert!(identity.start_key.is_some());
            assert!(identity
                .exe_path
                .as_ref()
                .unwrap()
                .to_lowercase()
                .ends_with("log-capture.exe"));
            assert!(parents.contains(&record.ancestors[0].pid));
            assert!(record.ancestors[0].start_key.is_some());
            assert!(records.iter().any(|entry| matches!(&entry.event,
                venera_debug_protocol::v1::HistoryEvent::Log { record:parent }
                if parent.text.ends_with("offline process parent") && parent.process.as_ref() == record.ancestors.first())));
            assert_eq!(
                checkpoint.processes.observed(identity.pid, after_exit),
                None
            );
            let event = record.structured.as_ref().unwrap();
            assert_eq!(
                record.component,
                if event.event_id == 5 {
                    "venera_d3d11"
                } else {
                    "venera_d3d12"
                }
            );
            assert_eq!(
                record.source_time.as_deref(),
                Some(clock::iso_filetime(event.source_time_filetime.0).as_str())
            );
            sequences
                .entry((
                    identity.pid,
                    identity.start_time_filetime.clone(),
                    event.event_id,
                ))
                .or_default()
                .insert(event.producer_sequence.0);
        }
        assert_eq!(sequences.len(), 4);
        for values in sequences.values() {
            assert_eq!(
                values.iter().copied().collect::<Vec<_>>(),
                (1..=32).collect::<Vec<_>>()
            );
        }
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        let journal =
            crate::journal::Journal::open(&journal_path, "user-log-import", Default::default())
                .unwrap();
        assert_eq!(journal.range().unwrap().high_water.0, records.len() as u64);
        println!("User log import evidence: {}", directory.display());
        sessions.pop().unwrap().session.stop().unwrap();
    }

    #[test]
    #[ignore = "Captures a stopped primary session with zero reported trace loss"]
    fn zero_loss_primary_stop_preserves_process_interruption_on_replay() {
        let directory =
            std::env::temp_dir().join(format!("venera-zero-loss-stop-{}", clock::filetime()));
        let capture = directory.join("capture");
        fs::create_dir_all(&capture).unwrap();
        let name = format!("Venera zero loss {}", std::process::id());
        let first = capture.join("kernel-live-1.etl");
        let session = FileSession::start(&name, &first, 0, 32).unwrap();
        FileSession::attach(&name, &capture)
            .unwrap()
            .0
            .stop()
            .unwrap();
        let summary = files::read(&first, &mut |_, _, _| Ok(())).unwrap();
        assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
        let while_stopped = clock::filetime();
        fs::copy(&first, directory.join("original.etl")).unwrap();
        let boot: u64 = summary.boot_time_filetime.parse().unwrap();
        let journal_path = directory.join("history");
        let journal =
            crate::journal::Journal::open(&journal_path, "zero-loss", Default::default()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        let source = files::header(&first).unwrap();
        history.discover_imports(vec![source.clone()]).unwrap();
        let identity = venera_debug_protocol::ProcessIdentity {
            pid: 10,
            start_time_filetime: Some((boot + 100).to_string()),
            start_key: Some("11".into()),
            boot_time_filetime: Some(boot.to_string()),
            exe_path: Some("C:\\old.exe".into()),
        };
        let start = crate::ancestry::Lifecycle::Start {
            identity: identity.clone(),
            parent: 0,
            parent_key: None,
            session: None,
        };
        history
            .checkpoint_processes(source.clone(), vec![start.clone()], boot + 200, now_ms())
            .unwrap();
        let state = Arc::new(Mutex::new(State::new("zero-loss".into(), true)));
        state.lock().unwrap().boot_id = boot.to_string();
        let mut sessions = vec![CaptureSession {
            session,
            active: first,
            prefix: "kernel-live-",
        }];
        rotate_sessions(&state, &history, &capture, &mut sessions).unwrap();
        assert!(sessions[0].session.running().unwrap());
        assert_eq!(
            history
                .process_checkpoint(boot)
                .unwrap()
                .processes
                .observed(10, while_stopped),
            None
        );
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        let mut journal =
            crate::journal::Journal::open(&journal_path, "zero-loss", Default::default()).unwrap();
        journal.discover_imports(vec![source.clone()]).unwrap();
        journal
            .checkpoint_processes(&source, vec![start], boot + 200, now_ms())
            .unwrap();
        let checkpoint = journal.process_checkpoint(boot).unwrap();
        assert_eq!(checkpoint.processes.observed(10, while_stopped), None);
        assert_eq!(
            checkpoint.processes.observed(10, boot + 150).unwrap().0,
            identity
        );
        println!("Zero-loss recovery evidence: {}", directory.display());
        sessions.pop().unwrap().session.stop().unwrap();
    }

    #[test]
    #[ignore = "Imports a deferred lifecycle file after its later stop file"]
    fn deferred_process_file_merges_after_later_file_import_and_reopen() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory =
            std::env::temp_dir().join(format!("venera-deferred-process-{}", clock::filetime()));
        let capture = directory.join("capture");
        fs::create_dir_all(&capture).unwrap();
        let [first, second, third] = deferred_paths(&capture);
        let mut session = FileSession::start(
            &format!("Venera deferred process {}", std::process::id()),
            &first,
            0,
            32,
        )
        .unwrap();
        session.enable(&etw::PROCESS_PROVIDER, 4, 0x10).unwrap();
        session.enable(&etw::PROVIDER, 4, 1).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "windows::capture::tests::process_history_child_fixture",
                "--ignored",
                "--exact",
                "--test-threads=1",
            ])
            .env("VENERA_HISTORY_FIXTURE", "parent")
            .env("VENERA_HISTORY_BARRIER", &directory)
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(30);
        while !directory.join("ready").exists() {
            assert!(Instant::now() < until, "process fixture ready timed out");
            assert!(child.try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(10));
        }
        let parent_pid = child.id();
        session.rotate(&second).unwrap();
        fs::write(directory.join("release"), b"continue").unwrap();
        assert!(child.wait().unwrap().success());
        session.rotate(&third).unwrap();
        let after_exit = clock::filetime();
        let summary = files::read(&second, &mut |_, _, _| Ok(())).unwrap();
        assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
        let boot: u64 = summary.boot_time_filetime.parse().unwrap();
        fs::copy(&first, directory.join("starts.etl")).unwrap();
        fs::copy(&second, directory.join("stops.etl")).unwrap();
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&first)
            .unwrap();
        let journal_path = directory.join("history");
        let journal =
            crate::journal::Journal::open(&journal_path, "deferred", Default::default()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        let state = Arc::new(Mutex::new(State::new("deferred".into(), true)));
        state.lock().unwrap().boot_id = boot.to_string();
        let mut sessions = vec![CaptureSession {
            session,
            active: third,
            prefix: "kernel-live-",
        }];
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        assert!(first.exists());
        assert!(!second.exists());
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        drop(locked);
        let journal =
            crate::journal::Journal::open(&journal_path, "deferred", Default::default()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        assert!(!first.exists());
        let records = history_records(&history);
        let logs: Vec<_> = records
            .iter()
            .filter_map(|entry| match &entry.event {
                venera_debug_protocol::v1::HistoryEvent::Log { record } => Some(record),
                _ => None,
            })
            .collect();
        let parent = logs
            .iter()
            .find(|record| record.text.ends_with("offline process parent"))
            .unwrap();
        let child = logs
            .iter()
            .find(|record| record.text.ends_with("offline process child"))
            .unwrap();
        assert_eq!(parent.process.as_ref().unwrap().pid, parent_pid);
        assert!(parent.process.as_ref().unwrap().start_key.is_some());
        assert_eq!(child.ancestors.first(), parent.process.as_ref());
        assert_eq!(
            history
                .process_checkpoint(boot)
                .unwrap()
                .processes
                .observed(parent_pid, after_exit),
            None
        );
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        println!("Deferred lifecycle evidence: {}", directory.display());
        sessions.pop().unwrap().session.stop().unwrap();
    }

    #[test]
    #[ignore = "Reads the primary boot trace before the collector starts"]
    fn boot_primary_session_adopts_and_seals_original_events() {
        check_boot_session(PRIMARY, "kernel-live-", "kernel");
    }

    #[test]
    #[ignore = "Imports numbered boot-session rotations into installed history with the collector stopped"]
    fn numbered_boot_session_rotations_preserve_markers_and_active_file() {
        use windows_sys::Win32::System::Services::*;
        unsafe {
            let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            assert!(!manager.is_null());
            let service = OpenServiceW(
                manager,
                wide("VeneraDebugGuest").as_ptr(),
                SERVICE_QUERY_STATUS,
            );
            assert!(!service.is_null());
            let mut status: SERVICE_STATUS = std::mem::zeroed();
            assert_ne!(QueryServiceStatus(service, &mut status), 0);
            CloseServiceHandle(service);
            CloseServiceHandle(manager);
            assert_eq!(
                status.dwCurrentState, SERVICE_STOPPED,
                "collector history owner must be stopped"
            );
        }
        let capture = root().unwrap();
        let evidence =
            std::env::temp_dir().join(format!("venera-numbered-boot-{}", clock::filetime()));
        fs::create_dir_all(&evidence).unwrap();
        println!("Numbered boot evidence: {}", evidence.display());
        let (session, active) = FileSession::attach(PRIMARY, &capture).unwrap();
        assert!(active
            .extension()
            .unwrap()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit()));
        let control = session.control_status().unwrap();
        assert_eq!(
            control.1,
            EVENT_TRACE_FILE_MODE_SEQUENTIAL | EVENT_TRACE_PERSIST_ON_HYBRID_SHUTDOWN
        );
        assert_eq!((control.2, control.3, control.4), (0, 0, 0));
        session.enable(&etw::KERNEL_PROVIDER, 5, 1).unwrap();
        let mut sessions = vec![CaptureSession {
            session,
            active,
            prefix: "kernel-live-",
        }];
        if owns_legacy().unwrap() {
            for name in ["NT Kernel Logger", "VeneraLegacyLog"] {
                match FileSession::attach(name, &capture) {
                    Ok((session, active)) => sessions.push(CaptureSession {
                        session,
                        active,
                        prefix: "legacy-live-",
                    }),
                    Err(error)
                        if error.raw_os_error() == Some(ERROR_WMI_INSTANCE_NOT_FOUND as i32)
                            || error.kind() == io::ErrorKind::PermissionDenied =>
                    {
                        ()
                    }
                    Err(error) => panic!("legacy attach: {error}"),
                }
            }
        }
        let preserve = |sessions: &[CaptureSession]| {
            for path in traces(&capture).unwrap() {
                if sessions.iter().any(|s| s.active == path) {
                    continue;
                }
                let saved = evidence.join(path.file_name().unwrap());
                if !saved.exists() {
                    fs::copy(&path, saved).unwrap();
                }
            }
        };
        let history_path = capture.parent().unwrap().join("DebugHistory");
        let journal = crate::journal::Journal::open(
            &history_path,
            "numbered-boot",
            crate::journal::RetentionPolicy {
                max_bytes: capture_storage::JOURNAL_BYTES,
                ..Default::default()
            },
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        let state = Arc::new(Mutex::new(State::new("numbered-boot".into(), true)));
        state.lock().unwrap().boot_id = clock::boot_id().unwrap();
        preserve(&sessions);
        let started = Instant::now();
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        println!(
            "initial sealed import elapsed_ms={}",
            started.elapsed().as_millis()
        );
        let range = history.range().unwrap();
        let baseline = venera_debug_protocol::v1::HistoryCursor {
            journal_id: range.journal_id,
            position: range.high_water,
        };
        let instance = clock::filetime();
        let markers = |records: &[venera_debug_protocol::v1::HistoryRecord]| {
            records
                .iter()
                .filter_map(|entry| {
                    if let venera_debug_protocol::v1::HistoryEvent::Log { record } = &entry.event {
                        record
                            .structured
                            .as_ref()
                            .filter(|event| event.producer_instance.0 == instance)
                            .map(|event| {
                                assert_eq!(entry.boot_id, clock::boot_id().unwrap());
                                assert_eq!(record.text, "boot rotation continuity");
                                assert_eq!(event.event_id, 1);
                                event.producer_sequence.0
                            })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        let mut provider = 0;
        unsafe {
            assert_eq!(
                EventRegister(&etw::KERNEL_PROVIDER, None, std::ptr::null(), &mut provider),
                0
            );
        }
        let descriptor = EVENT_DESCRIPTOR {
            Id: 1,
            Version: 1,
            Level: 4,
            Keyword: 1,
            ..Default::default()
        };
        for round in 0..3u64 {
            let round_start = history.range().unwrap().high_water;
            for sequence in round * 8 + 1..=round * 8 + 8 {
                let mut payload = Vec::new();
                payload.extend_from_slice(&instance.to_le_bytes());
                payload.extend_from_slice(&sequence.to_le_bytes());
                payload.extend_from_slice(&clock::filetime().to_le_bytes());
                payload.extend_from_slice(&[1, 0, 1, 4]);
                payload.extend_from_slice(&[0; 8]);
                payload.extend_from_slice(b"boot-rotation\0boot rotation continuity\0");
                let data = EVENT_DATA_DESCRIPTOR {
                    Ptr: payload.as_ptr() as u64,
                    Size: payload.len() as u32,
                    ..Default::default()
                };
                unsafe {
                    assert_eq!(EventWrite(provider, &descriptor, 1, &data), 0);
                }
            }
            import_sealed(&state, &history, &capture, &sessions).unwrap();
            assert_eq!(
                markers(&rotation_records_after(&history, &baseline)),
                (1..=round * 8).collect::<Vec<_>>()
            );
            let sealed = sessions[0].active.clone();
            assert!(sealed.exists());
            let requested = new_path(&capture, "kernel-live-").unwrap();
            let updated = sessions[0].session.rotate(&requested);
            println!("UPDATE request={} result={updated:?}", requested.display());
            sessions[0].active = updated.unwrap();
            let (query, actual) = FileSession::attach(PRIMARY, &capture).unwrap();
            let observed = query.control_status().unwrap();
            println!("QUERY actual={} control={observed:?}", actual.display());
            assert_eq!(observed, control);
            assert_eq!(sessions[0].active, actual);
            assert_ne!(actual, requested);
            assert_ne!(actual, sealed);
            let summary = files::read(&sealed, &mut |_, _, _| Ok(())).unwrap();
            assert!(summary.end_time_filetime.parse::<u64>().unwrap() > 0);
            assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
            let bytes = fs::metadata(&sealed).unwrap().len();
            preserve(&sessions);
            assert_eq!(fs::metadata(&sealed).unwrap().len(), bytes);
            let source = files::header(&sealed).unwrap();
            let started = Instant::now();
            import_sealed(&state, &history, &capture, &sessions).unwrap();
            println!(
                "round={round} sealed import elapsed_ms={}",
                started.elapsed().as_millis()
            );
            assert!(!sealed.exists());
            assert!(!history.import_sources().unwrap().contains(&source));
            assert!(actual.exists());
            let active_id = files::file_identity(&actual).unwrap();
            assert!(!history
                .import_sources()
                .unwrap()
                .iter()
                .any(|source| source.id.starts_with(&active_id)));
            let imported_through = history.range().unwrap().high_water;
            import_sealed(&state, &history, &capture, &sessions).unwrap();
            assert_eq!(history.range().unwrap().high_water, imported_through);
            let records = rotation_records_after(&history, &baseline);
            assert_eq!(markers(&records), (1..=round * 8 + 8).collect::<Vec<_>>());
            let gaps: Vec<_> = records
                .iter()
                .filter(|entry| entry.cursor.position.0 > round_start.0)
                .filter_map(|entry| {
                    if let venera_debug_protocol::v1::HistoryEvent::Gap { gap } = &entry.event {
                        Some(gap)
                    } else {
                        None
                    }
                })
                .collect();
            assert!(gaps.is_empty(), "rotation Gap observations: {gaps:?}");
        }
        unsafe {
            assert_eq!(EventUnregister(provider), 0);
        }
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        let journal = crate::journal::Journal::open(
            &history_path,
            "numbered-boot-reopen",
            crate::journal::RetentionPolicy {
                max_bytes: capture_storage::JOURNAL_BYTES,
                ..Default::default()
            },
        )
        .unwrap();
        let through = journal.range().unwrap().high_water;
        let mut after = Some(baseline);
        let mut reopened = Vec::new();
        loop {
            let page = journal
                .read(
                    venera_debug_protocol::v1::HistoryRead {
                        after,
                        through: Some(through),
                        limit: 256,
                    },
                    venera_debug_protocol::v1::MAX_FRAME_BYTES - 1024,
                )
                .unwrap();
            assert!(
                page.missing.is_empty(),
                "reopened rotation window: {:?}",
                page.missing
            );
            reopened.extend(page.records);
            if page.next.position.0 >= page.range.high_water.0 {
                break;
            }
            after = Some(page.next);
        }
        assert_eq!(
            markers(&reopened),
            [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24
            ]
        );
        fs::write(
            evidence.join("history.json"),
            serde_json::to_vec_pretty(&reopened).unwrap(),
        )
        .unwrap();
        journal.close().unwrap();
    }

    #[test]
    #[ignore = "Reads the owned GlobalLogger trace before the collector starts"]
    fn owned_global_session_adopts_and_seals_legacy_marker() {
        assert!(owns_legacy().unwrap(), "owned GlobalLogger configuration");
        check_boot_session("NT Kernel Logger", "legacy-live-", "legacy");
    }

    fn check_boot_session(name: &str, prefix: &str, output: &str) {
        let capture = root().unwrap();
        let evidence =
            std::env::temp_dir().join(format!("venera-boot-adoption-{}", clock::filetime()));
        fs::create_dir(&evidence).unwrap();
        println!("Boot adoption evidence: {}", evidence.display());
        let (mut session, original) = FileSession::attach(name, &capture).unwrap();
        let next = new_path(&capture, prefix).unwrap();
        session.rotate(&next).unwrap();
        let size = fs::metadata(&original).unwrap().len();
        let mut ids = Vec::new();
        let mut marker = false;
        let summary = files::read(&original, &mut |record, boot, _| {
            if let Some(capture) = unsafe { etw::decode_record(record, boot) }? {
                marker |= capture.text.contains("VIOGPU FULL build");
                if let Some(event) = capture.structured {
                    ids.push(event.event_id);
                }
            }
            Ok(())
        })
        .unwrap();
        fs::copy(&original, evidence.join(format!("{output}.etl"))).unwrap();
        files::inspect(
            &original,
            &mut fs::File::create(evidence.join(format!("{output}.jsonl"))).unwrap(),
        )
        .unwrap();
        assert_eq!(summary.boot_time_filetime, clock::boot_id().unwrap());
        assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
        assert_eq!(fs::metadata(&original).unwrap().len(), size);
        if output == "kernel" {
            assert!(
                ids.contains(&2) && ids.contains(&3),
                "initialization and registration events: {ids:?}"
            );
        } else {
            assert!(marker, "legacy boot marker");
        }
    }

    #[test]
    #[ignore = "Runs elevated Windows capture and durable import"]
    fn sealed_file_import_confirms_storage_and_resumes_the_session() {
        check_sealed_import(false);
    }

    #[test]
    #[ignore = "Runs elevated Windows capture recovery"]
    fn damaged_recorder_preserves_primary_import_and_releases_its_slot() {
        check_sealed_import(true);
    }

    fn check_sealed_import(damaged_recorder: bool) {
        let directory = std::env::temp_dir().join(format!("venera-import-{}", clock::filetime()));
        fs::create_dir(&directory).unwrap();
        println!("Capture evidence: {}", directory.display());
        let capture = directory.join("captures");
        fs::create_dir(&capture).unwrap();
        let history_path = directory.join("history");
        let journal =
            crate::journal::Journal::open(&history_path, "native-import", Default::default())
                .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        let state = Arc::new(Mutex::new(State::new("native-import".into(), true)));
        let first = capture.join("kernel-live-1.etl");
        let next = capture.join("kernel-live-2.etl");
        let name = format!("Venera import test {}", std::process::id());
        let mut session = FileSession::start(&name, &first, 0, 32).unwrap();
        let mut provider = 0;
        unsafe {
            assert_eq!(
                EventRegister(&etw::KERNEL_PROVIDER, None, std::ptr::null(), &mut provider),
                0
            );
        }
        session.enable(&etw::KERNEL_PROVIDER, 5, 1).unwrap();
        let boot: u64 = clock::boot_id().unwrap().parse().unwrap();
        state.lock().unwrap().boot_id = boot.to_string();
        let mut payload = Vec::new();
        payload.extend_from_slice(&42u64.to_le_bytes());
        payload.extend_from_slice(&1u64.to_le_bytes());
        payload.extend_from_slice(&(boot + 1).to_le_bytes());
        payload.extend_from_slice(&[2, 0, 1, 4]);
        payload.extend_from_slice(&[0; 8]);
        payload.extend_from_slice(b"DriverEntry\0native durable import\0");
        let descriptor = EVENT_DESCRIPTOR {
            Id: 2,
            Version: 1,
            Level: 4,
            Keyword: 1,
            ..Default::default()
        };
        let data = EVENT_DATA_DESCRIPTOR {
            Ptr: payload.as_ptr() as u64,
            Size: payload.len() as u32,
            ..Default::default()
        };
        unsafe {
            assert_eq!(EventWrite(provider, &descriptor, 1, &data), 0);
        }
        session.rotate(&next).unwrap();
        let source = files::header(&first).unwrap();
        let copy = directory.join("original.etl");
        fs::copy(&first, &copy).unwrap();
        let mut sessions = vec![CaptureSession {
            session,
            active: next.clone(),
            prefix: "kernel-live-",
        }];
        let damaged = capture.join("recorder-v1-00000000-0000-0000-0000-000000000000-partial.etl");
        if damaged_recorder {
            fs::write(&damaged, b"partial trace header").unwrap();
        }
        import_sealed(&state, &history, &capture, &sessions).unwrap();
        assert!(!first.exists());
        assert!(!damaged.exists());
        assert_eq!(
            history.range().unwrap().high_water.0,
            if damaged_recorder { 2 } else { 1 }
        );
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
        let journal =
            crate::journal::Journal::open(&history_path, "different", Default::default()).unwrap();
        let page = journal
            .read(
                venera_debug_protocol::v1::HistoryRead {
                    after: None,
                    through: None,
                    limit: 10,
                },
                100_000,
            )
            .unwrap();
        assert_eq!(page.records.len(), if damaged_recorder { 2 } else { 1 });
        let imported = page
            .records
            .iter()
            .find(|record| {
                matches!(
                    record.event,
                    venera_debug_protocol::v1::HistoryEvent::Log { .. }
                )
            })
            .unwrap();
        assert_eq!(imported.boot_id, source.boot_filetime.to_string());
        assert!(imported.startup);
        let venera_debug_protocol::v1::HistoryEvent::Log { record } = &imported.event else {
            panic!("kernel event")
        };
        assert_eq!(record.text, "native durable import");
        assert_eq!(
            record.structured.as_ref().unwrap().source_time_filetime.0,
            boot + 1
        );
        sessions[0].session.retain();
        drop(sessions);
        let (resumed, active) = FileSession::attach(&name, &capture).unwrap();
        assert_eq!(active.canonicalize().unwrap(), next.canonicalize().unwrap());
        resumed.stop().unwrap();
        unsafe {
            assert_eq!(EventUnregister(provider), 0);
        }
    }

    #[test]
    #[ignore = "Runs elevated Windows capacity recovery"]
    fn capacity_stop_recovers_primary_and_legacy_sessions() {
        for prefix in ["kernel-live-", "legacy-live-"] {
            let directory = std::env::temp_dir()
                .join(format!("venera-capacity-recovery-{}", clock::filetime()));
            let capture = directory.join("capture");
            fs::create_dir_all(&capture).unwrap();
            println!("Recovery evidence: {}", directory.display());
            let journal = crate::journal::Journal::open(
                directory.join("history"),
                "capacity",
                Default::default(),
            )
            .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
            let state = Arc::new(Mutex::new(State::new("capacity".into(), true)));
            state.lock().unwrap().boot_id = clock::boot_id().unwrap();
            let first = capture.join(format!("{prefix}first.etl"));
            let name = format!("Venera capacity recovery {}", clock::filetime());
            let session = FileSession::start(&name, &first, 0, 32).unwrap();
            let mut provider = 0;
            unsafe {
                assert_eq!(
                    EventRegister(&etw::KERNEL_PROVIDER, None, std::ptr::null(), &mut provider),
                    0
                );
            }
            session.enable(&etw::KERNEL_PROVIDER, 5, 1).unwrap();
            let boot: u64 = clock::boot_id().unwrap().parse().unwrap();
            let mut valid = Vec::new();
            valid.extend_from_slice(&42u64.to_le_bytes());
            valid.extend_from_slice(&1u64.to_le_bytes());
            valid.extend_from_slice(&(boot + 1).to_le_bytes());
            valid.extend_from_slice(&[2, 0, 1, 4]);
            valid.extend_from_slice(&[0; 8]);
            valid.extend_from_slice(b"DriverEntry\0retained before capacity\0");
            let valid_data = EVENT_DATA_DESCRIPTOR {
                Ptr: valid.as_ptr() as u64,
                Size: valid.len() as u32,
                ..Default::default()
            };
            let valid_descriptor = EVENT_DESCRIPTOR {
                Id: 2,
                Version: 1,
                Level: 4,
                Keyword: 1,
                ..Default::default()
            };
            unsafe {
                assert_eq!(EventWrite(provider, &valid_descriptor, 1, &valid_data), 0);
            }
            let payload = [1u8; 8192];
            let data = EVENT_DATA_DESCRIPTOR {
                Ptr: payload.as_ptr() as u64,
                Size: payload.len() as u32,
                ..Default::default()
            };
            let descriptor = EVENT_DESCRIPTOR {
                Id: 65535,
                Version: 1,
                Level: 4,
                Keyword: 1,
                ..Default::default()
            };
            for _ in 0..6000 {
                unsafe {
                    assert_eq!(EventWrite(provider, &descriptor, 1, &data), 0);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(session.running().unwrap(), false);
            fs::copy(&first, directory.join("capacity.etl")).unwrap();
            let mut sessions = vec![CaptureSession {
                session,
                active: first.clone(),
                prefix,
            }];
            rotate_sessions(&state, &history, &capture, &mut sessions).unwrap();
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].session.running().unwrap(), true);
            assert!(!first.exists());
            sessions[0]
                .session
                .enable(&etw::KERNEL_PROVIDER, 5, 1)
                .unwrap();
            valid[8..16].copy_from_slice(&2u64.to_le_bytes());
            valid[16..24].copy_from_slice(&(boot + 2).to_le_bytes());
            valid.truncate(36);
            valid.extend_from_slice(b"DriverEntry\0retained after restart\0");
            let continued = EVENT_DATA_DESCRIPTOR {
                Ptr: valid.as_ptr() as u64,
                Size: valid.len() as u32,
                ..Default::default()
            };
            unsafe {
                assert_eq!(EventWrite(provider, &valid_descriptor, 1, &continued), 0);
            }
            let sealed = sessions[0].active.clone();
            let next = new_path(&capture, prefix).unwrap();
            sessions[0].active = sessions[0].session.rotate(&next).unwrap();
            let mut markers = Vec::new();
            files::read(&sealed, &mut |event, boot, _| {
                if let Some(event) = unsafe { etw::decode_record(event, boot) }? {
                    if event
                        .structured
                        .as_ref()
                        .is_some_and(|event| event.producer_instance.0 == 42)
                    {
                        markers.push(event.text);
                    }
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(markers, ["retained after restart"]);
            fs::copy(&sealed, directory.join("replacement.etl")).unwrap();
            import_sealed(&state, &history, &capture, &sessions).unwrap();
            let records = history_records(&history);
            assert!(records.iter().any(|record| matches!(&record.event, venera_debug_protocol::v1::HistoryEvent::Gap { gap } if gap.reason.starts_with("capture_session_ended:"))));
            assert!(records.iter().any(|record| matches!(&record.event, venera_debug_protocol::v1::HistoryEvent::Gap { gap } if gap.reason.starts_with("capture_loss "))));
            assert!(records.iter().any(|entry| matches!(&entry.event, venera_debug_protocol::v1::HistoryEvent::Log { record } if record.text == "retained before capacity")));
            assert!(records.iter().any(|entry| matches!(&entry.event, venera_debug_protocol::v1::HistoryEvent::Log { record } if record.text == "retained after restart")));
            sessions.pop().unwrap().session.stop().unwrap();
            unsafe {
                assert_eq!(EventUnregister(provider), 0);
            }
            stop.store(true, Ordering::Release);
            owner.join().unwrap().unwrap();
        }
    }

    #[test]
    #[ignore = "Runs elevated Windows Recorder recovery"]
    fn interrupted_recorder_is_sealed_before_import() {
        let directory =
            std::env::temp_dir().join(format!("venera-recorder-recovery-{}", clock::filetime()));
        fs::create_dir(&directory).unwrap();
        let path =
            directory.join("recorder-v1-00000000-0000-0000-0000-000000000000-interrupted.etl");
        let name = format!("Venera Recorder replay {}", std::process::id());
        let mut session = FileSession::start(&name, &path, 0, 4).unwrap();
        session.retain();
        drop(session);
        let recovered = files::recover_recorders(&directory).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].canonicalize().unwrap(),
            path.canonicalize().unwrap()
        );
        let size = fs::metadata(&path).unwrap().len();
        assert!(size > 0);
        assert_eq!(files::recover_recorders(&directory).unwrap().len(), 0);
        assert_eq!(fs::metadata(&path).unwrap().len(), size);
        assert_eq!(
            files::header(&path).unwrap().boot_filetime.to_string(),
            clock::boot_id().unwrap()
        );
    }

    #[test]
    #[ignore = "Holds a native Recorder export for the operator interruption test"]
    fn recorder_export_interruption_fixture() {
        let directory = PathBuf::from(std::env::var_os("VENERA_RECOVERY_TEST_DIRECTORY").unwrap());
        fs::create_dir_all(&directory).unwrap();
        let guid = crate::kernel_events::RECORDER_TMF
            .split_whitespace()
            .next()
            .unwrap();
        let path = new_path(&directory, &format!("recorder-v1-{guid}-")).unwrap();
        let _session = files::begin_replay(&path).unwrap();
        println!(
            "Recorder export open: {}; process {}",
            path.display(),
            std::process::id()
        );
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    #[ignore = "Imports the operator-interrupted native Recorder export"]
    fn recorder_export_recovers_after_process_termination() {
        let directory = PathBuf::from(std::env::var_os("VENERA_RECOVERY_TEST_DIRECTORY").unwrap());
        let original = traces(&directory).unwrap();
        assert_eq!(original.len(), 1);
        let journal_path = directory.with_extension("history");
        let journal =
            crate::journal::Journal::open(&journal_path, "recorder-restart", Default::default())
                .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        let state = Arc::new(Mutex::new(State::new("recorder-restart".into(), true)));
        state.lock().unwrap().boot_id = clock::boot_id().unwrap();
        import_sealed(&state, &history, &directory, &[]).unwrap();
        assert_eq!(traces(&directory).unwrap().len(), 0);
        let range = history.range().unwrap();
        let records = history
            .read(venera_debug_protocol::v1::HistoryRead {
                after: Some(venera_debug_protocol::v1::HistoryCursor {
                    journal_id: range.journal_id,
                    position: venera_debug_protocol::v1::DecimalU64(
                        range.high_water.0.saturating_sub(1),
                    ),
                }),
                through: None,
                limit: 100,
            })
            .unwrap()
            .records;
        assert!(records.iter().any(|record| matches!(&record.event, venera_debug_protocol::v1::HistoryEvent::Gap { gap } if gap.reason.contains("recovered_export=true") || gap.reason.starts_with("capture_header_failed:"))));
        stop.store(true, Ordering::Release);
        owner.join().unwrap().unwrap();
    }
}

fn captured_through(path: &Path, boot: u64) -> io::Result<u64> {
    let mut last = 0;
    match files::read(path, &mut |event, event_boot, _| {
        if event_boot == boot {
            last = last.max(event.EventHeader.TimeStamp.max(0) as u64);
        }
        Ok(())
    }) {
        Ok(summary) if summary.boot_time_filetime == boot.to_string() => Ok(summary
            .end_time_filetime
            .parse::<u64>()
            .unwrap_or(0)
            .max(last)),
        Ok(_) => Ok(0),
        Err(error) if corrupt(&error) || error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn resume_primary(history: &crate::history::History, boot: u64, active: &Path) -> io::Result<()> {
    let source = files::header(active)?;
    let started = source
        .id
        .rsplit(':')
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|started| *started >= boot)
        .ok_or(io::ErrorKind::InvalidData)?;
    if source.boot_filetime != boot {
        return Err(io::ErrorKind::InvalidData.into());
    }
    history.process_capture(boot, 0, Some(started))
}

fn rotate_sessions(
    state: &Shared,
    history: &crate::history::History,
    directory: &Path,
    sessions: &mut Vec<CaptureSession>,
) -> io::Result<()> {
    sessions.sort_by_key(|session| session.prefix == "kernel-live-");
    let mut ended = Vec::new();
    for (index, session) in sessions.iter_mut().enumerate() {
        let rotation = if !session.session.running()? {
            Err(io::Error::from_raw_os_error(
                ERROR_WMI_INSTANCE_NOT_FOUND as i32,
            ))
        } else {
            new_path(directory, session.prefix).and_then(|next| {
                session.active = session.session.rotate(&next)?;
                Ok(())
            })
        };
        if let Err(error) = rotation {
            if error.raw_os_error() == Some(ERROR_WMI_INSTANCE_NOT_FOUND as i32) {
                let state = state.lock().unwrap();
                let entry = crate::journal::JournalInput {
                    boot_id: state.boot_id.clone(),
                    recorded_at_ms: now_ms(),
                    startup: false,
                    event: venera_debug_protocol::v1::HistoryEvent::Gap {
                        gap: venera_debug_protocol::v1::Gap {
                            stream_id: state.stream_id.clone(),
                            reason: format!("capture_session_ended: {}", session.prefix),
                            count: None,
                            first_seq: None,
                            last_seq: None,
                        },
                    },
                };
                drop(state);
                if session.prefix == "kernel-live-" {
                    let boot = entry
                        .boot_id
                        .parse()
                        .map_err(|_| io::ErrorKind::InvalidData)?;
                    history.process_capture(
                        boot,
                        captured_through(&session.active, boot)?,
                        None,
                    )?;
                }
                history.append_durable(entry)?;
                ended.push(index);
            } else if session.prefix == "kernel-live-" {
                return Err(error);
            } else {
                state.lock().unwrap().status.health["legacy_capture"] =
                    serde_json::json!({"error":error.to_string()});
                return Err(error);
            }
        }
    }
    let ended: Vec<_> = ended
        .into_iter()
        .rev()
        .map(|index| sessions.remove(index))
        .collect();
    import_sealed(state, history, directory, sessions)?;
    for old in ended {
        let result = (|| {
            let active = new_path(directory, old.prefix)?;
            let session = old.session.restart(&active)?;
            if old.prefix == "kernel-live-" {
                session.enable(&etw::KERNEL_PROVIDER, 5, 1)?;
                session.enable(&etw::PROCESS_PROVIDER, 4, 0x10)?;
                let boot = state
                    .lock()
                    .unwrap()
                    .boot_id
                    .parse()
                    .map_err(|_| io::ErrorKind::InvalidData)?;
                resume_primary(history, boot, &active)?;
            }
            Ok(CaptureSession {
                session,
                active,
                prefix: old.prefix,
            })
        })();
        match result {
            Ok(session) => sessions.push(session),
            Err(error) if old.prefix == "kernel-live-" => return Err(error),
            Err(error) => {
                state.lock().unwrap().status.health["legacy_capture"] =
                    serde_json::json!({"error":error.to_string()});
            }
        }
    }
    Ok(())
}

pub fn collect(state: Shared) {
    while !stopped() {
        let result = collect_once(&state);
        if let Err(error) = result {
            let mut state = state.lock().unwrap();
            state.status.health["kernel_events"] = serde_json::json!({"error":error.to_string()});
            state.notify();
        }
        for _ in 0..10 {
            if stopped() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn collect_once(state: &Shared) -> io::Result<()> {
    let directory = root()?;
    let history = state
        .lock()
        .unwrap()
        .history
        .clone()
        .ok_or(io::ErrorKind::NotConnected)?;
    // Identify running outputs before reclaiming completed handoff files after a restart.
    let mut running = Vec::new();
    let legacy_owned = match owns_legacy() {
        Ok(owned) => owned,
        Err(error) => {
            state.lock().unwrap().status.health["legacy_capture"] =
                serde_json::json!({"error":error.to_string()});
            false
        }
    };
    for (name, prefix) in [
        (PRIMARY, "kernel-live-"),
        ("NT Kernel Logger", "legacy-live-"),
        ("VeneraLegacyLog", "legacy-live-"),
    ] {
        if name != PRIMARY && !legacy_owned {
            continue;
        }
        match FileSession::attach(name, &directory) {
            Ok((session, active)) => running.push(CaptureSession {
                session,
                active,
                prefix,
            }),
            Err(error) if error.raw_os_error() == Some(ERROR_WMI_INSTANCE_NOT_FOUND as i32) => (),
            Err(error)
                if name != PRIMARY
                    && error.kind() == io::ErrorKind::PermissionDenied
                    && error.raw_os_error().is_none() =>
            {
                ()
            }
            Err(error) => return Err(error),
        }
    }
    let boot = state
        .lock()
        .unwrap()
        .boot_id
        .parse()
        .map_err(|_| io::ErrorKind::InvalidData)?;
    if let Some(primary) = running
        .iter()
        .find(|session| session.prefix == "kernel-live-")
    {
        resume_primary(&history, boot, &primary.active)?;
    } else {
        let mut through = 0;
        for path in traces(&directory)? {
            if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("kernel-")
            {
                through = through.max(captured_through(&path, boot)?);
            }
        }
        history.process_capture(boot, through, None)?;
    }
    import_sealed(state, &history, &directory, &running)?;
    drop(running);
    let mut sessions = vec![primary(&directory)?];
    resume_primary(&history, boot, &sessions[0].active)?;
    match legacy(&directory) {
        Ok(Some(session)) => sessions.push(session),
        Ok(None) => (),
        Err(error) => {
            state.lock().unwrap().status.health["legacy_capture"] =
                serde_json::json!({"error":error.to_string()});
        }
    }
    import_sealed(state, &history, &directory, &sessions)?;
    let guid = crate::kernel_events::RECORDER_TMF
        .split_whitespace()
        .next()
        .unwrap();
    let replay =
        new_path(&directory, &format!("recorder-v1-{guid}-")).and_then(|path| files::replay(&path));
    if let Err(error) = replay {
        state.lock().unwrap().status.health["recorder"] =
            serde_json::json!({"error":error.to_string()});
    }
    import_sealed(state, &history, &directory, &sessions)?;
    while !stopped() {
        let start = Instant::now();
        rotate_sessions(state, &history, &directory, &mut sessions)?;
        if !sessions
            .iter()
            .any(|session| session.prefix == "kernel-live-")
        {
            let primary = primary(&directory)?;
            resume_primary(&history, boot, &primary.active)?;
            sessions.push(primary);
        }
        if !sessions
            .iter()
            .any(|session| session.prefix == "legacy-live-")
        {
            match legacy(&directory) {
                Ok(Some(session)) => sessions.push(session),
                Ok(None) => (),
                Err(error) => {
                    state.lock().unwrap().status.health["legacy_capture"] =
                        serde_json::json!({"error":error.to_string()});
                }
            }
        }
        {
            let mut state = state.lock().unwrap();
            state.status.health["kernel_events"] =
                serde_json::json!({"state":"collecting", "import_ms":start.elapsed().as_millis()});
        }
        while !stopped() && start.elapsed() < Duration::from_millis(250) {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Ok(())
}
