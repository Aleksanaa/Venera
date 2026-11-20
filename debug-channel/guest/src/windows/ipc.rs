use super::*;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle},
    sync::mpsc,
    time::{Duration, Instant},
};
use venera_debug_protocol::{Filter, Message};
use windows_sys::Win32::{
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::Pipes::*,
    System::IO::*,
};
const PIPE: &str = "\\\\.\\pipe\\VeneraDebugGuest.v1";
const RPC_TIMEOUT: Duration = Duration::from_secs(5);
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
fn connect_file() -> io::Result<File> {
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
            SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION | FILE_FLAG_OVERLAPPED,
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
struct ClientPipe {
    handle: Arc<Handle>,
    deadline: Option<Instant>,
    stop: Arc<AtomicBool>,
    #[cfg(test)]
    io_pending: Option<Arc<AtomicBool>>,
}

fn connect_pipe() -> io::Result<ClientPipe> {
    let file = connect_file()?;
    Ok(ClientPipe {
        handle: Arc::new(Handle(file.into_raw_handle())),
        deadline: None,
        stop: Arc::new(AtomicBool::new(false)),
        #[cfg(test)]
        io_pending: None,
    })
}

impl ClientPipe {
    fn check_io(&self) -> io::Result<()> {
        if self.stop.load(Ordering::Acquire) || stopped() {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        if self.deadline.is_some_and(|end| Instant::now() >= end) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }

    unsafe fn complete(&self, overlapped: *mut OVERLAPPED, event: HANDLE) -> io::Result<usize> {
        #[cfg(test)]
        if let Some(pending) = &self.io_pending {
            pending.store(true, Ordering::Release);
        }
        let mut transferred = 0;
        loop {
            let issue = match self.check_io() {
                Err(issue) => issue,
                Ok(()) => {
                    let wait_ms = self.deadline.map_or(50, |end| {
                        end.saturating_duration_since(Instant::now())
                            .as_millis()
                            .clamp(1, 50) as u32
                    });
                    match WaitForSingleObject(event, wait_ms) {
                        WAIT_OBJECT_0 => {
                            if GetOverlappedResult(self.handle.0, overlapped, &mut transferred, 0)
                                == 0
                            {
                                return Err(error());
                            }
                            return Ok(transferred as usize);
                        }
                        WAIT_TIMEOUT => continue,
                        _ => error(),
                    }
                }
            };
            CancelIoEx(self.handle.0, overlapped);
            // Cancellation races completion. Reap either outcome before releasing the buffer and event.
            if GetOverlappedResult(self.handle.0, overlapped, &mut transferred, 1) != 0 {
                return Ok(transferred as usize);
            }
            let completion = error();
            return Err(
                if completion.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) {
                    issue
                } else {
                    completion
                },
            );
        }
    }
}

impl Read for ClientPipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check_io()?;
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
                &raw mut overlapped,
            );
            if submitted == 0 {
                let issue = error();
                if issue.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(issue);
                }
                return self.complete(&raw mut overlapped, event.0);
            }
            Ok(transferred as usize)
        }
    }
}

impl Write for ClientPipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check_io()?;
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
                &raw mut overlapped,
            ) == 0
            {
                let issue = error();
                if issue.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(issue);
                }
                return self.complete(&raw mut overlapped, event.0);
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
    let mut first = Vec::new();
    if reader
        .by_ref()
        .take((LIMIT + 1) as u64)
        .read_until(b'\n', &mut first)?
        == 0
    {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&first).unwrap_or(serde_json::Value::Null);
    *finite = false;
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
                let (lost, stream_id) = {
                    let mut state = state.lock().unwrap();
                    let key = session.to_string();
                    let previous = &state.status.health["session_helper_loss"][&key];
                    let before = if previous["pid"] == pid {
                        previous["dropped_total"].as_u64().unwrap_or(0)
                    } else {
                        0
                    };
                    let lost = frame.dropped.saturating_sub(before);
                    let mut changed = state.status.health["session_capture"][&key] != frame.health;
                    state.status.health["session_capture"][&key] = frame.health;
                    changed |= state.observe_helper_drops(session, pid, frame.dropped);
                    if changed {
                        state.notify();
                    }
                    (lost, state.stream_id.clone())
                };
                if lost != 0 {
                    retain_event(
                        &state,
                        venera_debug_protocol::v1::HistoryEvent::Gap {
                            gap: venera_debug_protocol::v1::Gap {
                                stream_id,
                                reason: "session_capture_overflow".into(),
                                count: Some(venera_debug_protocol::v1::DecimalU64(lost)),
                                first_seq: None,
                                last_seq: None,
                            },
                        },
                    );
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
        _ => rpc_client(file, reader, state, &first, local_stop),
    }
}

