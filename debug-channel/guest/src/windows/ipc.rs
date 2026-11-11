use super::*;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle},
    sync::mpsc,
};
use venera_debug_protocol::{Envelope, Filter, Message};
use windows_sys::Win32::{
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::Pipes::*,
    System::IO::*,
};
const PIPE: &str = "\\\\.\\pipe\\VeneraDebugGuest.v1";
const LIMIT: usize = venera_debug_protocol::DEFAULT_MAX_LINE_BYTES;

#[derive(serde::Serialize, serde::Deserialize)]
struct HelperFrame {
    capture: Option<Capture>,
    health: serde_json::Value,
    dropped: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_info: Option<venera_debug_protocol::SystemInfoResult>,
}

fn line<R: Read>(reader: &mut BufReader<R>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let count = reader
        .take((LIMIT + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if count == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    if bytes.len() > LIMIT || bytes.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Local message exceeds record budget",
        ));
    }
    Ok(bytes)
}
fn connect() -> io::Result<File> {
    connect_file(0)
}
fn connect_file(flags: u32) -> io::Result<File> {
    unsafe {
        if WaitNamedPipeW(wide(PIPE).as_ptr(), 5000) == 0 {
            return Err(error());
        }
        let handle = CreateFileW(
            wide(PIPE).as_ptr(),
            FILE_READ_DATA | FILE_WRITE_DATA | SYNCHRONIZE | READ_CONTROL,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION | flags,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(error());
        }
        let file = File::from_raw_handle(handle);
        let mut pid = 0;
        if GetNamedPipeServerProcessId(handle, &mut pid) == 0 {
            return Err(error());
        }
        if service::server_process()? != pid {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(file)
    }
}

#[derive(Clone)]
struct HelperPipe {
    handle: Arc<Handle>,
    stop: Arc<AtomicBool>,
    #[cfg(test)]
    io_pending: Option<Arc<AtomicBool>>,
}

fn connect_helper() -> io::Result<HelperPipe> {
    let file = connect_file(FILE_FLAG_OVERLAPPED)?;
    Ok(HelperPipe {
        handle: Arc::new(Handle(file.into_raw_handle())),
        stop: Arc::new(AtomicBool::new(false)),
        #[cfg(test)]
        io_pending: None,
    })
}

impl HelperPipe {
    unsafe fn complete(&self, overlapped: &OVERLAPPED) -> io::Result<usize> {
        #[cfg(test)]
        if let Some(pending) = &self.io_pending {
            pending.store(true, Ordering::Release);
        }
        let mut transferred = 0;
        loop {
            let wait = WaitForSingleObject(overlapped.hEvent, 50);
            if wait == WAIT_OBJECT_0 {
                return if GetOverlappedResult(self.handle.0, overlapped, &mut transferred, 0) != 0 {
                    Ok(transferred as usize)
                } else {
                    Err(error())
                };
            }
            if wait != WAIT_TIMEOUT || self.stop.load(Ordering::Acquire) || stopped() {
                let issue = if wait != WAIT_TIMEOUT {
                    error()
                } else {
                    io::ErrorKind::ConnectionAborted.into()
                };
                CancelIoEx(self.handle.0, overlapped);
                // Keep the operation and caller's buffer alive through cancellation completion.
                GetOverlappedResult(self.handle.0, overlapped, &mut transferred, 1);
                return Err(issue);
            }
        }
    }
}

impl Read for HelperPipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        unsafe {
            let event = Handle(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()));
            if event.0.is_null() {
                return Err(error());
            }
            let mut overlapped: OVERLAPPED = std::mem::zeroed();
            overlapped.hEvent = event.0;
            let mut transferred = 0;
            let submitted = ReadFile(
                self.handle.0,
                buffer.as_mut_ptr().cast(),
                buffer.len().min(u32::MAX as usize) as u32,
                &mut transferred,
                &mut overlapped,
            );
            if submitted == 0 {
                let issue = error();
                if issue.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(issue);
                }
                return self.complete(&overlapped);
            }
            Ok(transferred as usize)
        }
    }
}

