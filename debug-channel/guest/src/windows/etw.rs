use super::*;
pub(super) mod files;
use windows_sys::{core::GUID, Win32::System::Diagnostics::Etw::*};
pub(super) const PROVIDER: GUID = GUID::from_u128(0x13976d09_a327_438c_950b_7f03192815c7);
pub(super) const PROCESS_PROVIDER: GUID = GUID::from_u128(0x22fb2cd6_0e7b_422b_a0c7_2fad1fd0e716);
const NAME: &str = "Venera Debug Guest kernel messages";

pub fn diagnostic(args: &[String]) -> io::Result<()> {
    match args {
        [command, path] if command == "inspect" => {
            files::inspect(std::path::Path::new(path), &mut io::stdout().lock())
        }
        [command, path] if command == "recorder" => files::replay(std::path::Path::new(path)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "trace inspect ETL | trace recorder ETL",
        )),
    }
}
pub const KERNEL_PROVIDER: GUID = GUID::from_u128(0x6c5a2378_22b9_4c10_a91f_69e34c280b81);
pub const RECORDER_PROVIDER: GUID = GUID::from_u128(0x88ff4af9_b7b4_40ff_94ad_271b4777ed97);

pub(super) fn recorder_message() -> GUID {
    let text = crate::kernel_events::RECORDER_TMF
        .split_whitespace()
        .next()
        .unwrap();
    GUID::from_u128(u128::from_str_radix(&text.replace('-', ""), 16).unwrap())
}

fn recorder_message_id() -> u16 {
    crate::kernel_events::RECORDER_TMF
        .lines()
        .find(|line| line.starts_with("#typev "))
        .and_then(|line| line.split_whitespace().nth(2))
        .unwrap()
        .parse()
        .unwrap()
}

