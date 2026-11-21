use std::{fs::File, io, path::Path};

#[cfg(unix)]
pub(super) fn unit(root: &Path) -> io::Result<u64> {
    use std::os::fd::AsRawFd;
    let dir = File::open(root)?;
    let mut s = std::mem::MaybeUninit::uninit();
    if unsafe { libc::fstatvfs(dir.as_raw_fd(), s.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { s.assume_init() }.f_frsize as u64)
}

#[cfg(windows)]
pub(super) fn unit(root: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceW, GetVolumePathNameW};
    let path: Vec<u16> = root
        .canonicalize()?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut volume = vec![0u16; 32768];
    let (mut sectors, mut bytes) = (0, 0);
    if unsafe { GetVolumePathNameW(path.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) } == 0
        || unsafe {
            GetDiskFreeSpaceW(
                volume.as_ptr(),
                &mut sectors,
                &mut bytes,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(sectors) * u64::from(bytes))
}

pub(super) fn rounded(unit: u64, size: u64) -> u64 {
    size.div_ceil(unit).saturating_mul(unit)
}

pub(super) fn charge(unit: u64, file: &File) -> io::Result<u64> {
    let meta = file.metadata()?;
    #[cfg(test)]
    if unit == 1 {
        return Ok(meta.len());
    }
    #[cfg(unix)]
    let allocated = {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    };
    #[cfg(windows)]
    let allocated = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::*;
        let mut info = FILE_STANDARD_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileStandardInfo,
                (&mut info as *mut FILE_STANDARD_INFO).cast(),
                size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        u64::try_from(info.AllocationSize).map_err(|_| super::invalid("invalid file allocation"))?
    };
    Ok(allocated.max(rounded(unit, meta.len())))
}

// Specify NTFS allocation before extending EOF; a grouped write uses this extent.
pub(super) fn prepare(_file: &File, _unit: u64, _size: u64) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::*;
        if _unit > 1 {
            let extent = FILE_ALLOCATION_INFO {
                AllocationSize: rounded(_unit, _size) as i64,
            };
            if unsafe {
                SetFileInformationByHandle(
                    _file.as_raw_handle(),
                    FileAllocationInfo,
                    (&extent as *const FILE_ALLOCATION_INFO).cast(),
                    size_of_val(&extent) as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}
