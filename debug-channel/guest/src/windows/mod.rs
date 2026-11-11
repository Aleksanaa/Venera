mod clock;
mod dbwin;
mod etw;
mod ipc;
mod process;
mod service;
mod sessions;
mod transport;

use crate::{collector::Capture, state::State};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use venera_debug_protocol::{Envelope, LogRecord, Message};
use windows_sys::Win32::{
    Foundation::*,
    System::{
        RemoteDesktop::ProcessIdToSessionId, SystemInformation::GetSystemTimeAsFileTime,
        Threading::*, Time::*,
    },
};

pub type Shared = Arc<Mutex<State>>;
pub static STOP: AtomicBool = AtomicBool::new(false);
pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
pub fn error() -> io::Error {
    io::Error::last_os_error()
}
pub struct Handle(pub HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
pub fn stopped() -> bool {
    STOP.load(Ordering::Acquire)
}
pub fn timestamp_after(ms: u64) -> String {
    unsafe {
        let mut ft: FILETIME = std::mem::zeroed();
        GetSystemTimeAsFileTime(&mut ft);
        let ticks = ((ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64)
            .saturating_add(ms.saturating_mul(10000));
        let ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut st: SYSTEMTIME = std::mem::zeroed();
        FileTimeToSystemTime(&ft, &mut st);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
        )
    }
}
pub fn accept(state: &Shared, capture: Capture) {
    let mut state = state.lock().unwrap();
    let cached = capture
        .pid
        .and_then(|pid| state.processes.observed(pid, capture.observed_filetime));
    let mut capture = capture;
    if let Some((identity, session)) = cached {
        capture.process = crate::collector::merge_identity(capture.process.take(), Some(identity));
        capture.session_id = session.or(capture.session_id);
    }
    let tracked = capture
        .process
        .as_ref()
        .is_some_and(|identity| state.processes.contains(identity));
    if crate::collector::needs_identity(&state.status.effective_filter, &capture, tracked) {
        if state.pending_captures.len() < 256 && capture.text.len() <= 4092 {
            state
                .pending_captures
                .push_back((std::time::Instant::now(), capture));
        } else {
            state.status.loss["process_capture_queue_dropped"] = serde_json::json!(
                state.status.loss["process_capture_queue_dropped"]
                    .as_u64()
                    .unwrap_or(0)
                    + 1
            );
        }
        return;
    }
    let (capture, lost) = state.assembler.ingest(capture);
    if lost != 0 {
        let stream_id = state.stream_id.clone();
        state.queue_control(&Envelope::new(Message::Gap {
            stream_id,
            reason: "native_capture_incomplete".into(),
            count: Some(lost),
            first_seq: None,
            last_seq: None,
        }));
    }
    let Some(capture) = capture else {
        return;
    };
    let identity = capture
        .process
        .clone()
        .or_else(|| capture.pid.map(process::identity));
    let class = crate::collector::classify_capture(&capture).or_else(|| {
        let executable = identity.as_ref()?.exe_path.as_ref()?;
        let directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let trusted = ["vgpusrv.exe", "viogpuap.exe"].iter().any(|name| {
            let expected = directory.join(name).to_string_lossy().into_owned();
            unsafe {
                windows_sys::Win32::Globalization::CompareStringOrdinal(
                    wide(executable).as_ptr(),
                    -1,
                    wide(&expected).as_ptr(),
                    -1,
                    1,
                ) == windows_sys::Win32::Globalization::CSTR_EQUAL
            }
        });
        trusted.then(|| crate::collector::Classification {
            component: "service".into(),
            level: "unknown".into(),
            raw_level: "unknown".into(),
            tid: None,
        })
    });
    let Some(class) = class else {
        return;
    };
    if class.component == "probe" {
        return;
    }
    state.expire();
    let observed = capture
        .pid
        .and_then(|pid| state.processes.observed(pid, capture.observed_filetime));
    let identity = crate::collector::merge_identity(
        identity,
        observed.as_ref().map(|(identity, _)| identity.clone()),
    );
    let session_id = observed
        .and_then(|(_, session)| session)
        .or(capture.session_id);
    let ancestors = identity
        .as_ref()
        .map(|p| state.processes.ancestors(p, state.filter_started_filetime))
        .unwrap_or_default();
    let mut record = LogRecord {
        stream_id: state.stream_id.clone(),
        seq: 0,
        observed_at: capture.observed_at,
        source_time: capture.source_time,
        source: capture.source,
        component: class.component,
        level: class.level,
        raw_level: capture.raw_level.unwrap_or(class.raw_level),
        session_id,
        process: identity,
        tid: capture.tid.or(class.tid),
        text: capture.text,
    };
    if !venera_debug_protocol::matches_filter(&state.status.effective_filter, &record, &ancestors) {
        return;
    }
    state.sequence += 1;
    record.seq = state.sequence;
    let bytes = venera_debug_protocol::encode(&Envelope::new(Message::Log { record })).unwrap();
    if bytes.len() > venera_debug_protocol::DEFAULT_MAX_LINE_BYTES {
        let stream_id = state.stream_id.clone();
        let seq = state.sequence;
        state.queue_control(&Envelope::new(Message::Gap {
            stream_id,
            reason: "record_size_exceeded".into(),
            count: Some(1),
            first_seq: Some(seq),
            last_seq: Some(seq),
        }));
        return;
    }
    state.broadcast(bytes.clone());
    let seq = state.sequence;
    state.enqueue_log(seq, bytes);
}
pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("service") | None => service::dispatch()?,
        Some("session-helper") => ipc::helper()?,
        Some("install-service") => service::install()?,
        Some("uninstall-service") => service::uninstall()?,
        _ => ipc::cli(&args)?,
    }
    Ok(())
}