fn same_guid(a: GUID, b: GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

fn guid_text(guid: GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7]
    )
}
unsafe fn property(record: &EVENT_RECORD, name: &str) -> Option<Vec<u8>> {
    let name = wide(name);
    let descriptor = PROPERTY_DATA_DESCRIPTOR {
        PropertyName: name.as_ptr() as u64,
        ArrayIndex: u32::MAX,
        Reserved: 0,
    };
    let mut size = 0;
    if TdhGetPropertySize(record, 0, std::ptr::null(), 1, &descriptor, &mut size) != 0
        || size > 65536
    {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    (TdhGetProperty(
        record,
        0,
        std::ptr::null(),
        1,
        &descriptor,
        size,
        buffer.as_mut_ptr(),
    ) == 0)
        .then_some(buffer)
}
unsafe fn number(record: &EVENT_RECORD, name: &str) -> Option<u64> {
    let bytes = property(record, name)?;
    match bytes.len() {
        4 => Some(u32::from_le_bytes(bytes.try_into().ok()?) as u64),
        8 => Some(u64::from_le_bytes(bytes.try_into().ok()?)),
        _ => None,
    }
}
pub(super) unsafe fn process_start_key(record: &EVENT_RECORD) -> io::Result<Option<u64>> {
    if record.ExtendedDataCount == 0 {
        return Ok(None);
    }
    if record.ExtendedData.is_null() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    for data in std::slice::from_raw_parts(record.ExtendedData, record.ExtendedDataCount as usize) {
        if data.ExtType as u32 == EVENT_HEADER_EXT_TYPE_PROCESS_START_KEY {
            if data.DataSize != 8 || data.DataPtr == 0 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            return Ok(Some(std::ptr::read_unaligned(data.DataPtr as *const u64)));
        }
    }
    Ok(None)
}

pub(super) unsafe fn process_lifecycle(
    record: &EVENT_RECORD,
    boot: u64,
) -> io::Result<Option<crate::ancestry::Lifecycle>> {
    use crate::ancestry::Lifecycle;
    if !same_guid(record.EventHeader.ProviderId, PROCESS_PROVIDER)
        || ![1, 2].contains(&record.EventHeader.EventDescriptor.Id)
    {
        return Ok(None);
    }
    let pid = number(record, "ProcessID")
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(io::ErrorKind::InvalidData)?;
    let start = number(record, "CreateTime")
        .filter(|start| *start >= boot)
        .ok_or(io::ErrorKind::InvalidData)?;
    Ok(Some(match record.EventHeader.EventDescriptor.Id {
        1 => {
            let parent = number(record, "ParentProcessID")
                .and_then(|n| u32::try_from(n).ok())
                .ok_or(io::ErrorKind::InvalidData)?;
            let session = number(record, "SessionID").and_then(|n| u32::try_from(n).ok());
            let path = property(record, "ImageName").and_then(|bytes| {
                if bytes.len() % 2 != 0 {
                    return None;
                }
                let utf16: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .take_while(|c| *c != 0)
                    .collect();
                String::from_utf16(&utf16).ok()
            });
            let identity = venera_debug_protocol::ProcessIdentity {
                pid,
                start_time_filetime: Some(start.to_string()),
                exe_path: path,
                boot_time_filetime: Some(boot.to_string()),
                start_key: if record.EventHeader.ProcessId == pid {
                    process_start_key(record)?.map(|key| key.to_string())
                } else {
                    None
                },
            };
            Lifecycle::Start {
                identity,
                parent,
                session,
                parent_key: if record.EventHeader.ProcessId == parent {
                    process_start_key(record)?
                } else {
                    None
                },
            }
        }
        2 => {
            let end = number(record, "ExitTime")
                .filter(|end| *end >= start)
                .ok_or(io::ErrorKind::InvalidData)?;
            Lifecycle::End {
                pid,
                start,
                end,
                key: if record.EventHeader.ProcessId == pid {
                    process_start_key(record)?
                } else {
                    None
                },
            }
        }
        _ => unreachable!(),
    }))
}

unsafe fn process_event(record: &EVENT_RECORD, state: &Shared) -> bool {
    let mut state = state.lock().unwrap();
    let boot = state.boot_id.parse().unwrap_or(0);
    if let Ok(Some(key)) = process_start_key(record) {
        state.processes.apply(crate::ancestry::Lifecycle::Context {
            pid: record.EventHeader.ProcessId,
            key,
            time: record.EventHeader.TimeStamp as u64,
        });
    }
    match process_lifecycle(record, boot) {
        Ok(Some(mut change)) => {
            if let crate::ancestry::Lifecycle::Start { identity, .. } = &mut change {
                identity.exe_path = identity.exe_path.take().map(process::dos_path);
            }
            state.processes.apply(change)
        }
        Ok(None) => true,
        Err(_) => false,
    }
}

unsafe extern "system" fn event(record: *mut EVENT_RECORD) {
    let record = &*record;
    let id = record.EventHeader.ProviderId;
    let state = &*(record.UserContext as *const Shared);
    if same_guid(id, PROCESS_PROVIDER) {
        if !process_event(record, state) {
            let mut state = state.lock().unwrap();
            let count = state.status.loss["process_event_parse_errors"]
                .as_u64()
                .unwrap_or(0)
                + 1;
            state.status.loss["process_event_parse_errors"] = serde_json::json!(count);
        }
        return;
    }
    let boot = state.lock().unwrap().boot_id.parse().unwrap_or(0);
    match decode_record(record, boot) {
        Ok(Some(capture)) => accept(state, capture),
        Ok(None) => (),
        Err(error) => {
            let mut state = state.lock().unwrap();
            let count = state.status.loss["kernel_event_parse_errors"]
                .as_u64()
                .unwrap_or(0)
                + 1;
            state.status.loss["kernel_event_parse_errors"] = serde_json::json!(count);
            state.status.health["kernel_decode"] = serde_json::json!({"error":error.to_string()});
        }
    }
}

pub unsafe fn decode_record(record: &EVENT_RECORD, boot: u64) -> io::Result<Option<Capture>> {
    decode_record_from(record, boot, recorder_message())
}

pub(super) unsafe fn decode_record_from(
    record: &EVENT_RECORD,
    boot: u64,
    recorder_guid: GUID,
) -> io::Result<Option<Capture>> {
    let provider = record.EventHeader.ProviderId;
    let modern = same_guid(provider, KERNEL_PROVIDER);
    let recorder = same_guid(provider, recorder_guid);
    let legacy = same_guid(provider, PROVIDER);
    if !modern && !recorder && !legacy {
        return Ok(None);
    }
    if record.UserData.is_null() || record.UserDataLength == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Kernel event payload is empty",
        ));
    }
    let bytes =
        std::slice::from_raw_parts(record.UserData as *const u8, record.UserDataLength as usize);
    let pid = (record.EventHeader.ProcessId != u32::MAX && record.EventHeader.ProcessId != 0)
        .then_some(record.EventHeader.ProcessId);
    let tid = (record.EventHeader.ThreadId != u32::MAX && record.EventHeader.ThreadId != 0)
        .then_some(record.EventHeader.ThreadId);
    let observed_filetime = clock::filetime();
    let context_key = process_start_key(record)?;
    if modern && matches!(record.EventHeader.EventDescriptor.Id, 5 | 6) {
        let descriptor = record.EventHeader.EventDescriptor;
        let decoded =
            crate::user_events::decode(bytes, descriptor.Id, descriptor.Version, descriptor.Level)?;
        let pid = pid.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "User-mode event process is missing",
            )
        })?;
        let mut structured = decoded.structured(boot, pid);
        use venera_debug_protocol::v1::{DecimalU64, EventValue};
        structured.fields.insert(
            "transport_time_filetime".into(),
            EventValue::Uint64(DecimalU64(record.EventHeader.TimeStamp as u64)),
        );
        if let Some(key) = context_key {
            structured.fields.insert(
                "transport_process_start_key".into(),
                EventValue::Uint64(DecimalU64(key)),
            );
        }
        return Ok(Some(Capture {
            pid: Some(pid),
            tid,
            session_id: None,
            observed_at: clock::iso_filetime(observed_filetime),
            observed_filetime,
            source_time: Some(clock::iso_filetime(decoded.source_time)),
            source: "etw".into(),
            text: dbwin::decode_ansi(&decoded.message),
            raw_level: Some(decoded.level_name().into()),
            component: Some(decoded.component.into()),
            structured: Some(structured),
            process: Some(venera_debug_protocol::ProcessIdentity {
                pid,
                start_time_filetime: Some(decoded.created.to_string()),
                boot_time_filetime: Some(boot.to_string()),
                start_key: context_key.map(|key| key.to_string()),
                exe_path: None,
            }),
        }));
    }
    if modern || recorder {
        if recorder && record.EventHeader.EventDescriptor.Id != recorder_message_id() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unknown Recorder message ID",
            ));
        }
        let decoded = if modern {
            crate::kernel_events::decode(bytes)?
        } else {
            crate::kernel_events::decode_recorder(bytes)?
        };
        if modern
            && (record.EventHeader.EventDescriptor.Id != decoded.event_id
                || record.EventHeader.EventDescriptor.Version != decoded.version)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Kernel event descriptor differs from its payload",
            ));
        }
        let mut structured = decoded.structured(boot, dbwin::decode_ansi(decoded.function));
        use venera_debug_protocol::v1::{DecimalU64, EventValue};
        structured.fields.insert(
            "transport_provider".into(),
            EventValue::AnsiString(guid_text(provider)),
        );
        structured.fields.insert(
            "transport_event_id".into(),
            EventValue::Uint16(record.EventHeader.EventDescriptor.Id),
        );
        structured.fields.insert(
            "transport_version".into(),
            EventValue::Uint8(record.EventHeader.EventDescriptor.Version),
        );
        structured.fields.insert(
            "transport_time_filetime".into(),
            EventValue::Uint64(DecimalU64(record.EventHeader.TimeStamp as u64)),
        );
        if let Some(pid) = pid {
            structured
                .fields
                .insert("transport_process_id".into(), EventValue::Uint32(pid));
        }
        if modern {
            if let Some(key) = process_start_key(record)? {
                structured.fields.insert(
                    "transport_process_start_key".into(),
                    EventValue::Uint64(DecimalU64(key)),
                );
            }
        }
        return Ok(Some(Capture {
            pid: None,
            tid,
            session_id: None,
            observed_at: clock::iso_filetime(observed_filetime),
            observed_filetime,
            source_time: Some(clock::iso_filetime(decoded.source_time)),
            source: if recorder { "wpp_recorder" } else { "etw" }.into(),
            text: dbwin::decode_ansi(decoded.message),
            raw_level: Some(decoded.level_name().into()),
            process: None,
            component: Some("kernel".into()),
            structured: Some(structured),
        }));
    }
    if bytes.len() <= 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "DebugPrint event payload is incomplete",
        ));
    }
    let raw = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let body = &bytes[8..];
    let length = body
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(body.len());
    Ok(Some(Capture {
        pid,
        tid,
        session_id: None,
        observed_at: clock::iso_filetime(observed_filetime),
        observed_filetime,
        source_time: Some(clock::iso_filetime(record.EventHeader.TimeStamp as u64)),
        source: "etw".into(),
        text: dbwin::decode_ansi(&body[..length]),
        raw_level: Some(raw.to_string()),
        process: pid.map(|pid| venera_debug_protocol::ProcessIdentity {
            pid,
            start_time_filetime: None,
            exe_path: None,
            boot_time_filetime: Some(boot.to_string()),
            start_key: context_key.map(|key| key.to_string()),
        }),
        component: None,
        structured: None,
    }))
}