impl Write for HelperPipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        unsafe {
            let event = Handle(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()));
            if event.0.is_null() {
                return Err(error());
            }
            let mut overlapped: OVERLAPPED = std::mem::zeroed();
            overlapped.hEvent = event.0;
            let mut transferred = 0;
            if WriteFile(
                self.handle.0,
                bytes.as_ptr().cast(),
                bytes.len().min(u32::MAX as usize) as u32,
                &mut transferred,
                &mut overlapped,
            ) == 0
            {
                let issue = error();
                if issue.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(issue);
                }
                return self.complete(&overlapped);
            }
            Ok(transferred as usize)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
unsafe fn admin(pipe: HANDLE) -> bool {
    if ImpersonateNamedPipeClient(pipe) == 0 {
        return false;
    }
    let mut sid = [0u64; 16];
    let mut length = std::mem::size_of_val(&sid) as u32;
    let mut yes = 0;
    if CreateWellKnownSid(
        WinBuiltinAdministratorsSid,
        std::ptr::null_mut(),
        sid.as_mut_ptr().cast(),
        &mut length,
    ) != 0
    {
        CheckTokenMembership(std::ptr::null_mut(), sid.as_mut_ptr().cast(), &mut yes);
    }
    RevertToSelf();
    yes != 0
}
fn reply(file: &mut File, value: &serde_json::Value) -> io::Result<()> {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")
}
fn client(file: File, state: Shared, finite: &mut bool, local_stop: &AtomicBool) -> io::Result<()> {
    if client_stopped(local_stop) {
        return Ok(());
    }
    let pipe = file.as_raw_handle();
    let mut reader = BufReader::new(file.try_clone()?);
    let mut file = file;
    let first = line(&mut reader)?;
    let value: serde_json::Value = serde_json::from_slice(&first)?;
    *finite = !matches!(
        value["type"].as_str(),
        Some("local.capture" | "local.subscribe")
    );
    match value["type"].as_str() {
        Some("local.capture") => {
            let mut pid = 0;
            let mut session = 0;
            if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0
                || unsafe { ProcessIdToSessionId(pid, &mut session) } == 0
            {
                return Err(error());
            }
            let identity = process::identity(pid);
            if !state
                .lock()
                .unwrap()
                .helpers
                .get(&session)
                .is_some_and(|expected| {
                    expected.pid == pid
                        && expected.start_time_filetime.is_some()
                        && expected.start_time_filetime == identity.start_time_filetime
                })
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            reply(&mut file, &serde_json::json!({"ok":true}))?;
            while !client_stopped(local_stop) {
                let bytes = line(&mut reader)?;
                let mut frame: HelperFrame = serde_json::from_slice(&bytes)?;
                let live_helper = process::identity(pid);
                if live_helper.start_time_filetime.is_none()
                    || live_helper.start_time_filetime != identity.start_time_filetime
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                if !state
                    .lock()
                    .unwrap()
                    .helpers
                    .get(&session)
                    .is_some_and(|expected| {
                        expected.pid == pid
                            && expected.start_time_filetime == identity.start_time_filetime
                    })
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                if let Some(result) = frame.system_info.take() {
                    system_info::helper_result(session, result);
                }
                {
                    let mut state = state.lock().unwrap();
                    let key = session.to_string();
                    let mut changed = state.status.health["session_capture"][&key] != frame.health;
                    state.status.health["session_capture"][&key] = frame.health;
                    changed |= state.observe_helper_drops(session, pid, frame.dropped);
                    if changed {
                        state.notify();
                    }
                }
                if let Some(request_id) = system_info::helper_command(session) {
                    reply(
                        &mut file,
                        &serde_json::json!({"type":"local.system.info.get","request_id":request_id}),
                    )?;
                }
                let Some(mut capture) = frame.capture else {
                    continue;
                };
                if let Some(source_pid) = capture.pid {
                    let mut actual = 0;
                    let cached = state
                        .lock()
                        .unwrap()
                        .processes
                        .observed(source_pid, capture.observed_filetime);
                    let verified = cached
                        .as_ref()
                        .is_some_and(|(_, actual)| *actual == Some(session))
                        || (unsafe { ProcessIdToSessionId(source_pid, &mut actual) } != 0
                            && actual == session);
                    if !verified {
                        continue;
                    }
                    let captured = capture
                        .process
                        .take()
                        .filter(|identity| identity.pid == source_pid);
                    capture.process = crate::collector::merge_identity(
                        captured,
                        cached
                            .map(|(identity, _)| identity)
                            .or_else(|| Some(process::identity(source_pid))),
                    );
                }
                capture.session_id = Some(session);
                capture.source = "dbwin".into();
                capture.tid = None;
                capture.raw_level = None;
                capture.component = None;
                accept(&state, capture);
            }
            Ok(())
        }
        Some("local.subscribe") => {
            let (tx, rx) = mpsc::sync_channel(crate::state::LOG_RECORDS);
            let bytes = {
                let mut state = state.lock().unwrap();
                state.subscribers.push(tx);
                let current = state.current();
                venera_debug_protocol::encode(&Envelope::new(Message::Status { status: current }))?
            };
            file.write_all(&bytes)?;
            while !client_stopped(local_stop) {
                match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                    Ok(bytes) => file.write_all(&bytes)?,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => return Ok(()),
                }
            }
            Ok(())
        }
        Some("local.forwarding") => {
            if !unsafe { admin(pipe) } {
                return reply(
                    &mut file,
                    &serde_json::json!({"ok":false,"error":{"code":"permission_denied","message":"Administrator approval is required"}}),
                );
            }
            let enabled = value["enabled"]
                .as_bool()
                .ok_or(io::ErrorKind::InvalidInput)?;
            let mut state = state.lock().unwrap();
            service::persist_forwarding(enabled)?;
            state.set_forwarding(enabled);
            let response = serde_json::json!({"ok":true,"status":state.current()});
            drop(state);
            reply(&mut file, &response)
        }
        _ => {
            let envelope = venera_debug_protocol::decode(&first)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
            if !matches!(envelope.body, Message::StatusGet { .. }) && !unsafe { admin(pipe) } {
                let (request_id, operation) = match envelope.body {
                    Message::FilterSet { request_id, .. } => (request_id, "filter.set"),
                    Message::FilterReset { request_id } => (request_id, "filter.reset"),
                    _ => return Err(io::ErrorKind::InvalidInput.into()),
                };
                let mut response = state.lock().unwrap().result(request_id, operation);
                if let Message::RequestResult { result } = &mut response.body {
                    result.ok = false;
                    result.error = Some(venera_debug_protocol::RequestError {
                        code: "permission_denied".into(),
                        message: "Administrator approval is required".into(),
                    });
                }
                return file.write_all(&venera_debug_protocol::encode(&response)?);
            }
            let expires = match &envelope.body {
                Message::FilterSet {
                    duration_ms: Some(ms),
                    ..
                } => Some(timestamp_after(*ms)),
                _ => None,
            };
            let result = state
                .lock()
                .unwrap()
                .control(envelope.body, expires)
                .ok_or(io::ErrorKind::InvalidInput)?;
            file.write_all(&venera_debug_protocol::encode(&result)?)
        }
    }
}

