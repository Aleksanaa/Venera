use super::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::{Environment::*, RemoteDesktop::*};

struct Helper {
    process: Handle,
}

fn start(session: u32, state: &Shared, job: HANDLE) -> io::Result<Helper> {
    unsafe {
        let mut raw = std::ptr::null_mut();
        if WTSQueryUserToken(session, &mut raw) == 0 {
            return Err(error());
        }
        let token = Handle(raw);
        let exe = std::env::current_exe()?;
        let app = wide(&exe.to_string_lossy());
        let mut command = wide(&format!("\"{}\" session-helper", exe.display()));
        let mut environment = std::ptr::null_mut();
        if CreateEnvironmentBlock(&mut environment, token.0, 0) == 0 {
            return Err(error());
        }
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        // The desktop buffer stays alive through CreateProcessAsUserW.
        let mut desktop = wide("winsta0\\default");
        startup.lpDesktop = desktop.as_mut_ptr();
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessAsUserW(
            token.0,
            app.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
            environment,
            std::ptr::null(),
            &startup,
            &mut info,
        );
        let err = error();
        DestroyEnvironmentBlock(environment);
        if ok == 0 {
            return Err(err);
        }
        let thread = Handle(info.hThread);
        let process = Handle(info.hProcess);
        if AssignProcessToJobObject(job, process.0) == 0 {
            let issue = error();
            TerminateProcess(process.0, 1);
            return Err(issue);
        }
        state
            .lock()
            .unwrap()
            .helpers
            .insert(session, process::identity(info.dwProcessId));
        if ResumeThread(thread.0) == u32::MAX {
            let issue = error();
            TerminateProcess(process.0, 1);
            state.lock().unwrap().helpers.remove(&session);
            return Err(issue);
        }
        Ok(Helper { process })
    }
}

pub fn manage(state: Shared) {
    let job = unsafe {
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
            transport::health(
                &state,
                "sessions",
                serde_json::json!({"error":error().to_string()}),
            );
            return;
        }
        job
    };
    let mut helpers: std::collections::BTreeMap<u32, Helper> = Default::default();
    while !stopped() {
        unsafe {
            let mut sessions = std::ptr::null_mut();
            let mut count = 0;
            if WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut sessions, &mut count)
                != 0
            {
                let mut logged_in = Vec::new();
                for session in std::slice::from_raw_parts(sessions, count as usize) {
                    if session.SessionId == 0 {
                        continue;
                    }
                    let mut username = std::ptr::null_mut();
                    let mut bytes = 0;
                    let has_user = WTSQuerySessionInformationW(
                        WTS_CURRENT_SERVER_HANDLE,
                        session.SessionId,
                        WTSUserName,
                        &mut username,
                        &mut bytes,
                    ) != 0
                        && bytes > 2;
                    if !username.is_null() {
                        WTSFreeMemory(username.cast());
                    }
                    if has_user {
                        logged_in.push((session.SessionId, session.State));
                    }
                }
                WTSFreeMemory(sessions.cast());
                helpers.retain(|id, helper| {
                    let alive = logged_in.iter().any(|(session, _)| session == id)
                        && WaitForSingleObject(helper.process.0, 0) == WAIT_TIMEOUT;
                    if !alive {
                        TerminateProcess(helper.process.0, 0);
                        let mut shared = state.lock().unwrap();
                        shared.helpers.remove(id);
                        if let Some(loss) =
                            shared.status.health["session_helper_loss"].as_object_mut()
                        {
                            if loss.remove(&id.to_string()).is_some() {
                                shared.notify();
                            }
                        }
                    }
                    alive
                });
                let mut statuses = Vec::new();
                for (id, connection) in logged_in {
                    let mut issue = None;
                    if let std::collections::btree_map::Entry::Vacant(entry) = helpers.entry(id) {
                        match start(id, &state, job.0) {
                            Ok(helper) => {
                                entry.insert(helper);
                            }
                            Err(e) => issue = Some(e.to_string()),
                        }
                    }
                    statuses.push(serde_json::json!({"session_id":id,"connection_state":connection,"helper":if helpers.contains_key(&id) {"running"} else {"retrying"},"error":issue}));
                }
                let mut state = state.lock().unwrap();
                let status = serde_json::json!(statuses);
                if state.status.health["sessions"] != status {
                    state.status.health["sessions"] = status;
                    state.notify();
                }
            }
        }
        process::update_ancestry(&state);
        for _ in 0..10 {
            if stopped() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    for (_, helper) in helpers {
        unsafe {
            TerminateProcess(helper.process.0, 0);
            WaitForSingleObject(helper.process.0, 3000);
        }
    }
}
