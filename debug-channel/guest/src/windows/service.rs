use super::*;
use std::sync::atomic::AtomicPtr;
use windows_sys::Win32::{
    Storage::FileSystem::DELETE,
    System::{Registry::*, Services::*},
};

pub const NAME: &str = "VeneraDebugGuest";

fn history_root() -> io::Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from(
        std::env::var_os("ProgramData")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ProgramData is unavailable"))?,
    )
    .join("Venera")
    .join("DebugHistory"))
}

pub fn prepare_history() -> io::Result<()> {
    crate::journal::Journal::open(
        history_root()?,
        format!("{}-{}", clock::filetime(), unsafe { GetCurrentProcessId() }),
        crate::journal::RetentionPolicy {
            max_bytes: crate::capture_storage::JOURNAL_BYTES,
            ..Default::default()
        },
    )?
    .close()
}
static STATUS: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
unsafe fn report(current: u32, code: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: current,
        dwControlsAccepted: if current == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE
        } else {
            0
        },
        dwWin32ExitCode: code,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: if current == SERVICE_STOP_PENDING {
            10000
        } else {
            0
        },
    };
    SetServiceStatus(STATUS.load(Ordering::Acquire), &status);
}
unsafe extern "system" fn handler(
    control: u32,
    _: u32,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> u32 {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        STOP.store(true, Ordering::Release);
        report(SERVICE_STOP_PENDING, 0);
    }
    NO_ERROR
}
unsafe extern "system" fn main(_: u32, _: *mut *mut u16) {
    let handle =
        RegisterServiceCtrlHandlerExW(wide(NAME).as_ptr(), Some(handler), std::ptr::null());
    if handle.is_null() {
        return;
    }
    STATUS.store(handle, Ordering::Release);
    report(SERVICE_START_PENDING, 0);
    let stream_id = format!("{}-{}", timestamp_after(0), GetCurrentProcessId());
    let configured = forwarding();
    let mut initial = State::new(stream_id, configured.as_ref().copied().unwrap_or(false));
    let history_stop = Arc::new(AtomicBool::new(false));
    let history = (|| -> io::Result<_> {
        let boot_id = clock::boot_id()?;
        let root = history_root()?;
        let journal = crate::journal::Journal::open(
            root,
            initial.stream_id.clone(),
            crate::journal::RetentionPolicy {
                max_bytes: crate::capture_storage::JOURNAL_BYTES,
                ..Default::default()
            },
        )?;
        let (history, worker) = crate::history::History::start(journal, history_stop.clone())?;
        initial.boot_id = boot_id;
        initial.history = Some(history);
        Ok(worker)
    })();
    let history_worker = match history {
        Ok(worker) => worker,
        Err(_) => {
            report(SERVICE_STOPPED, ERROR_WRITE_FAULT);
            return;
        }
    };
    if let Err(e) = configured {
        initial.status.health["configuration"] = serde_json::json!({"error":e.to_string()});
    }
    let state = Arc::new(Mutex::new(initial));
    let mut threads = Vec::new();
    let shared = state.clone();
    threads.push(std::thread::spawn(move || ipc::serve(shared)));
    let shared = state.clone();
    threads.push(std::thread::spawn(move || sessions::manage(shared)));
    let shared = state.clone();
    threads.push(std::thread::spawn(move || transport::serve(shared)));
    let shared = state.clone();
    threads.push(std::thread::spawn(move || etw::collect(shared)));
    let shared = state.clone();
    threads.push(std::thread::spawn(move || capture::collect(shared)));
    let shared = state.clone();
    threads.push(std::thread::spawn(move || {
        while !stopped() {
            let result = dbwin::collect(
                true,
                |capture| accept(&shared, capture),
                || !stopped(),
                || transport::health(&shared, "dbwin", serde_json::json!("collecting")),
            );
            let mut state = shared.lock().unwrap();
            state.status.health["dbwin"] = serde_json::json!(match result {
                Ok(()) => "stopped".into(),
                Err(e) => e.to_string(),
            });
            state.notify();
            drop(state);
            if !stopped() {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }));
    report(SERVICE_RUNNING, 0);
    while !stopped() {
        if history_worker.is_finished() {
            STOP.store(true, Ordering::Release);
            break;
        }
        state.lock().unwrap().expire();
        expire_native_captures(&state);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Producers can finish submitting captures while history is still running.
    for producer in threads {
        let _ = producer.join();
    }
    finish_native_captures(&state);
    let code = match crate::history::finish(Vec::new(), &history_stop, history_worker) {
        Ok(()) => 0,
        _ => ERROR_WRITE_FAULT,
    };
    report(SERVICE_STOPPED, code);
}
pub fn dispatch() -> io::Result<()> {
    let mut name = wide(NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        Err(error())
    } else {
        Ok(())
    }
}
pub fn server_process() -> io::Result<u32> {
    unsafe {
        let manager = ScHandle(OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CONNECT,
        ));
        if manager.0.is_null() {
            return Err(error());
        }
        let service = ScHandle(OpenServiceW(
            manager.0,
            wide(NAME).as_ptr(),
            SERVICE_QUERY_STATUS,
        ));
        if service.0.is_null() {
            return Err(error());
        }
        let mut status: SERVICE_STATUS_PROCESS = std::mem::zeroed();
        let mut required = 0;
        if QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut required,
        ) == 0
        {
            return Err(error());
        }
        if status.dwProcessId == 0 {
            return Err(io::ErrorKind::NotConnected.into());
        }
        Ok(status.dwProcessId)
    }
}
struct ScHandle(SC_HANDLE);
impl Drop for ScHandle {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
pub fn install() -> io::Result<()> {
    unsafe {
        let manager = ScHandle(OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CREATE_SERVICE,
        ));
        if manager.0.is_null() {
            return Err(error());
        }
        let command = wide(&format!(
            "\"{}\" service",
            std::env::current_exe()?.display()
        ));
        let service = ScHandle(CreateServiceW(
            manager.0,
            wide(NAME).as_ptr(),
            wide("Venera Debug Guest").as_ptr(),
            SERVICE_QUERY_STATUS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            command.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        ));
        if service.0.is_null() {
            return Err(error());
        }
        Ok(())
    }
}
pub fn uninstall() -> io::Result<()> {
    unsafe {
        let manager = ScHandle(OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CONNECT,
        ));
        if manager.0.is_null() {
            return Err(error());
        }
        let service = ScHandle(OpenServiceW(
            manager.0,
            wide(NAME).as_ptr(),
            DELETE | SERVICE_STOP | SERVICE_QUERY_STATUS,
        ));
        if service.0.is_null() {
            return Err(error());
        }
        let mut status = std::mem::zeroed();
        ControlService(service.0, SERVICE_CONTROL_STOP, &mut status);
        if DeleteService(service.0) == 0 {
            return Err(error());
        }
        Ok(())
    }
}
const KEY: &str = "SOFTWARE\\Venera\\DebugChannel";
pub fn forwarding() -> io::Result<bool> {
    unsafe {
        let mut value = 1u32;
        let mut bytes = 4;
        let code = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide(KEY).as_ptr(),
            wide("ForwardingEnabled").as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut bytes,
        );
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(true);
        }
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        Ok(value != 0)
    }
}
pub fn persist_forwarding(enabled: bool) -> io::Result<()> {
    unsafe {
        let mut key = std::ptr::null_mut();
        let code = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(KEY).as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        );
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        let value = u32::from(enabled);
        let code = RegSetValueExW(
            key,
            wide("ForwardingEnabled").as_ptr(),
            0,
            REG_DWORD,
            (&value as *const u32).cast(),
            4,
        );
        RegCloseKey(key);
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code as i32))
        }
    }
}
