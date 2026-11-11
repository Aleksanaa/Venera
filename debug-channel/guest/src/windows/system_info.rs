use super::*;
use std::{
    io::Read,
    os::windows::io::FromRawHandle,
    path::PathBuf,
    sync::{mpsc, OnceLock},
    time::{Duration, Instant},
};
use venera_debug_protocol::{RequestError, SystemInfoResult, SystemInfoSection};
use windows_sys::Win32::{
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::*,
    System::{JobObjects::*, Pipes::*, Registry::*, RemoteDesktop::*},
};

#[path = "system_info_pending.rs"]
mod pending;
use pending::PendingReport;
static PENDING: OnceLock<Mutex<Option<PendingReport>>> = OnceLock::new();
fn pending() -> &'static Mutex<Option<PendingReport>> {
    PENDING.get_or_init(|| Mutex::new(None))
}

pub fn helper_command(session: u32) -> Option<String> {
    pending().lock().unwrap().as_mut()?.command(session)
}
pub fn helper_result(session: u32, result: SystemInfoResult) {
    if let Some(pending) = pending().lock().unwrap().as_ref() {
        pending.deliver(session, result);
    }
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

fn failure(request_id: String, session_id: Option<u32>, code: &str, message: String) -> Envelope {
    Envelope::new(Message::SystemInfoResult {
        result: SystemInfoResult {
            request_id,
            ok: false,
            collected_at: timestamp_after(0),
            session_id,
            sections: Vec::new(),
            error: Some(RequestError {
                code: code.into(),
                message,
            }),
        },
    })
}

pub fn request(state: Shared, request_id: String) -> Option<Envelope> {
    if ACTIVE.swap(true, Ordering::AcqRel) {
        return Some(failure(
            request_id,
            None,
            "busy",
            "A system report is being collected".into(),
        ));
    }
    let fallback = request_id.clone();
    let result = std::thread::Builder::new()
        .name("system-info".into())
        .spawn(move || {
            struct Active;
            impl Drop for Active {
                fn drop(&mut self) {
                    ACTIVE.store(false, Ordering::Release);
                }
            }
            let _active = Active;
            let session = unsafe { WTSGetActiveConsoleSessionId() };
            let helper = session != u32::MAX
                && session != 0
                && state.lock().unwrap().helpers.contains_key(&session);
            let response = if helper {
                let (sender, receiver) = mpsc::sync_channel(1);
                *pending().lock().unwrap() =
                    Some(PendingReport::new(request_id.clone(), session, sender));
                let result = pending::wait(&receiver, Duration::from_secs(30), &STOP);
                pending::clear(&mut pending().lock().unwrap(), &request_id);
                match result {
                    Some(result) => Envelope::new(Message::SystemInfoResult { result }),
                    None => failure(
                        request_id,
                        Some(session),
                        "timeout",
                        "System report collection timed out".into(),
                    ),
                }
            } else {
                Envelope::new(Message::SystemInfoResult {
                    result: collect_local(request_id, 0, &STOP),
                })
            };
            while !stopped() {
                if state.lock().unwrap().queue_control(&response) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    if let Err(issue) = result {
        ACTIVE.store(false, Ordering::Release);
        return Some(failure(fallback, None, "start_failed", issue.to_string()));
    }
    None
}

fn installed_panel() -> io::Result<PathBuf> {
    unsafe {
        let mut data = vec![0u16; 32768];
        let mut bytes = (data.len() * 2) as u32;
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Venera").as_ptr(),
            wide("InstallLocation").as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            std::ptr::null_mut(),
            data.as_mut_ptr().cast(),
            &mut bytes,
        ) == 0
        {
            let end = data.iter().position(|c| *c == 0).unwrap_or(data.len());
            let path =
                PathBuf::from(String::from_utf16_lossy(&data[..end])).join("venera-panel.exe");
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    let path = PathBuf::from(
        std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()),
    )
    .join("Venera")
    .join("venera-panel.exe");
    if path.is_file() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Venera panel installation was not found",
        ))
    }
}

unsafe fn run(cancel: &AtomicBool) -> io::Result<Vec<u8>> {
    let started = Instant::now();
    let exe = installed_panel()?;
    let job = Handle(CreateJobObjectW(std::ptr::null(), std::ptr::null()));
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if job.0.is_null()
        || SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) == 0
    {
        return Err(error());
    }
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    if CreatePipe(&mut read, &mut write, &security, 0) == 0 {
        return Err(error());
    }
    let read = Handle(read);
    let write = Handle(write);
    if SetHandleInformation(read.0, HANDLE_FLAG_INHERIT, 0) == 0 {
        return Err(error());
    }
    let null = Handle(CreateFileW(
        wide("NUL").as_ptr(),
        GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        &security,
        OPEN_EXISTING,
        0,
        std::ptr::null_mut(),
    ));
    if null.0 == INVALID_HANDLE_VALUE {
        return Err(error());
    }
    let mut startup: STARTUPINFOW = std::mem::zeroed();
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdOutput = write.0;
    startup.hStdError = null.0;
    startup.hStdInput = null.0;
    let app = wide(&exe.to_string_lossy());
    let mut command = wide(&format!("\"{}\" /info /json", exe.display()));
    let mut info: PROCESS_INFORMATION = std::mem::zeroed();
    let flags = CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT;
    let ok = CreateProcessW(
        app.as_ptr(),
        command.as_mut_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        1,
        flags,
        std::ptr::null(),
        std::ptr::null(),
        &startup,
        &mut info,
    );
    if ok == 0 {
        return Err(error());
    }
    let process = Handle(info.hProcess);
    let thread = Handle(info.hThread);
    if AssignProcessToJobObject(job.0, process.0) == 0 {
        let issue = error();
        TerminateProcess(process.0, 1);
        return Err(issue);
    }
    drop(write);
    let mut file = std::fs::File::from_raw_handle(read.0);
    std::mem::forget(read);
    let reader = std::thread::Builder::new()
        .name("system-info-output".into())
        .spawn(move || {
            let mut output = Vec::new();
            let mut buffer = [0u8; 16384];
            let mut oversized = false;
            loop {
                let count = match file.read(&mut buffer) {
                    Ok(count) => count,
                    Err(issue) if issue.kind() == io::ErrorKind::BrokenPipe => break,
                    Err(issue) => return Err(issue),
                };
                if count == 0 {
                    break;
                }
                if output.len() + count <= 900_000 {
                    output.extend_from_slice(&buffer[..count]);
                } else {
                    oversized = true;
                }
            }
            if oversized {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "System report is too large",
                ))
            } else {
                Ok(output)
            }
        })?;
    let mut issue = None;
    if ResumeThread(thread.0) == u32::MAX {
        issue = Some(error());
    }
    while issue.is_none() {
        match WaitForSingleObject(process.0, 50) {
            WAIT_OBJECT_0 => break,
            WAIT_TIMEOUT => (),
            _ => {
                issue = Some(error());
                break;
            }
        }
        if stopped()
            || cancel.load(Ordering::Acquire)
            || started.elapsed() >= Duration::from_secs(30)
        {
            issue = Some(io::Error::new(
                io::ErrorKind::TimedOut,
                "System report collection timed out",
            ));
        }
    }
    let mut code = 0;
    if issue.is_none() && GetExitCodeProcess(process.0, &mut code) == 0 {
        issue = Some(error());
    }
    drop(job);
    let output = reader
        .join()
        .map_err(|_| io::Error::other("System report output interrupted"))?;
    if let Some(issue) = issue {
        return Err(issue);
    }
    if code > 1 {
        return Err(io::Error::other(format!(
            "System report process ended with status {code}"
        )));
    }
    output
}

