use std::{
    fs::File,
    io::{self, Read, Write},
    mem::{size_of, zeroed},
    os::fd::{AsRawFd, FromRawFd},
    time::{Duration, Instant},
};

pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

pub trait Connector: Send + 'static {
    fn connect(&mut self, timeout: Duration) -> io::Result<Box<dyn Stream>>;
}

pub struct VsockConnector {
    address: libc::sockaddr_vm,
}

impl VsockConnector {
    pub fn new(cid: u32, port: u32) -> io::Result<Self> {
        if cid <= 2 || cid == u32::MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Guest CID must be between 3 and 4294967294",
            ));
        }
        if port == 0 || port == u32::MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Port must be between 1 and 4294967294",
            ));
        }
        // Linux sockaddr_vm uses native-endian CID and port fields.
        let mut address: libc::sockaddr_vm = unsafe { zeroed() };
        address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        address.svm_cid = cid;
        address.svm_port = port;
        Ok(Self { address })
    }
}

impl Connector for VsockConnector {
    fn connect(&mut self, timeout: Duration) -> io::Result<Box<dyn Stream>> {
        let deadline = Instant::now() + timeout;
        let fd = unsafe {
            libc::socket(
                libc::AF_VSOCK,
                libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // File owns the socket from this point, including every error path.
        let socket = unsafe { File::from_raw_fd(fd) };
        let result = unsafe {
            libc::connect(
                socket.as_raw_fd(),
                &self.address as *const _ as *const libc::sockaddr,
                size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(error);
            }
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Guest connection timed out",
                    ));
                }
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let result = unsafe {
                    libc::poll(
                        &mut poll,
                        1,
                        remaining.as_millis().max(1).min(i32::MAX as u128) as i32,
                    )
                };
                if result < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if result == 0 {
                    continue;
                }
                let mut error: libc::c_int = 0;
                let mut len = size_of::<libc::c_int>() as libc::socklen_t;
                if unsafe {
                    libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        &mut error as *mut _ as *mut libc::c_void,
                        &mut len,
                    )
                } < 0
                {
                    return Err(io::Error::last_os_error());
                }
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error));
                }
                break;
            }
        }
        Ok(Box::new(socket))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_guest_address_has_native_fields_and_zero_padding() {
        let connector = VsockConnector::new(42, 49152).unwrap();
        let address = connector.address;
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &address as *const _ as *const u8,
                size_of::<libc::sockaddr_vm>(),
            )
        };
        assert_eq!(bytes.len(), 16);
        assert_eq!(address.svm_family, 40);
        assert_eq!(address.svm_cid, 42);
        assert_eq!(address.svm_port, 49152);
        assert_eq!(u32::from_ne_bytes(bytes[4..8].try_into().unwrap()), 49152);
        assert_eq!(u32::from_ne_bytes(bytes[8..12].try_into().unwrap()), 42);
        assert_eq!(&bytes[2..4], &[0, 0]);
        assert_eq!(&bytes[12..16], &[0, 0, 0, 0]);
    }

    #[test]
    fn reserved_guest_addresses_report_invalid_input() {
        for (cid, port) in [
            (0, 49152),
            (1, 49152),
            (2, 49152),
            (u32::MAX, 49152),
            (3, 0),
            (3, u32::MAX),
        ] {
            assert_eq!(
                VsockConnector::new(cid, port).err().unwrap().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        let address = VsockConnector::new(4294967294, 4294967294).unwrap().address;
        assert_eq!(address.svm_cid, 4294967294);
        assert_eq!(address.svm_port, 4294967294);
    }
}
