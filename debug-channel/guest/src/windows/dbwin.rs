use super::*;
use windows_sys::Win32::{
    Globalization::*,
    Security::{Authorization::*, SECURITY_ATTRIBUTES},
    System::Memory::*,
};

pub fn decode_ansi(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    unsafe {
        let size = MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        let mut utf16 = vec![0u16; size as usize];
        MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            utf16.as_mut_ptr(),
            size,
        );
        String::from_utf16_lossy(&utf16)
    }
}

pub fn collect(
    global: bool,
    mut emit: impl FnMut(Capture),
    mut alive: impl FnMut() -> bool,
    ready_callback: impl FnOnce(),
) -> io::Result<()> {
    unsafe {
        let prefix = if global { "Global\\" } else { "" };
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide("D:(A;;GA;;;WD)(A;;GA;;;AN)S:(ML;;NW;;;S-1-16-0)").as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(error());
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let ready = Handle(CreateEventW(
            &sa,
            0,
            0,
            wide(&format!("{prefix}DBWIN_BUFFER_READY")).as_ptr(),
        ));
        let existing = GetLastError() == ERROR_ALREADY_EXISTS;
        let data = Handle(CreateEventW(
            &sa,
            0,
            0,
            wide(&format!("{prefix}DBWIN_DATA_READY")).as_ptr(),
        ));
        let section = Handle(CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            &sa,
            PAGE_READWRITE,
            0,
            4096,
            wide(&format!("{prefix}DBWIN_BUFFER")).as_ptr(),
        ));
        LocalFree(descriptor);
        if existing {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "Debug output is being collected by another application",
            ));
        }
        if ready.0.is_null() || data.0.is_null() || section.0.is_null() {
            return Err(error());
        }
        let view = MapViewOfFile(section.0, FILE_MAP_READ, 0, 0, 4096);
        if view.Value.is_null() {
            return Err(error());
        }
        ready_callback();
        while alive() {
            SetEvent(ready.0);
            match WaitForSingleObject(data.0, 200) {
                WAIT_TIMEOUT => continue,
                WAIT_OBJECT_0 => {}
                _ => {
                    UnmapViewOfFile(view);
                    return Err(error());
                }
            }
            let bytes = std::slice::from_raw_parts(view.Value as *const u8, 4096);
            let pid = u32::from_ne_bytes(bytes[..4].try_into().unwrap());
            let size = bytes[4..].iter().position(|&b| b == 0).unwrap_or(4092);
            let payload = bytes[4..4 + size].to_vec();
            let observed_filetime = clock::filetime();
            let mut session = 0;
            let session_id = (ProcessIdToSessionId(pid, &mut session) != 0).then_some(session);
            let text = decode_ansi(&payload);
            emit(Capture {
                pid: Some(pid),
                tid: None,
                session_id,
                observed_at: timestamp_after(0),
                observed_filetime,
                source_time: None,
                source: "dbwin".into(),
                text,
                raw_level: None,
                process: Some(process::identity(pid)),
                component: None,
            });
        }
        UnmapViewOfFile(view);
        Ok(())
    }
}
