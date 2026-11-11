use super::*;
use venera_debug_protocol::ProcessIdentity;
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;

pub fn dos_path(path: String) -> String {
    if let Some(path) = path.strip_prefix("\\??\\") {
        return path.to_owned();
    }
    if !path.starts_with("\\Device\\") {
        return path;
    }
    unsafe {
        for letter in b'A'..=b'Z' {
            let drive = format!("{}:", letter as char);
            let mut target = vec![0u16; 32768];
            if windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW(
                wide(&drive).as_ptr(),
                target.as_mut_ptr(),
                target.len() as u32,
            ) == 0
            {
                continue;
            }
            let end = target.iter().position(|c| *c == 0).unwrap_or(target.len());
            let prefix = String::from_utf16_lossy(&target[..end]);
            if path.len() > prefix.len()
                && path
                    .get(..prefix.len())
                    .is_some_and(|part| part.eq_ignore_ascii_case(&prefix))
                && path.as_bytes().get(prefix.len()) == Some(&b'\\')
            {
                return format!("{}{}", drive, &path[prefix.len()..]);
            }
        }
    }
    path
}

pub fn identity(pid: u32) -> ProcessIdentity {
    let mut result = ProcessIdentity {
        pid,
        start_time_filetime: None,
        exe_path: None,
    };
    unsafe {
        let process = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid));
        if process.0.is_null() {
            return result;
        }
        let mut created: FILETIME = std::mem::zeroed();
        let mut exit = created;
        let mut kernel = created;
        let mut user = created;
        if GetProcessTimes(process.0, &mut created, &mut exit, &mut kernel, &mut user) != 0 {
            result.start_time_filetime = Some(
                ((created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64).to_string(),
            );
        }
        let mut path = vec![0u16; 32768];
        let mut len = path.len() as u32;
        if QueryFullProcessImageNameW(process.0, 0, path.as_mut_ptr(), &mut len) != 0 {
            result.exe_path = Some(String::from_utf16_lossy(&path[..len as usize]));
        }
    }
    result
}

pub fn update_ancestry(state: &Shared) {
    unsafe {
        let snapshot = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
        if snapshot.0 == INVALID_HANDLE_VALUE {
            return;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut processes = std::collections::HashMap::new();
        let mut ok = Process32FirstW(snapshot.0, &mut entry);
        while ok != 0 {
            processes.insert(
                entry.th32ProcessID,
                (identity(entry.th32ProcessID), entry.th32ParentProcessID),
            );
            ok = Process32NextW(snapshot.0, &mut entry);
        }
        let mut state = state.lock().unwrap();
        for (identity, parent_pid) in processes.values() {
            if identity.start_time_filetime.is_none() {
                continue;
            }
            state.processes.started(identity.clone(), *parent_pid, None);
        }
    }
}