pub fn collect_local(request_id: String, session: u32, cancel: &AtomicBool) -> SystemInfoResult {
    let result = unsafe { collect(cancel) };
    match result {
        Ok(sections) => SystemInfoResult {
            request_id,
            ok: true,
            collected_at: timestamp_after(0),
            session_id: Some(session),
            sections,
            error: None,
        },
        Err((code, issue)) => {
            let Envelope {
                body: Message::SystemInfoResult { result },
                ..
            } = failure(request_id, Some(session), code, issue.to_string())
            else {
                unreachable!()
            };
            result
        }
    }
}

unsafe fn collect(
    cancel: &AtomicBool,
) -> Result<Vec<SystemInfoSection>, (&'static str, io::Error)> {
    let bytes = run(cancel).map_err(|e| {
        (
            if e.kind() == io::ErrorKind::TimedOut {
                "timeout"
            } else {
                "collection_failed"
            },
            e,
        )
    })?;
    parse_report(&bytes)
}

fn parse_report(bytes: &[u8]) -> Result<Vec<SystemInfoSection>, (&'static str, io::Error)> {
    #[derive(serde::Deserialize)]
    struct Report {
        sections: Vec<SystemInfoSection>,
    }
    serde_json::from_slice::<Report>(bytes)
        .map(|r| r.sections)
        .map_err(|e| {
            (
                "invalid_report",
                io::Error::new(io::ErrorKind::InvalidData, e),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_json_report_preserves_warning_text() {
        let sections =
            parse_report(include_bytes!("../../../protocol/tests/panel-report.json")).unwrap();
        assert_eq!(
            serde_json::json!({"sections":sections}),
            serde_json::json!({"sections":[{"title":"Host","rows":[{"key":"GPU","value":"Unknown: Venus did not answer","bad":true,"warning":"请检查图形驱动。\n重新启动 Windows 后重试。"},{"key":"Renderer","value":"Venus","bad":false,"warning":""}]}]})
        );
    }
}
