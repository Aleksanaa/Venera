use std::{
    io,
    time::{Duration, Instant},
};

pub(super) enum WriteStep {
    Blocked,
    Progress,
    Complete,
}

pub(super) struct PendingWrite {
    bytes: Vec<u8>,
    offset: usize,
    last_progress: Instant,
}

pub(super) fn send_error(code: i32) -> io::Error {
    // viosock's TxEnqueue returns STATUS_BUFFER_TOO_SMALL when peer credit is
    // exhausted; its WSP passes that NTSTATUS through as the socket error.
    const STATUS_BUFFER_TOO_SMALL: i32 = 0xc0000023u32 as i32;
    const WSAEWOULDBLOCK: i32 = 10035;
    if matches!(code, WSAEWOULDBLOCK | STATUS_BUFFER_TOO_SMALL) {
        io::ErrorKind::WouldBlock.into()
    } else {
        io::Error::from_raw_os_error(code)
    }
}

impl PendingWrite {
    pub(super) fn new(bytes: Vec<u8>, now: Instant) -> Self {
        Self {
            bytes,
            offset: 0,
            last_progress: now,
        }
    }

    pub(super) fn write(
        &mut self,
        now: Instant,
        allowed: bool,
        mut send: impl FnMut(&[u8]) -> io::Result<usize>,
    ) -> io::Result<WriteStep> {
        if !allowed {
            return Err(io::ErrorKind::Interrupted.into());
        }
        match send(&self.bytes[self.offset..]) {
            Ok(0) => Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => {
                self.offset += count;
                self.last_progress = now;
                Ok(if self.offset == self.bytes.len() {
                    WriteStep::Complete
                } else {
                    WriteStep::Progress
                })
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if now.duration_since(self.last_progress) >= Duration::from_secs(2) {
                    Err(io::ErrorKind::TimedOut.into())
                } else {
                    Ok(WriteStep::Blocked)
                }
            }
            Err(error) => Err(error),
        }
    }
}
