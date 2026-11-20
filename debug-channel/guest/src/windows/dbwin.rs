use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_ACP};

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
