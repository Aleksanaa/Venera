use windows_sys::Win32::{
    Foundation::{FILETIME, SYSTEMTIME},
    System::{SystemInformation::GetSystemTimeAsFileTime, Time::FileTimeToSystemTime},
};
pub fn filetime() -> u64 {
    unsafe {
        let mut value: FILETIME = std::mem::zeroed();
        GetSystemTimeAsFileTime(&mut value);
        ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
    }
}

pub fn boot_id() -> std::io::Result<String> {
    #[repr(C)]
    struct TimeOfDay {
        boot: i64,
        current: i64,
        bias: i64,
        zone: u32,
        reserved: u32,
        boot_bias: u64,
        sleep_bias: u64,
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtQuerySystemInformation(
            class: u32,
            data: *mut std::ffi::c_void,
            size: u32,
            returned: *mut u32,
        ) -> i32;
    }
    let mut value: TimeOfDay = unsafe { std::mem::zeroed() };
    let result = unsafe {
        NtQuerySystemInformation(
            3,
            (&mut value as *mut TimeOfDay).cast(),
            std::mem::size_of::<TimeOfDay>() as u32,
            std::ptr::null_mut(),
        )
    };
    if result < 0 || value.boot <= 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Windows boot time query failed: {result}"),
        ));
    }
    Ok(value.boot.to_string())
}
pub fn iso_filetime(value: u64) -> String {
    unsafe {
        let ft = FILETIME {
            dwLowDateTime: value as u32,
            dwHighDateTime: (value >> 32) as u32,
        };
        let mut st: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut st) == 0 {
            return String::new();
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:07}Z",
            st.wYear,
            st.wMonth,
            st.wDay,
            st.wHour,
            st.wMinute,
            st.wSecond,
            value % 10_000_000
        )
    }
}

#[cfg(test)]
mod tests {
    use super::iso_filetime;

    #[test]
    fn formats_the_captured_boot_marker_at_filetime_precision() {
        assert_eq!(
            iso_filetime(134393633912317080),
            "2026-11-17T04:29:51.2317080Z"
        );
        assert_eq!(
            iso_filetime(134393633912317081),
            "2026-11-17T04:29:51.2317081Z"
        );
        assert_eq!(
            iso_filetime(134393633920000000),
            "2026-11-17T04:29:52.0000000Z"
        );
    }
}