pub fn serve(state: Shared) {
    let mut first = true;
    let mut namespace_guard: Option<Handle> = None;
    let mut workers = PipeWorkers::new();
    while !stopped() {
        workers.reap_finished();
        let result = unsafe {
            let mut descriptor = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12019b;;;AU)").as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return;
            }
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let flags = PIPE_ACCESS_DUPLEX
                | if first {
                    FILE_FLAG_FIRST_PIPE_INSTANCE
                } else {
                    0
                };
            let pipe = Handle(CreateNamedPipeW(
                wide(PIPE).as_ptr(),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                32,
                65536,
                65536,
                5000,
                &sa,
            ));
            LocalFree(descriptor);
            if pipe.0 == INVALID_HANDLE_VALUE {
                Err(error())
            } else {
                first = false;
                loop {
                    if ConnectNamedPipe(pipe.0, std::ptr::null_mut()) != 0 {
                        break;
                    }
                    let code = GetLastError();
                    if code == ERROR_PIPE_CONNECTED {
                        break;
                    }
                    if code == ERROR_NO_DATA {
                        DisconnectNamedPipe(pipe.0);
                    } else if code != ERROR_PIPE_LISTENING {
                        transport::health(
                            &state,
                            "ipc",
                            serde_json::json!({"error":io::Error::from_raw_os_error(code as i32).to_string()}),
                        );
                        return;
                    }
                    if stopped() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                let mode = PIPE_READMODE_BYTE | PIPE_WAIT;
                if SetNamedPipeHandleState(pipe.0, &mode, std::ptr::null(), std::ptr::null()) == 0 {
                    Err(error())
                } else {
                    if namespace_guard.is_none() {
                        let mut duplicate = std::ptr::null_mut();
                        if DuplicateHandle(
                            GetCurrentProcess(),
                            pipe.0,
                            GetCurrentProcess(),
                            &mut duplicate,
                            0,
                            0,
                            DUPLICATE_SAME_ACCESS,
                        ) == 0
                        {
                            return;
                        }
                        namespace_guard = Some(Handle(duplicate));
                    }
                    let raw = pipe.0 as usize;
                    std::mem::forget(pipe);
                    let shared = state.clone();
                    let local_stop = workers.local_stop.clone();
                    workers.threads.push(std::thread::spawn(move || {
                        let file = File::from_raw_handle(raw as HANDLE);
                        let disconnect = file.try_clone();
                        let mut finite = false;
                        let result = client(file, shared, &mut finite, &local_stop);
                        if let Ok(file) = disconnect {
                            let _ = finish_response(
                                &file,
                                result.is_ok() && finite && !client_stopped(&local_stop),
                            );
                        }
                    }));
                    Ok(())
                }
            }
        };
        if let Err(e) = result {
            let busy = e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32);
            {
                let mut state = state.lock().unwrap();
                state.status.health["ipc"] = serde_json::json!({"error":e.to_string()});
                state.notify();
            }
            if !busy {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

fn client_stopped(local_stop: &AtomicBool) -> bool {
    stopped() || local_stop.load(Ordering::Acquire)
}

struct PipeWorkers {
    threads: Vec<std::thread::JoinHandle<()>>,
    local_stop: Arc<AtomicBool>,
}
impl PipeWorkers {
    fn new() -> Self {
        Self {
            threads: Vec::new(),
            local_stop: Arc::new(AtomicBool::new(false)),
        }
    }
    fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.threads.len() {
            if self.threads[index].is_finished() {
                let _ = self.threads.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
    }
}
impl Drop for PipeWorkers {
    fn drop(&mut self) {
        self.local_stop.store(true, Ordering::Release);
        cancel_workers(std::mem::take(&mut self.threads));
    }
}

fn finish_response(file: &File, flush: bool) -> io::Result<()> {
    unsafe {
        let result = if flush && FlushFileBuffers(file.as_raw_handle()) == 0 {
            Err(error())
        } else {
            Ok(())
        };
        DisconnectNamedPipe(file.as_raw_handle());
        result
    }
}

fn cancel_workers(workers: Vec<std::thread::JoinHandle<()>>) {
    for worker in &workers {
        while !worker.is_finished() {
            unsafe {
                windows_sys::Win32::System::IO::CancelSynchronousIo(worker.as_raw_handle());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
}

fn helper_commands(
    reader: BufReader<HelperPipe>,
    session: u32,
    stop: Arc<AtomicBool>,
    reports: mpsc::SyncSender<venera_debug_protocol::SystemInfoResult>,
) {
    helper_commands_with(reader, session, stop, reports, system_info::collect_local);
}

fn helper_commands_with(
    mut reader: BufReader<HelperPipe>,
    session: u32,
    stop: Arc<AtomicBool>,
    reports: mpsc::SyncSender<venera_debug_protocol::SystemInfoResult>,
    collect: impl Fn(String, u32, &AtomicBool) -> venera_debug_protocol::SystemInfoResult,
) {
    while !stop.load(Ordering::Acquire) {
        let bytes = match line(&mut reader) {
            Ok(bytes) => bytes,
            Err(_) => break,
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            break;
        };
        if value["type"] != "local.system.info.get" {
            continue;
        }
        let Some(request_id) = value["request_id"].as_str() else {
            continue;
        };
        let result = collect(request_id.to_owned(), session, &stop);
        if stop.load(Ordering::Acquire) {
            break;
        }
        if reports.try_send(result).is_err() {
            break;
        }
    }
    stop.store(true, Ordering::Release);
}

pub fn helper() -> io::Result<()> {
    let mut session = 0;
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 {
        return Err(error());
    }
    let (tx, rx) = mpsc::sync_channel(2048);
    let status = Arc::new(Mutex::new(serde_json::json!("starting")));
    let dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let collector_status = status.clone();
    let collector_dropped = dropped.clone();
    std::thread::spawn(move || loop {
        let result = dbwin::collect(
            false,
            |capture| {
                if tx.try_send(capture).is_err() {
                    collector_dropped.fetch_add(1, Ordering::Relaxed);
                }
            },
            || true,
            || {
                *collector_status.lock().unwrap() = serde_json::json!("collecting");
            },
        );
        if let Err(e) = result {
            *collector_status.lock().unwrap() = serde_json::json!({"error":e.to_string()});
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
    loop {
        if let Ok(mut file) = connect_helper() {
            let connection_stop = file.stop.clone();
            if file.write_all(b"{\"type\":\"local.capture\"}\n").is_ok() {
                let mut reader = BufReader::new(file.clone());
                if line(&mut reader).is_ok() {
                    let reader_stop = connection_stop.clone();
                    let (report_tx, report_rx) = mpsc::sync_channel(1);
                    let commands = std::thread::Builder::new()
                        .name("system-info-commands".into())
                        .spawn(move || helper_commands(reader, session, reader_stop, report_tx))?;
                    loop {
                        let capture = match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                            Ok(capture) => Some(capture),
                            Err(mpsc::RecvTimeoutError::Timeout) => None,
                            Err(_) => break,
                        };
                        if commands.is_finished() {
                            break;
                        }
                        let frame = HelperFrame {
                            capture,
                            health: status.lock().unwrap().clone(),
                            dropped: dropped.load(Ordering::Relaxed),
                            system_info: report_rx.try_recv().ok(),
                        };
                        if serde_json::to_writer(&mut file, &frame).is_err()
                            || file.write_all(b"\n").is_err()
                        {
                            break;
                        }
                    }
                    connection_stop.store(true, Ordering::Release);
                    let _ = commands.join();
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

pub fn cli(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let request = match args {
        [command, flag] if command == "status" && flag == "--json" => serde_json::to_value(Envelope::new(Message::StatusGet { request_id:"local-cli".into() }))?,
        [command, state] if command == "forwarding" && ["on", "off"].contains(&state.as_str()) => serde_json::json!({"type":"local.forwarding","enabled":state == "on"}),
        [command, flag, path] if command == "filter-set" && flag == "--json" => {
            let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
            let filter: Filter = serde_json::from_value(value.get("filter").cloned().unwrap_or(value.clone()))?;
            let duration_ms = value.get("duration_ms").and_then(serde_json::Value::as_u64);
            serde_json::to_value(Envelope::new(Message::FilterSet { request_id:"local-cli".into(), filter, duration_ms }))?
        },
        [command] if command == "filter-reset" => serde_json::to_value(Envelope::new(Message::FilterReset { request_id:"local-cli".into() }))?,
        [command, flag] if command == "logs" && flag == "--follow" => serde_json::json!({"type":"local.subscribe"}),
        _ => return Err("Commands: status --json; forwarding on|off; filter-set --json FILE; filter-reset; logs --follow; install-service; uninstall-service; service; session-helper".into()),
    };
    let mut file = connect()
        .map_err(|error| io::Error::new(error.kind(), format!("Connect to service: {error}")))?;
    reply(&mut file, &request)
        .map_err(|error| io::Error::new(error.kind(), format!("Write service request: {error}")))?;
    let mut reader = BufReader::new(file);
    let following = request["type"] == "local.subscribe";
    loop {
        let bytes = line(&mut reader).map_err(|error| {
            io::Error::new(error.kind(), format!("Read service response: {error}"))
        })?;
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if value["ok"] == false && request["type"] == "status.get" {
            serde_json::to_writer(io::stdout().lock(), &value)?;
            println!();
            return Err("Service request failed".into());
        }
        if request["type"] == "status.get" {
            value = value["status"].clone();
        }
        serde_json::to_writer(io::stdout().lock(), &value)?;
        println!();
        if !following {
            if value["ok"] == false {
                return Err("Service request failed".into());
            }
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static NEXT_PIPE: AtomicU32 = AtomicU32::new(0);
    const RESPONSE: &[u8] = b"{\"ok\":true,\"status\":{\"forwarding_enabled\":true}}\n";

    #[test]
    fn helper_duplex_delivers_health_before_report_command() {
        let (mut service, mut helper) = helper_pair();
        let pending_read = Arc::new(AtomicBool::new(false));
        let mut helper_reader = BufReader::new(helper.clone());
        let stop = helper.stop.clone();
        let (observed, received) = mpsc::channel();
        let server = std::thread::spawn(move || -> io::Result<()> {
            let mut reader = BufReader::new(service.try_clone()?);
            assert_eq!(line(&mut reader)?, b"{\"type\":\"local.capture\"}\n");
            service.write_all(b"{\"ok\":true}\n")?;
            assert_eq!(
                line(&mut reader)?,
                b"{\"capture\":null,\"health\":\"collecting\",\"dropped\":0}\n"
            );
            observed.send("health").unwrap();
            service
                .write_all(b"{\"type\":\"local.system.info.get\",\"request_id\":\"duplex-2\"}\n")?;
            let result: serde_json::Value = serde_json::from_slice(&line(&mut reader)?)?;
            assert_eq!(
                result,
                serde_json::json!({"capture":null,"health":"collecting","dropped":0,"system_info":{"request_id":"duplex-2","ok":true,"collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Venera","bad":false,"warning":""}]}]}})
            );
            observed.send("result").unwrap();
            Ok(())
        });
        helper.write_all(b"{\"type\":\"local.capture\"}\n").unwrap();
        assert_eq!(line(&mut helper_reader).unwrap(), b"{\"ok\":true}\n");
        helper_reader.get_mut().io_pending = Some(pending_read.clone());
        let (reports, report_rx) = mpsc::sync_channel(1);
        let reader_stop = stop.clone();
        let commands = std::thread::spawn(move || {
            helper_commands_with(
                helper_reader,
                2,
                reader_stop,
                reports,
                |request_id, session, _| {
                    assert_eq!(request_id, "duplex-2");
                    assert_eq!(session, 2);
                    serde_json::from_value(serde_json::json!({"request_id":"duplex-2","ok":true,"collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Venera","bad":false,"warning":""}]}]})).unwrap()
                },
            );
        });
        let started = std::time::Instant::now();
        let pending = loop {
            if pending_read.load(Ordering::Acquire) {
                break true;
            }
            if started.elapsed() > std::time::Duration::from_secs(2) {
                break false;
            }
            std::thread::yield_now();
        };
        let connection = helper.clone();
        let (written, completion) = mpsc::channel();
        let writer = std::thread::spawn(move || -> io::Result<()> {
            helper.write_all(b"{\"capture\":null,\"health\":\"collecting\",\"dropped\":0}\n")?;
            let result = report_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|_| io::ErrorKind::TimedOut)?;
            let frame = HelperFrame {
                capture: None,
                health: serde_json::json!("collecting"),
                dropped: 0,
                system_info: Some(result),
            };
            serde_json::to_writer(&mut helper, &frame)?;
            helper.write_all(b"\n")?;
            written.send(()).unwrap();
            Ok(())
        });
        let health = received.recv_timeout(std::time::Duration::from_secs(2));
        let completed = completion.recv_timeout(std::time::Duration::from_secs(3));
        let result = received.recv_timeout(std::time::Duration::from_secs(3));
        stop.store(true, Ordering::Release);
        for _ in 0..100 {
            if server.is_finished() && writer.is_finished() && commands.is_finished() {
                break;
            }
            unsafe {
                CancelIoEx(connection.handle.0, std::ptr::null());
                CancelSynchronousIo(server.as_raw_handle());
                CancelSynchronousIo(writer.as_raw_handle());
                CancelSynchronousIo(commands.as_raw_handle());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            pending,
            "helper command read must be pending before health is sent"
        );
        assert_eq!(
            health,
            Ok("health"),
            "health must reach the service while command read is pending"
        );
        assert_eq!(completed, Ok(()), "helper report must reach the service");
        assert_eq!(result, Ok("result"));
        assert!(
            server.is_finished() && writer.is_finished() && commands.is_finished(),
            "pipe workers must finish after cancellation"
        );
        writer.join().unwrap().unwrap();
        server.join().unwrap().unwrap();
        commands.join().unwrap();
        drop(connection);
    }

    #[test]
    fn capture_frames_keep_health_and_report_results() {
        let old: HelperFrame = serde_json::from_value(
            serde_json::json!({"capture":null,"health":"collecting","dropped":4}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(old).unwrap(),
            serde_json::json!({"capture":null,"health":"collecting","dropped":4})
        );
        let report = serde_json::json!({"capture":null,"health":"collecting","dropped":4,"system_info":{"request_id":"report-2","ok":true,"collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Unavailable","bad":true,"warning":"请检查图形驱动。\n重新启动 Windows 后重试。"}]}]}});
        let decoded: HelperFrame = serde_json::from_value(report.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), report);
    }

    #[test]
    fn helper_command_reader_finishes_when_service_disconnects() {
        let (mut service, helper) = helper_pair();
        let (reports, receiver) = mpsc::sync_channel(1);
        let (finished, completion) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            helper_commands(
                BufReader::new(helper),
                2,
                Arc::new(AtomicBool::new(false)),
                reports,
            );
            finished.send("finished").unwrap();
        });
        service.write_all(b"{\"type\":\"local.health\"}\n").unwrap();
        drop(service);
        assert_eq!(
            completion
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            "finished"
        );
        assert_eq!(
            receiver.recv_timeout(std::time::Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        worker.join().unwrap();
    }

    #[test]
    fn helper_command_reader_joins_when_connection_owner_stops() {
        let (_service, mut helper) = helper_pair();
        let pending = Arc::new(AtomicBool::new(false));
        helper.io_pending = Some(pending.clone());
        let (reports, receiver) = mpsc::sync_channel(1);
        let stop = helper.stop.clone();
        let reader_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            helper_commands(BufReader::new(helper), 2, reader_stop, reports)
        });
        wait_pending(&pending);
        stop.store(true, Ordering::Release);
        assert_eq!(
            receiver.recv_timeout(std::time::Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        worker.join().unwrap();
    }

    fn wait_pending(pending: &AtomicBool) {
        let started = std::time::Instant::now();
        while !pending.load(Ordering::Acquire) {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "pipe operation must be pending"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn helper_pending_write_finishes_when_connection_owner_stops() {
        let (_service, mut helper) = helper_pair();
        let pending = Arc::new(AtomicBool::new(false));
        helper.io_pending = Some(pending.clone());
        let stop = helper.stop.clone();
        let (finished, completion) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = helper.write_all(&vec![b'x'; 1024 * 1024]);
            finished.send(result.map_err(|issue| issue.kind())).unwrap();
        });
        wait_pending(&pending);
        stop.store(true, Ordering::Release);
        assert_eq!(
            completion
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            Err(io::ErrorKind::ConnectionAborted)
        );
        worker.join().unwrap();
    }

    fn pipe_pair() -> (File, File) {
        pipe_pair_flags(0)
    }

    fn helper_pair() -> (File, HelperPipe) {
        let (server, client) = pipe_pair_flags(FILE_FLAG_OVERLAPPED);
        (
            server,
            HelperPipe {
                handle: Arc::new(Handle(client.into_raw_handle())),
                stop: Arc::new(AtomicBool::new(false)),
                io_pending: None,
            },
        )
    }

    fn pipe_pair_flags(flags: u32) -> (File, File) {
        unsafe {
            let name = wide(&format!(
                "\\\\.\\pipe\\VeneraDebugGuest-test-{}-{}",
                GetCurrentProcessId(),
                NEXT_PIPE.fetch_add(1, Ordering::Relaxed)
            ));
            let server = CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65536,
                65536,
                0,
                std::ptr::null(),
            );
            assert_ne!(server, INVALID_HANDLE_VALUE, "{}", error());
            let server = File::from_raw_handle(server);
            let client = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                flags,
                std::ptr::null_mut(),
            );
            assert_ne!(client, INVALID_HANDLE_VALUE, "{}", error());
            let client = File::from_raw_handle(client);
            assert!(
                ConnectNamedPipe(server.as_raw_handle(), std::ptr::null_mut()) != 0
                    || GetLastError() == ERROR_PIPE_CONNECTED
            );
            (server, client)
        }
    }

    #[test]
    fn finite_response_survives_delayed_pipe_reader() {
        for delay in [100, 500] {
            let (mut server, client) = pipe_pair();
            let (written, received) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                server.write_all(RESPONSE).unwrap();
                written.send(()).unwrap();
                finish_response(&server, true)
            });
            received
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(delay));
            let mut reader = BufReader::new(client);
            assert_eq!(line(&mut reader).unwrap(), RESPONSE);
            worker.join().unwrap().unwrap();
        }
    }

    #[test]
    fn service_stop_cancels_response_flush_for_nonreading_client() {
        let (mut server, _client) = pipe_pair();
        let (written, received) = mpsc::channel();
        let (finished, outcome) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            server.write_all(RESPONSE).unwrap();
            written.send(()).unwrap();
            let result = finish_response(&server, true);
            finished.send(result.unwrap_err().raw_os_error()).unwrap();
        });
        received
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        cancel_workers(vec![worker]);
        assert_eq!(
            outcome
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            Some(ERROR_OPERATION_ABORTED as i32)
        );
    }

    #[test]
    fn local_log_subscription_projects_ordinary_program_capture() {
        let (server, mut pipe_client) = pipe_pair();
        let state = Arc::new(Mutex::new(State::new("pipe-test".into(), true)));
        state.lock().unwrap().status.effective_filter.components = vec!["venus".into()];
        let mut owner = PipeWorkers::new();
        let local_stop = owner.local_stop.clone();
        let shared = state.clone();
        owner.threads.push(std::thread::spawn(move || {
            let mut finite = false;
            client(server, shared, &mut finite, &local_stop).unwrap();
        }));
        pipe_client
            .write_all(b"{\"type\":\"local.subscribe\"}\n")
            .unwrap();
        let (observed, received) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(pipe_client);
            for _ in 0..2 {
                let value: serde_json::Value =
                    serde_json::from_slice(&line(&mut reader).unwrap()).unwrap();
                observed.send(value).unwrap();
            }
        });
        assert_eq!(
            received
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()["type"],
            "status"
        );
        accept(
            &state,
            Capture {
                pid: Some(42),
                tid: None,
                session_id: Some(2),
                observed_at: "2026-11-12T00:00:00Z".into(),
                observed_filetime: 100,
                source_time: None,
                source: "dbwin".into(),
                text: "ordinary program: hello 中文".into(),
                raw_level: None,
                process: Some(venera_debug_protocol::ProcessIdentity {
                    pid: 42,
                    start_time_filetime: Some("100".into()),
                    exe_path: Some("C:\\Tools\\ordinary.exe".into()),
                }),
                component: None,
            },
        );
        assert_eq!(
            received
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            serde_json::json!({"major":1,"type":"log","stream_id":"pipe-test","seq":1,
                "observed_at":"2026-11-12T00:00:00Z","source_time":null,"source":"dbwin",
                "component":"","level":"unknown","raw_level":"unknown","session_id":2,
                "process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\Tools\\ordinary.exe"},
                "tid":null,"text":"ordinary program: hello 中文"})
        );
        reader.join().unwrap();
        drop(owner);
    }

    fn drop_listener_owner(subscribe: bool) {
        assert!(!stopped());
        let (server, mut pipe_client) = pipe_pair();
        let mut owner = PipeWorkers::new();
        let local_stop = owner.local_stop.clone();
        let (entered, entry) = mpsc::channel();
        let (exited, exit) = mpsc::channel();
        owner.threads.push(std::thread::spawn(move || {
            let state = Arc::new(Mutex::new(State::new("pipe-test".into(), true)));
            let mut finite = false;
            entered.send(()).unwrap();
            let result = client(server, state, &mut finite, &local_stop);
            exited
                .send((finite, result.map_err(|error| error.raw_os_error())))
                .unwrap();
        }));
        entry
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        if subscribe {
            pipe_client
                .write_all(b"{\"type\":\"local.subscribe\"}\n")
                .unwrap();
            let (observed, response) = mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(pipe_client);
                let bytes = line(&mut reader).unwrap();
                observed.send((reader, bytes)).unwrap();
            });
            let (reader, bytes) = response
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["type"],
                "status"
            );
            pipe_client = reader.into_inner();
        } else {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let (dropped, completion) = mpsc::channel();
        let listener = std::thread::spawn(move || {
            drop(owner);
            dropped.send(()).unwrap();
        });
        completion
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("Listener owner must join its pipe workers");
        listener.join().unwrap();
        let (finite, result) = exit
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(!finite);
        if subscribe {
            assert_eq!(result, Ok(()));
        } else {
            assert_eq!(result, Err(Some(ERROR_OPERATION_ABORTED as i32)));
        }
        assert!(!stopped());
        drop(pipe_client);
    }

    #[test]
    fn listener_owner_drop_joins_client_waiting_for_first_request() {
        drop_listener_owner(false);
    }

    #[test]
    fn listener_owner_drop_joins_idle_log_subscription() {
        drop_listener_owner(true);
    }
}