fn rpc_client(
    mut file: File,
    mut reader: BufReader<File>,
    state: Shared,
    first: &[u8],
    local_stop: &AtomicBool,
) -> io::Result<()> {
    use crate::connection::Connection;
    let peer = state.lock().unwrap().add_local_peer();
    let mut connection = Connection::local(peer, unsafe { admin(file.as_raw_handle()) });
    let result = (|| {
        for request in connection.connect() {
            reply(&mut file, &request)?;
        }
        let mut bytes = first.to_vec();
        while !client_stopped(local_stop) {
            let received = connection.receive_negotiated(
                &bytes,
                |message| {
                    let expires = match &message {
                        Message::FilterSet {
                            duration_ms: Some(ms),
                            ..
                        } => Some(timestamp_after(*ms)),
                        _ => None,
                    };
                    state.lock().unwrap().control(message, expires)
                },
                |request, local, encoding| history_request(&state, request, local, encoding),
            );
            for value in received.output {
                reply(&mut file, &value)?;
            }
            for request in received.reports {
                if let Some(result) = system_info::request(state.clone(), peer, request) {
                    if let Some(value) = connection.report(&result) {
                        reply(&mut file, &value)?;
                    }
                }
            }
            bytes.clear();
            loop {
                if client_stopped(local_stop) {
                    return Ok(());
                }
                if state.lock().unwrap().peer_closed(peer) {
                    return Err(io::ErrorKind::ConnectionAborted.into());
                }
                let next = state.lock().unwrap().next_line(peer);
                if let Some((_, bytes)) = next {
                    let envelope = venera_debug_protocol::decode(&bytes)
                        .map_err(|_| io::ErrorKind::InvalidData)?;
                    if matches!(envelope.body, Message::SystemInfoResult { .. }) {
                        if let Some(value) = connection.report(&envelope) {
                            reply(&mut file, &value)?;
                        }
                    } else {
                        file.write_all(&crate::state::wire::encode(&envelope)?)?;
                    }
                }
                let mut available = 0;
                if unsafe {
                    PeekNamedPipe(
                        file.as_raw_handle(),
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(error());
                }
                if available > 0 || !reader.buffer().is_empty() {
                    let mut chunk = [0; 16384];
                    let count = reader.read(&mut chunk)?;
                    if count == 0 {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        Ok(())
    })();
    state.lock().unwrap().remove_peer(peer);
    result
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
    reader: BufReader<ClientPipe>,
    session: u32,
    stop: Arc<AtomicBool>,
    reports: mpsc::SyncSender<venera_debug_protocol::SystemInfoResult>,
) {
    helper_commands_with(reader, session, stop, reports, system_info::collect_local);
}

fn helper_commands_with(
    mut reader: BufReader<ClientPipe>,
    session: u32,
    stop: Arc<AtomicBool>,
    reports: mpsc::SyncSender<venera_debug_protocol::SystemInfoResult>,
    collect: impl Fn(String, u32, &AtomicBool) -> venera_debug_protocol::SystemInfoResult,
) {
    while !stop.load(Ordering::Acquire) {
        let bytes = match line(&mut reader) {
            Ok(bytes) => bytes,
            Err(_error) => {
                #[cfg(test)]
                eprintln!("helper command frame read: {_error:?}");
                break;
            }
        };
        let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => value,
            Err(_error) => {
                #[cfg(test)]
                eprintln!(
                    "helper command JSON decode ({} bytes): {_error}",
                    bytes.len()
                );
                break;
            }
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

fn capture_handshake(
    file: &mut ClientPipe,
    deadline: Instant,
) -> io::Result<BufReader<ClientPipe>> {
    file.deadline = Some(deadline);
    file.write_all(b"{\"type\":\"local.capture\"}\n")?;
    let mut reader = BufReader::new(file.clone());
    let ack: serde_json::Value = serde_json::from_slice(&line(&mut reader)?)?;
    if ack["ok"] != true {
        return Err(io::ErrorKind::InvalidData.into());
    }
    file.check_io()?;
    file.deadline = None;
    reader.get_mut().deadline = None;
    Ok(reader)
}

struct HelperOutput {
    captures: mpsc::Receiver<Capture>,
    pending: Option<Capture>,
    status: Arc<Mutex<serde_json::Value>>,
    dropped: Arc<std::sync::atomic::AtomicU64>,
}

impl HelperOutput {
    fn send_frames(
        &mut self,
        file: &mut impl Write,
        reports: mpsc::Receiver<venera_debug_protocol::SystemInfoResult>,
        commands_finished: impl Fn() -> bool,
    ) -> io::Result<()> {
        loop {
            if self.pending.is_none() {
                self.pending = match self.captures.recv_timeout(Duration::from_secs(1)) {
                    Ok(capture) => Some(capture),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                };
            }
            if commands_finished() {
                return Ok(());
            }
            let frame = HelperFrame {
                capture: self.pending.take(),
                health: self.status.lock().unwrap().clone(),
                dropped: self.dropped.load(Ordering::Relaxed),
                system_info: reports.try_recv().ok(),
            };
            let encoded = serde_json::to_vec(&frame);
            self.pending = frame.capture;
            let mut bytes = encoded?;
            bytes.push(b'\n');
            let first = file.write(&bytes)?;
            if first == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            // Only an untouched capture can move to the next connection.
            let had_capture = self.pending.take().is_some();
            if let Err(issue) = file.write_all(&bytes[first..]) {
                if had_capture {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
                return Err(issue);
            }
        }
    }
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
    let mut output = HelperOutput {
        captures: rx,
        pending: None,
        status,
        dropped,
    };
    loop {
        if let Ok(mut file) = connect_pipe() {
            let connection_stop = file.stop.clone();
            if let Ok(reader) = capture_handshake(&mut file, Instant::now() + RPC_TIMEOUT) {
                let reader_stop = connection_stop.clone();
                let (report_tx, report_rx) = mpsc::sync_channel(1);
                let commands = std::thread::Builder::new()
                    .name("system-info-commands".into())
                    .spawn(move || helper_commands(reader, session, reader_stop, report_tx))?;
                let _ = output.send_frames(&mut file, report_rx, || commands.is_finished());
                connection_stop.store(true, Ordering::Release);
                let _ = commands.join();
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

pub fn cli(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use venera_debug_protocol::v1::Method;
    let (method, params) = match args {
        [command, flag] if command == "status" && flag == "--json" => (Method::StatusGet, serde_json::json!({})),
        [command, state] if command == "forwarding" && ["on", "off"].contains(&state.as_str()) => (Method::ForwardingSet, serde_json::json!({"enabled":state == "on"})),
        [command, flag, path] if command == "filter-set" && flag == "--json" => {
            let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
            let filter: Filter = serde_json::from_value(value.get("filter").cloned().unwrap_or(value.clone()))?;
            let duration_ms = value.get("duration_ms").and_then(serde_json::Value::as_u64);
            (Method::FilterSet, serde_json::json!({"filter":crate::state::wire::filter_v1(&filter)?,"duration_ms":duration_ms.map(|value| value.to_string())}))
        },
        [command] if command == "filter-reset" => (Method::FilterReset, serde_json::json!({})),
        [command, flag] if command == "logs" && flag == "--follow" => return follow(),
        _ => return Err("Commands: status --json; forwarding on|off; filter-set --json FILE; filter-reset; logs --follow; install-service; uninstall-service; service; session-helper".into()),
    };
    let (mut client, mut file) = rpc_connect()?;
    let value = rpc_call(&mut client, &mut file, method, params)?;
    let value = if method == Method::StatusGet {
        value
    } else {
        serde_json::json!({"ok":true,"status":value})
    };
    serde_json::to_writer(io::stdout().lock(), &value)?;
    println!();
    Ok(())
}

fn rpc_connect() -> io::Result<(venera_debug_protocol::client::Client, ClientPipe)> {
    let mut file = connect_pipe()?;
    let client = rpc_negotiate(&mut file)?;
    Ok((client, file))
}

fn rpc_negotiate(file: &mut ClientPipe) -> io::Result<venera_debug_protocol::client::Client> {
    let mut client = venera_debug_protocol::client::Client::default();
    client.connect();
    file.deadline = Some(Instant::now() + RPC_TIMEOUT);
    while client
        .require(venera_debug_protocol::v1::Method::Discover)
        .is_err()
    {
        while client.has_output() {
            client.write(file)?;
        }
        for reply in client.read(file)? {
            reply
                .result
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.message))?;
        }
        if let Some(error) = &client.negotiation().failure {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                error.message.clone(),
            ));
        }
    }
    Ok(client)
}

fn rpc_call(
    client: &mut venera_debug_protocol::client::Client,
    file: &mut ClientPipe,
    method: venera_debug_protocol::v1::Method,
    params: serde_json::Value,
) -> io::Result<serde_json::Value> {
    file.deadline = Some(Instant::now() + RPC_TIMEOUT);
    let id = client
        .request(method, params)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.message))?;
    loop {
        while client.has_output() {
            client.write(file)?;
        }
        for reply in client.read(file)? {
            if reply.id == id {
                return reply.result.map_err(|error| {
                    io::Error::new(
                        if error.code == -32005 {
                            io::ErrorKind::WouldBlock
                        } else {
                            io::ErrorKind::Other
                        },
                        error.message,
                    )
                });
            }
        }
    }
}

fn follow() -> Result<(), Box<dyn std::error::Error>> {
    use venera_debug_protocol::{
        client::{HistoryConsumer, Start},
        v1::*,
    };
    let mut consumer = HistoryConsumer::new(Start::Now);
    let mut connection = None;
    loop {
        let page = (|| -> io::Result<(HistoryPage, venera_debug_protocol::Filter)> {
            if connection.is_none() {
                connection = Some(rpc_connect()?);
            }
            let (client, file) = connection.as_mut().unwrap();
            let status: Status = serde_json::from_value(rpc_call(
                client,
                file,
                Method::StatusGet,
                serde_json::json!({}),
            )?)?;
            let range = serde_json::from_value(rpc_call(
                client,
                file,
                Method::HistoryRange,
                serde_json::json!({}),
            )?)?;
            consumer
                .range(range, None)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.message))?;
            let page: HistoryPage = serde_json::from_value(rpc_call(
                client,
                file,
                Method::HistoryRead,
                serde_json::to_value(consumer.read_request())?,
            )?)?;
            consumer
                .page(&page)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.message))?;
            Ok((
                page,
                crate::state::wire::filter_legacy(status.effective_filter),
            ))
        })();
        let (page, filter) = match page {
            Ok(page) => page,
            Err(error) => {
                if error.kind() != io::ErrorKind::WouldBlock {
                    connection = None;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
        };
        let mut output = io::stdout().lock();
        consumer.display(&page, &filter, &mut output)?;
        if consumer.caught_up() {
            std::thread::sleep(std::time::Duration::from_millis(200));
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
    fn pipe_completion_reports_bytes_or_pending_cancellation() {
        for complete_first in [true, false] {
            for timeout in [true, false] {
                let (mut server, mut pipe) = helper_pair();
                let mut bytes = [0u8; 8];
                unsafe {
                    let event = Handle(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()));
                    assert!(!event.0.is_null(), "{}", error());
                    let mut op: OVERLAPPED = std::mem::zeroed();
                    op.hEvent = event.0;
                    let submitted = ReadFile(
                        pipe.handle.0,
                        bytes.as_mut_ptr(),
                        8,
                        std::ptr::null_mut(),
                        &raw mut op,
                    );
                    let status = GetLastError();
                    assert_eq!(submitted, 0);
                    assert_eq!(status, ERROR_IO_PENDING);

                    // Keep all fallible assertions until the borrowed buffer is reaped.
                    let wait = if complete_first {
                        server
                            .write_all(b"abc")
                            .map(|()| WaitForSingleObject(event.0, 2000))
                    } else {
                        Ok(WaitForSingleObject(event.0, 0))
                    };
                    let reason = if timeout {
                        pipe.deadline = Some(Instant::now());
                        io::ErrorKind::TimedOut
                    } else {
                        pipe.stop.store(true, Ordering::Release);
                        io::ErrorKind::ConnectionAborted
                    };
                    let result = pipe.complete(&raw mut op, event.0).map_err(|e| e.kind());
                    if complete_first {
                        assert_eq!(wait.unwrap(), WAIT_OBJECT_0);
                        assert_eq!(result, Ok(3));
                        assert_eq!(&bytes[..3], b"abc");
                    } else {
                        assert_eq!(wait.unwrap(), WAIT_TIMEOUT);
                        assert_eq!(result, Err(reason));
                    }
                    assert_eq!(pipe.read(&mut bytes).unwrap_err().kind(), reason);
                    assert_eq!(pipe.write(b"next").unwrap_err().kind(), reason);
                }
            }
        }
    }

    #[test]
    fn helper_reconnects_after_partial_ack() {
        let (mut stalled, mut first) = helper_pair();
        let (mut service, mut second) = helper_pair();
        stalled.write_all(b"{\"ok\":true}").unwrap();
        service.write_all(b"{\"ok\":true}\n").unwrap();
        let (done, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let failure =
                capture_handshake(&mut first, Instant::now() + Duration::from_millis(250))
                    .err()
                    .unwrap();
            assert_eq!(failure.kind(), io::ErrorKind::TimedOut);
            capture_handshake(&mut second, Instant::now() + RPC_TIMEOUT).unwrap();
            done.send(()).unwrap();
        });
        result.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            line(&mut BufReader::new(service)).unwrap(),
            b"{\"type\":\"local.capture\"}\n"
        );
        worker.join().unwrap();
    }

    fn helper_output() -> (mpsc::Sender<Capture>, HelperOutput) {
        let (tx, captures) = mpsc::channel();
        (
            tx,
            HelperOutput {
                captures,
                pending: None,
                status: Arc::new(Mutex::new(serde_json::json!("collecting"))),
                dropped: Arc::default(),
            },
        )
    }

    #[test]
    fn helper_reconnect_delivers_captures_in_order() {
        for (finished, capacity, expected, drops) in [
            (true, 0, vec!["first", "second"], 0),
            (false, 0, vec!["first", "second"], 0),
            (false, 4, vec!["second"], 1),
        ] {
            let (tx, mut output) = helper_output();
            for text in ["first", "second"] {
                tx.send(serde_json::from_value(serde_json::json!({"text":text,"source":"dbwin","observed_at":"now","observed_filetime":0})).unwrap()).unwrap();
            }
            drop(tx);
            let mut prefix = [0u8; 4];
            let mut old = &mut prefix[..capacity];
            let outcome = output.send_frames(&mut old, mpsc::channel().1, || finished);
            assert_eq!(outcome.is_ok(), finished);
            let (service, mut helper) = helper_pair();
            output
                .send_frames(&mut helper, mpsc::channel().1, || false)
                .unwrap();
            helper.write_all(b"\n").unwrap();
            let frames: Vec<HelperFrame> = BufReader::new(service)
                .lines()
                .map(Result::unwrap)
                .take_while(|s| !s.is_empty())
                .map(|s| serde_json::from_str(&s).unwrap())
                .collect();
            assert_eq!(
                frames
                    .iter()
                    .map(|f| f.capture.as_ref().unwrap().text.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(frames[0].dropped, drops);
        }
    }

    #[test]
    fn helper_duplex_delivers_health_before_report_command() {
        let (mut service, mut helper) = helper_pair();
        let pending_read = Arc::new(AtomicBool::new(false));
        let stop = helper.stop.clone();
        let (observed, received) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(service.try_clone().expect("service reader"));
            assert_eq!(
                line(&mut reader).expect("capture handshake"),
                b"{\"type\":\"local.capture\"}\n"
            );
            service
                .write_all(b"{\"ok\":true}\n")
                .expect("service acknowledgement");
            assert_eq!(
                line(&mut reader).expect("service health frame"),
                b"{\"capture\":null,\"health\":\"collecting\",\"dropped\":0}\n"
            );
            observed.send("health").unwrap();
            service
                .write_all(b"{\"type\":\"local.system.info.get\",\"request_id\":\"duplex-2\"}\n")
                .expect("service command write");
            let result = loop {
                let bytes = line(&mut reader).expect("service report frame");
                let frame: serde_json::Value =
                    serde_json::from_slice(&bytes).expect("service report JSON");
                if !frame["system_info"].is_null() {
                    break frame;
                }
            };
            assert_eq!(
                result,
                serde_json::json!({"capture":null,"health":"collecting","dropped":0,"system_info":{"request_id":"duplex-2","ok":true,"collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Venera","bad":false,"warning":""}]}]}})
            );
            observed.send("result").unwrap();
        });
        let mut helper_reader =
            capture_handshake(&mut helper, Instant::now() + Duration::from_millis(750)).unwrap();
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
        let (captures, mut output) = helper_output();
        let writer_stop = stop.clone();
        let writer = std::thread::spawn(move || {
            let _ = output.send_frames(&mut helper, report_rx, || {
                writer_stop.load(Ordering::Acquire)
            });
            written.send(()).unwrap();
        });
        let health = received.recv_timeout(std::time::Duration::from_secs(2));
        let result = received.recv_timeout(std::time::Duration::from_secs(3));
        drop(captures);
        let completed = completion.recv_timeout(std::time::Duration::from_secs(3));
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
        assert_eq!(
            completed,
            Ok(()),
            "helper report must reach the service; service observation: {result:?}"
        );
        assert_eq!(result, Ok("result"));
        assert!(
            server.is_finished() && writer.is_finished() && commands.is_finished(),
            "pipe workers must finish after cancellation"
        );
        writer.join().unwrap();
        server.join().unwrap();
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

    #[test]
    fn rpc_backpressure_expires_pending_write() {
        let (_server, mut pipe) = helper_pair();
        let pending = Arc::new(AtomicBool::new(false));
        pipe.io_pending = Some(pending.clone());
        let (done, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let payload = vec![b'x'; 1024 * 1024];
            pipe.deadline = Some(Instant::now() + Duration::from_millis(250));
            done.send(pipe.write_all(&payload).map_err(|err| err.kind()))
                .unwrap();
        });
        assert_eq!(
            result.recv_timeout(Duration::from_secs(2)),
            Ok(Err(io::ErrorKind::TimedOut))
        );
        assert!(pending.load(Ordering::Acquire));
        worker.join().unwrap();
    }

    fn helper_pair() -> (File, ClientPipe) {
        let (server, client) = pipe_pair_flags(FILE_FLAG_OVERLAPPED);
        (
            server,
            ClientPipe {
                handle: Arc::new(Handle(client.into_raw_handle())),
                deadline: None,
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
    fn local_history_reads_ordinary_program_capture() {
        let (server, mut pipe_client) = helper_pair();
        let state = Arc::new(Mutex::new(State::new("pipe-test".into(), false)));
        state.lock().unwrap().status.effective_filter.components = vec!["venus".into()];
        let root = std::env::temp_dir().join(format!(
            "venera-pipe-history-{}-{}",
            unsafe { GetCurrentProcessId() },
            NEXT_PIPE.fetch_add(1, Ordering::Relaxed)
        ));
        let journal = crate::journal::Journal::open(
            &root,
            "pipe-journal",
            crate::journal::RetentionPolicy::default(),
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, worker) = crate::history::History::start(journal, stop.clone()).unwrap();
        state.lock().unwrap().history = Some(history);
        state.lock().unwrap().boot_id = "boot".into();
        let mut owner = PipeWorkers::new();
        let local_stop = owner.local_stop.clone();
        let shared = state.clone();
        owner.threads.push(std::thread::spawn(move || {
            let mut finite = false;
            let _ = client(server, shared, &mut finite, &local_stop);
        }));
        let mut rpc = rpc_negotiate(&mut pipe_client).unwrap();
        accept(
            &state,
            Capture {
                structured: None,
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
                    ..Default::default()
                }),
                component: None,
            },
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let page = loop {
            let page = rpc_call(
                &mut rpc,
                &mut pipe_client,
                venera_debug_protocol::v1::Method::HistoryRead,
                serde_json::json!({"after":null,"through":null,"limit":256}),
            )
            .unwrap();
            if page["records"].as_array().unwrap().len() == 1 {
                break page;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(
            page["records"][0]["event"]["record"],
            serde_json::json!({"stream_id":"pipe-test","seq":"1",
                "observed_at":"2026-11-12T00:00:00Z","source_time":null,"source":"dbwin",
                "component":"","level":"unknown","raw_level":"unknown","session_id":2,
                "process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\Tools\\ordinary.exe"},
                "tid":null,"text":"ordinary program: hello 中文"})
        );
        drop(pipe_client);
        drop(owner);
        stop.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
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
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"first\",\"method\":\"endpoint.info\",\"params\":{}}\n")
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
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["method"],
                "endpoint.info"
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
    fn listener_owner_drop_joins_idle_rpc_connection() {
        drop_listener_owner(true);
    }
}
