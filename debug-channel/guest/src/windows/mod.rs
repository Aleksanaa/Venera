mod clock;
mod capture;
mod dbwin;
mod etw;
mod ipc;
mod process;
mod service;
mod sessions;
mod system_info;
mod transport;

use crate::{collector::Capture, state::State};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use venera_debug_protocol::{Envelope, Message};
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
    let shared = state.clone();
    let mut state = state.lock().unwrap();
    let cached = capture
        .pid
        .and_then(|pid| state.processes.observed(pid, capture.observed_filetime));
    let mut capture = capture;
    if let Some((identity, session)) = cached {
        capture.process = crate::collector::merge_identity(capture.process.take(), Some(identity));
        capture.session_id = session.or(capture.session_id);
    }
    let (capture, lost) = state.assembler.ingest(capture);
    let untracked_loss = state.assembler.take_untracked_loss();
    drop(state);
    retain_native_losses(&shared, lost, untracked_loss);
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
    let mut state = shared.lock().unwrap();
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
    let mut capture = capture;
    capture.process = identity;
    capture.session_id = session_id;
    let startup = state
        .boot_id
        .parse::<u64>()
        .ok()
        .map(|boot| {
            let source = capture.structured.as_ref()
                .map_or(capture.observed_filetime, |event| event.source_time_filetime.0);
            source >= boot && source - boot < 120 * 10_000_000
        });
    let record = state.capture_record(capture, class);
    drop(state);
    if let Some(record) = record {
        retain_event_at(
            &shared,
            venera_debug_protocol::v1::HistoryEvent::Log { record },
            startup,
        );
    }
}

fn retain_native_losses(state: &Shared, lost: u64, untracked_loss: bool) {
    use venera_debug_protocol::v1::{DecimalU64, Gap, HistoryEvent};
    if lost == 0 && !untracked_loss {
        return;
    }
    let stream_id = state.lock().unwrap().stream_id.clone();
    let retain_gap = |reason: &str, count| {
        retain_event(
            state,
            HistoryEvent::Gap {
                gap: Gap {
                    stream_id: stream_id.clone(),
                    reason: reason.into(),
                    count,
                    first_seq: None,
                    last_seq: None,
                },
            },
        );
    };
    if lost != 0 {
        retain_gap("native_capture_incomplete", Some(DecimalU64(lost)));
    }
    if untracked_loss {
        retain_gap("native_capture_overload", None);
    }
}

fn expire_native_captures(state: &Shared) {
    let lost = {
        let mut state = state.lock().unwrap();
        state.assembler.expire(Instant::now())
    };
    retain_native_losses(state, lost, false);
}

fn finish_native_captures(state: &Shared) {
    let lost = state.lock().unwrap().assembler.finish();
    retain_native_losses(state, lost, false);
}

pub fn retain_event(state: &Shared, event: venera_debug_protocol::v1::HistoryEvent) {
    retain_event_at(state, event, None);
}

fn retain_event_at(
    state: &Shared,
    event: venera_debug_protocol::v1::HistoryEvent,
    startup: Option<bool>,
) {
    let (history, boot_id) = {
        let state = state.lock().unwrap();
        (state.history.clone(), state.boot_id.clone())
    };
    if let Some(history) = history {
        let entry = crate::journal::JournalInput {
            boot_id,
            recorded_at_ms: clock::filetime().saturating_sub(116444736000000000) / 10000,
            startup: startup.unwrap_or_else(|| unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() } < 120_000),
            event,
        };
        if let Err(error) = history.append(entry) {
            transport::health(
                state,
                "history",
                serde_json::json!({"error":error.to_string()}),
            );
        }
    }
}
pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("service") | None => service::dispatch()?,
        Some("session-helper") => ipc::helper()?,
        Some("install-service") => service::install()?,
        Some("uninstall-service") => service::uninstall()?,
        Some("trace") => etw::diagnostic(&args[1..])?,
        Some("prepare-history") => service::prepare_history()?,
        _ => ipc::cli(&args)?,
    }
    Ok(())
}

pub fn history_request(
    state: &Shared,
    request: venera_debug_protocol::v1::Incoming,
    local: bool,
    encoding: venera_debug_protocol::v1::HistoryEncoding,
) -> venera_debug_protocol::v1::RpcResult<serde_json::Value> {
    use venera_debug_protocol::v1::{Incoming, RpcError};
    let failure = |error: io::Error| RpcError::new(-32007, error.to_string());
    let (history, forwarding) = {
        let state = state.lock().unwrap();
        (state.history.clone(), state.status.forwarding_enabled)
    };
    let history = history.ok_or_else(|| RpcError::new(-32007, "History is starting"))?;
    if let Incoming::ForwardingSet { params, .. } = request {
        let status = history
            .forwarding(params.enabled, state.clone(), service::persist_forwarding)
            .map_err(failure)?;
        return crate::state::wire::status_v1(&status)
            .and_then(serde_json::to_value)
            .map_err(|error| RpcError::new(-32603, error.to_string()));
    }
    history.rpc_encoded(request, local || forwarding, encoding)
}