pub fn collect(state: Shared) {
    while !stopped() {
        collect_once(state.clone());
        for _ in 0..10 {
            if stopped() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

fn collect_once(state: Shared) {
    unsafe {
        let name = wide(NAME);
        let size = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name.len() * 2;
        let mut storage = vec![0u64; size.div_ceil(8)];
        let props = storage.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
        (*props).Wnode.BufferSize = size as u32;
        (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        (*props).Wnode.ClientContext = 2;
        (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        (*props).LogFileMode = EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_SYSTEM_LOGGER_MODE;
        (*props).EnableFlags = 0;
        (*props).FlushTimer = 1;
        let mut session = CONTROLTRACE_HANDLE { Value: 0 };
        let initial = storage.clone();
        let mut code = StartTraceW(&mut session, name.as_ptr(), props);
        if code == ERROR_ALREADY_EXISTS {
            // SCM owns this fixed guest trace name across service restarts.
            let stopped = ControlTraceW(
                CONTROLTRACE_HANDLE { Value: 0 },
                name.as_ptr(),
                props,
                EVENT_TRACE_CONTROL_STOP,
            );
            if stopped == 0 {
                storage.copy_from_slice(&initial);
                code = StartTraceW(&mut session, name.as_ptr(), props);
            }
        }
        if code != 0 {
            let mut s = state.lock().unwrap();
            s.status.health["etw"] = serde_json::json!({"error":code});
            s.status.health["process_events"] = serde_json::json!("stopped");
            s.notify();
            return;
        }
        let params = ENABLE_TRACE_PARAMETERS {
            Version: ENABLE_TRACE_PARAMETERS_VERSION_2,
            EnableProperty: EVENT_ENABLE_PROPERTY_PROCESS_START_KEY,
            ..Default::default()
        };
        let process_code = EnableTraceEx2(
            session,
            &PROCESS_PROVIDER,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER,
            TRACE_LEVEL_INFORMATION as u8,
            0x10,
            0,
            0,
            &params,
        );
        {
            let mut s = state.lock().unwrap();
            s.status.health["process_events"] = if process_code == 0 {
                serde_json::json!("collecting")
            } else {
                serde_json::json!({"error":process_code})
            };
        }
        process::update_ancestry(&state);
        let mut logfile: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
        logfile.LoggerName = name.as_ptr() as *mut u16;
        logfile.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
        logfile.Anonymous2.EventRecordCallback = Some(event);
        logfile.Context = (&state as *const Shared).cast_mut().cast();
        let trace = OpenTraceW(&mut logfile);
        if trace.Value == u64::MAX {
            {
                let mut s = state.lock().unwrap();
                s.status.health["etw"] = serde_json::json!({"error":error().to_string()});
                s.status.health["process_events"] = serde_json::json!("stopped");
                s.notify();
            }
            ControlTraceW(session, name.as_ptr(), props, EVENT_TRACE_CONTROL_STOP);
            return;
        }
        {
            let mut s = state.lock().unwrap();
            s.status.health["etw"] = serde_json::json!("collecting");
            s.notify();
        }
        let trace_value = trace.Value;
        let reader = std::thread::spawn(move || {
            let trace = PROCESSTRACE_HANDLE { Value: trace_value };
            ProcessTrace(&trace, 1, std::ptr::null(), std::ptr::null())
        });
        let mut maintenance = std::time::Instant::now();
        let mut reported_loss = 0;
        while !stopped() && !reader.is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if maintenance.elapsed() < std::time::Duration::from_secs(5) {
                continue;
            }
            maintenance = std::time::Instant::now();
            if ControlTraceW(session, name.as_ptr(), props, EVENT_TRACE_CONTROL_QUERY) == 0 {
                retain_loss(&state, &mut reported_loss, (*props).EventsLost);
            }
            let mut s = state.lock().unwrap();
            let boundary = clock::filetime().saturating_sub(60 * 10_000_000);
            s.processes.retire_before(boundary);
        }
        if ControlTraceW(session, name.as_ptr(), props, EVENT_TRACE_CONTROL_STOP) == 0 {
            retain_loss(&state, &mut reported_loss, (*props).EventsLost);
        }
        CloseTrace(trace);
        let outcome = reader.join();
        let mut s = state.lock().unwrap();
        s.status.health["etw"] = match outcome {
            Ok(0) if stopped() => serde_json::json!("stopped"),
            Ok(0) => serde_json::json!({"error":"Event collection ended"}),
            Ok(code) => serde_json::json!({"error":code}),
            Err(_) => serde_json::json!({"error":"Event collection thread failed"}),
        };
        s.status.health["process_events"] = serde_json::json!("stopped");
        s.notify();
    }
}

fn retain_loss(state: &Shared, previous: &mut u32, total: u32) {
    let gap = {
        let mut state = state.lock().unwrap();
        state.status.loss["etw_events_lost"] = serde_json::json!(total);
        crate::collector::etw_loss(&state.stream_id, previous, total)
    };
    if let Some(gap) = gap {
        retain_event(state, gap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_payload(id: u16, created: u64, sequence: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&134393633912317080u64.to_le_bytes());
        bytes.extend_from_slice(&created.to_le_bytes());
        bytes.extend_from_slice(&sequence.to_le_bytes());
        bytes.extend_from_slice(if id == 5 {
            b"venera_d3d11\0"
        } else {
            b"venera_d3d12\0"
        });
        bytes.extend_from_slice(b"Draw:41 repeated message\0");
        bytes
    }

    unsafe fn user_capture(
        id: u16,
        pid: u32,
        created: u64,
        sequence: u64,
        flags: u16,
        key: Option<u64>,
    ) -> io::Result<Capture> {
        let bytes = user_payload(id, created, sequence);
        let mut event: EVENT_RECORD = std::mem::zeroed();
        event.EventHeader.ProviderId = KERNEL_PROVIDER;
        event.EventHeader.ProcessId = pid;
        event.EventHeader.ThreadId = 77;
        event.EventHeader.TimeStamp = 134393633912317090;
        event.EventHeader.Flags = flags;
        event.EventHeader.EventDescriptor = EVENT_DESCRIPTOR {
            Id: id,
            Version: 1,
            Level: 3,
            Keyword: 1,
            ..Default::default()
        };
        event.UserData = bytes.as_ptr().cast_mut().cast();
        event.UserDataLength = bytes.len() as u16;
        let raw_key = key.unwrap_or(0);
        let mut extension = EVENT_HEADER_EXTENDED_DATA_ITEM {
            ExtType: EVENT_HEADER_EXT_TYPE_PROCESS_START_KEY as u16,
            DataSize: 8,
            DataPtr: (&raw_key as *const u64) as u64,
            ..Default::default()
        };
        if key.is_some() {
            event.ExtendedDataCount = 1;
            event.ExtendedData = &mut extension;
        }
        decode_record(&event, 134393633875000000)?.ok_or(io::ErrorKind::InvalidData.into())
    }

    #[test]
    fn user_log_layout_preserves_module_process_time_and_body_on_both_architectures() {
        for flags in [
            EVENT_HEADER_FLAG_32_BIT_HEADER as u16,
            EVENT_HEADER_FLAG_64_BIT_HEADER as u16,
        ] {
            for (id, component) in [(5, "venera_d3d11"), (6, "venera_d3d12")] {
                let capture = unsafe {
                    user_capture(
                        id,
                        42,
                        134393633875000100,
                        9007199254740997,
                        flags,
                        Some(987),
                    )
                }
                .unwrap();
                assert_eq!(capture.component.as_deref(), Some(component));
                assert_eq!(capture.raw_level.as_deref(), Some("warning"));
                assert_eq!(capture.text, "Draw:41 repeated message");
                assert_eq!(capture.pid, Some(42));
                assert_eq!(capture.tid, Some(77));
                assert_eq!(
                    capture.source_time.as_deref(),
                    Some("2026-11-17T04:29:51.2317080Z")
                );
                let identity = capture.process.unwrap();
                assert_eq!(
                    identity.start_time_filetime.as_deref(),
                    Some("134393633875000100")
                );
                assert_eq!(
                    identity.boot_time_filetime.as_deref(),
                    Some("134393633875000000")
                );
                assert_eq!(identity.start_key.as_deref(), Some("987"));
                let event = capture.structured.unwrap();
                assert_eq!(event.event_id, id);
                assert_eq!(event.producer_sequence.0, 9007199254740997);
                assert_eq!(event.source_time_filetime.0, 134393633912317080);
            }
        }
    }

    #[test]
    fn decoded_user_logs_deduplicate_by_module_and_process_after_history_reopen() {
        use crate::journal::{ImportBatch, ImportEntry, ImportSource};
        use venera_debug_protocol::v1::{HistoryEvent, HistoryRead};
        let boot = 134393633875000000;
        let time = 134393633912317080;
        let now = (time - 116444736000000000) / 10000;
        let path = std::env::temp_dir().join(format!("venera-user-records-{}", clock::filetime()));
        let mut journal =
            crate::journal::Journal::open(&path, "user-records", Default::default()).unwrap();
        let source = ImportSource {
            id: "user-file-one".into(),
            boot_filetime: boot,
        };
        journal.discover_imports(vec![source.clone()]).unwrap();
        let parent = venera_debug_protocol::ProcessIdentity {
            pid: 20,
            start_time_filetime: Some((boot + 50).to_string()),
            boot_time_filetime: Some(boot.to_string()),
            start_key: Some("888".into()),
            exe_path: Some("C:\\parent.exe".into()),
        };
        let child = venera_debug_protocol::ProcessIdentity {
            pid: 42,
            start_time_filetime: Some((boot + 100).to_string()),
            boot_time_filetime: Some(boot.to_string()),
            start_key: Some("987".into()),
            exe_path: Some("C:\\child.exe".into()),
        };
        journal
            .checkpoint_processes(
                &source,
                vec![
                    crate::ancestry::Lifecycle::Start {
                        identity: parent.clone(),
                        parent: 0,
                        parent_key: None,
                        session: Some(1),
                    },
                    crate::ancestry::Lifecycle::Start {
                        identity: child.clone(),
                        parent: 20,
                        parent_key: Some(888),
                        session: Some(1),
                    },
                    crate::ancestry::Lifecycle::End {
                        pid: 20,
                        start: boot + 50,
                        end: time - 1,
                        key: Some(888),
                    },
                    crate::ancestry::Lifecycle::End {
                        pid: 42,
                        start: boot + 100,
                        end: time + 1,
                        key: Some(987),
                    },
                ],
                time + 2,
                now,
            )
            .unwrap();
        let mut state = State::new("user-records".into(), true);
        let mut entries = Vec::new();
        for (id, pid, created, sequence) in [
            (5, 42, boot + 100, 1),
            (6, 42, boot + 100, 1),
            (5, 43, boot + 100, 1),
            (5, 42, boot + 100, 2),
            (5, 42, boot + 200, 1),
        ] {
            let capture = unsafe {
                user_capture(
                    id,
                    pid,
                    created,
                    sequence,
                    EVENT_HEADER_FLAG_64_BIT_HEADER as u16,
                    None,
                )
            }
            .unwrap();
            let class = crate::collector::classify_capture(&capture).unwrap();
            let mut record = state.capture_record(capture, Some(class)).unwrap();
            assert_eq!(
                journal.process_checkpoint(boot).unwrap().attribute_record(
                    &mut record,
                    boot,
                    time,
                    true
                ),
                crate::ancestry::Attribution::Ready
            );
            entries.push(ImportEntry {
                ordinal: entries.len() as u64 + 1,
                source_filetime: time,
                event: HistoryEvent::Log { record },
            });
        }
        journal
            .import_batch(ImportBatch {
                source: source.clone(),
                through: 5,
                complete: true,
                entries,
                now_ms: now,
            })
            .unwrap();
        drop(journal);
        let mut journal =
            crate::journal::Journal::open(&path, "user-records", Default::default()).unwrap();
        let second = ImportSource {
            id: "user-file-two".into(),
            boot_filetime: boot,
        };
        journal.discover_imports(vec![second.clone()]).unwrap();
        let capture = unsafe {
            user_capture(
                5,
                42,
                boot + 100,
                1,
                EVENT_HEADER_FLAG_32_BIT_HEADER as u16,
                Some(987),
            )
        }
        .unwrap();
        let class = crate::collector::classify_capture(&capture).unwrap();
        let record = state.capture_record(capture, Some(class)).unwrap();
        journal
            .import_batch(ImportBatch {
                source: source.clone(),
                through: 5,
                complete: true,
                entries: vec![ImportEntry {
                    ordinal: 1,
                    source_filetime: time,
                    event: HistoryEvent::Log {
                        record: record.clone(),
                    },
                }],
                now_ms: now,
            })
            .unwrap();
        journal
            .import_batch(ImportBatch {
                source: second,
                through: 1,
                complete: true,
                entries: vec![ImportEntry {
                    ordinal: 1,
                    source_filetime: time,
                    event: HistoryEvent::Log { record },
                }],
                now_ms: now,
            })
            .unwrap();
        let page = journal
            .read(
                HistoryRead {
                    after: None,
                    through: None,
                    limit: 100,
                },
                100000,
            )
            .unwrap();
        assert_eq!(page.records.len(), 5);
        let HistoryEvent::Log { record } = &page.records[0].event else {
            panic!("user log")
        };
        assert_eq!(record.process.as_ref(), Some(&child));
        assert_eq!(record.ancestors, [parent]);
        assert_eq!(record.component, "venera_d3d11");
        assert_eq!(record.text, "Draw:41 repeated message");
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn extended_start_key_preserves_source_identity_and_kernel_execution_context() {
        let key = 9007199254740997u64;
        let mut extension = EVENT_HEADER_EXTENDED_DATA_ITEM {
            ExtType: EVENT_HEADER_EXT_TYPE_PROCESS_START_KEY as u16,
            DataSize: 8,
            DataPtr: (&key as *const u64) as u64,
            ..Default::default()
        };
        let payload = b"\0\0\0\0\x04\0\0\0venera_d3d12[42:1] I message\0";
        let mut event: EVENT_RECORD = unsafe { std::mem::zeroed() };
        event.EventHeader.ProviderId = PROVIDER;
        event.EventHeader.ProcessId = 42;
        event.EventHeader.TimeStamp = 134393633912317080;
        event.ExtendedDataCount = 1;
        event.ExtendedData = &mut extension;
        event.UserData = payload.as_ptr().cast_mut().cast();
        event.UserDataLength = payload.len() as u16;
        let capture = unsafe { decode_record(&event, 134393633875000000) }
            .unwrap()
            .unwrap();
        assert_eq!(
            capture.process.unwrap().start_key.as_deref(),
            Some("9007199254740997")
        );
        extension.DataSize = 4;
        assert_eq!(
            unsafe { decode_record(&event, 134393633875000000) }
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        extension.DataSize = 8;
        let payload = b"\x01\0\0\0\0\0\0\0\x02\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\x03\0\x01\x04\0\0\0\0\0\0\0\0DxgkInitialize\0Registered\0";
        event.EventHeader.ProviderId = KERNEL_PROVIDER;
        event.EventHeader.EventDescriptor.Id = 3;
        event.EventHeader.EventDescriptor.Version = 1;
        event.UserData = payload.as_ptr().cast_mut().cast();
        event.UserDataLength = payload.len() as u16;
        let capture = unsafe { decode_record(&event, 1) }.unwrap().unwrap();
        assert_eq!(capture.process, None);
        assert_eq!(capture.pid, None);
        assert_eq!(
            capture.structured.unwrap().fields["transport_process_start_key"],
            venera_debug_protocol::v1::EventValue::Uint64(venera_debug_protocol::v1::DecimalU64(
                9007199254740997
            ))
        );
        event.EventHeader.ProviderId = recorder_message();
        event.EventHeader.EventDescriptor.Id = 10;
        let capture = unsafe { decode_record(&event, 1) }.unwrap().unwrap();
        assert_eq!(capture.process, None);
        assert_eq!(capture.structured.unwrap().event_id, 3);
    }

    #[test]
    fn recorder_descriptor_selects_the_paired_message_layout() {
        let payload = b"\x01\0\0\0\0\0\0\0\x02\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\x03\0\x01\x04\0\0\0\0\0\0\0\0DxgkInitialize\0Registered\0";
        let mut event: EVENT_RECORD = unsafe { std::mem::zeroed() };
        event.EventHeader.ProviderId = recorder_message();
        event.EventHeader.EventDescriptor.Id = 10;
        event.UserData = payload.as_ptr().cast_mut().cast();
        event.UserDataLength = payload.len() as u16;
        let capture = unsafe { decode_record(&event, 1) }.unwrap().unwrap();
        assert_eq!(capture.text, "Registered");
        assert_eq!(capture.structured.unwrap().event_id, 3);
        event.EventHeader.EventDescriptor.Id = 11;
        assert_eq!(
            unsafe { decode_record(&event, 1) }.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn default_component_dbgprint_becomes_a_kernel_log_record() {
        // DbgPrint uses DPFLTR_DEFAULT_ID and DPFLTR_INFO_LEVEL.
        let payload = b"\x65\0\0\0\x03\0\0\0VioGpu: DxgkInitialize failed with Status: 0xC0000001\n\0";
        let mut event: EVENT_RECORD = unsafe { std::mem::zeroed() };
        event.EventHeader.ProviderId = PROVIDER;
        event.EventHeader.TimeStamp = 134393633912317080;
        event.EventHeader.ProcessId = 4;
        event.UserData = payload.as_ptr().cast_mut().cast();
        event.UserDataLength = payload.len() as u16;
        let capture = unsafe { decode_record(&event, 134393633875000000) }.unwrap().unwrap();
        let class = crate::collector::classify_capture(&capture).unwrap();
        let mut state = crate::state::State::new("dbgprint".into(), true);
        let record = state.capture_record(capture, Some(class)).unwrap();
        assert_eq!(record.source, "etw");
        assert_eq!(record.component, "kernel");
        assert_eq!(record.raw_level, "3");
        assert_eq!(record.text, "VioGpu: DxgkInitialize failed with Status: 0xC0000001\n");
    }

    #[test]
    fn captured_debugprint_marker_keeps_its_original_filetime() {
        // early-kmd.etl.001: payload starts at 65640, message starts at 65648.
        let payload =
            b"\x65\0\0\0\x09\0\0\x80VioGpu: ---> VIOGPU FULL build on on Jan  1 1980 00:00:00\n\0";
        let mut event: EVENT_RECORD = unsafe { std::mem::zeroed() };
        event.EventHeader.ProviderId = PROVIDER;
        event.EventHeader.TimeStamp = 134393633912317080;
        event.EventHeader.ProcessId = 4;
        event.UserData = payload.as_ptr().cast_mut().cast();
        event.UserDataLength = payload.len() as u16;
        let capture = unsafe { decode_record(&event, 134393633875000000) }
            .unwrap()
            .unwrap();
        assert_eq!(
            capture.source_time.as_deref(),
            Some("2026-11-17T04:29:51.2317080Z")
        );
        assert_eq!(capture.raw_level.as_deref(), Some("2147483657"));
        assert_eq!(
            capture.text,
            "VioGpu: ---> VIOGPU FULL build on on Jan  1 1980 00:00:00\n"
        );
    }
}
