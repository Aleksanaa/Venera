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
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
        )
    }
}
