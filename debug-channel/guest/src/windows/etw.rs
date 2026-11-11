use super::*;
use windows_sys::{core::GUID, Win32::System::Diagnostics::Etw::*};
const PROVIDER: GUID = GUID::from_u128(0x13976d09_a327_438c_950b_7f03192815c7);
const PROCESS_PROVIDER: GUID = GUID::from_u128(0x22fb2cd6_0e7b_422b_a0c7_2fad1fd0e716);
const NAME: &str = "Venera Debug Guest kernel messages";

fn same_guid(a: GUID, b: GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
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
unsafe fn process_event(record: &EVENT_RECORD, state: &Shared) -> bool {
    if ![1, 2].contains(&record.EventHeader.EventDescriptor.Id) {
        return true;
    }
    let Some(pid) = number(record, "ProcessID").and_then(|n| u32::try_from(n).ok()) else {
        return false;
    };
    let Some(start) = number(record, "CreateTime") else {
        return false;
    };
    let mut state = state.lock().unwrap();
    match record.EventHeader.EventDescriptor.Id {
        1 => {
            let Some(parent) =
                number(record, "ParentProcessID").and_then(|n| u32::try_from(n).ok())
            else {
                return false;
            };
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
                String::from_utf16(&utf16).ok().map(process::dos_path)
            });
            let mut identity = venera_debug_protocol::ProcessIdentity {
                pid,
                start_time_filetime: Some(start.to_string()),
                exe_path: path,
            };
            let live = process::identity(pid);
            if live.start_time_filetime == identity.start_time_filetime && live.exe_path.is_some() {
                identity.exe_path = live.exe_path;
            }
            if !state.processes.started(identity, parent, session) {
                state.status.loss["process_tracking_overflow"] = serde_json::json!(true);
            }
        }
        2 => {
            if let Some(end) = number(record, "ExitTime") {
                state.processes.ended(pid, start, end);
            } else {
                return false;
            }
        }
        _ => {}
    }
    true
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
    if id.data1 != PROVIDER.data1
        || id.data2 != PROVIDER.data2
        || id.data3 != PROVIDER.data3
        || id.data4 != PROVIDER.data4
        || record.UserDataLength <= 8
    {
        return;
    }
    let bytes =
        std::slice::from_raw_parts(record.UserData as *const u8, record.UserDataLength as usize);
    let raw = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
    let body = &bytes[8..];
    let length = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    let pid = (record.EventHeader.ProcessId != u32::MAX && record.EventHeader.ProcessId != 0)
        .then_some(record.EventHeader.ProcessId);
    let tid = (record.EventHeader.ThreadId != u32::MAX && record.EventHeader.ThreadId != 0)
        .then_some(record.EventHeader.ThreadId);
    let mut session = 0;
    let session_id =
        pid.and_then(|pid| (ProcessIdToSessionId(pid, &mut session) != 0).then_some(session));
    accept(
        state,
        Capture {
            pid,
            tid,
            session_id,
            observed_at: timestamp_after(0),
            observed_filetime: clock::filetime(),
            source_time: Some(clock::iso_filetime(record.EventHeader.TimeStamp as u64)),
            source: "etw".into(),
            text: dbwin::decode_ansi(&body[..length]),
            raw_level: Some(raw.to_string()),
            process: pid.map(process::identity),
            component: None,
        },
    );
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
        (*props).EnableFlags = EVENT_TRACE_FLAG_DBGPRINT;
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
        let process_code = EnableTraceEx2(
            session,
            &PROCESS_PROVIDER,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER,
            TRACE_LEVEL_INFORMATION as u8,
            0x10,
            0,
            0,
            std::ptr::null(),
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
        while !stopped() && !reader.is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if maintenance.elapsed() < std::time::Duration::from_secs(5) {
                continue;
            }
            maintenance = std::time::Instant::now();
            ControlTraceW(session, name.as_ptr(), props, EVENT_TRACE_CONTROL_QUERY);
            let mut s = state.lock().unwrap();
            s.status.loss["etw_events_lost"] = serde_json::json!((*props).EventsLost);
            let boundary = clock::filetime().saturating_sub(60 * 10_000_000);
            s.processes.retire_before(boundary);
        }
        ControlTraceW(session, name.as_ptr(), props, EVENT_TRACE_CONTROL_QUERY);
        {
            let mut s = state.lock().unwrap();
            s.status.loss["etw_events_lost"] = serde_json::json!((*props).EventsLost);
        }
        ControlTraceW(session, name.as_ptr(), props, EVENT_TRACE_CONTROL_STOP);
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
