use super::{Decoder, CHUNK, MAX_TEXT};
use std::io;

// Raw text plus bounded per-message and per-fragment framing.
pub const MAX_GRAPHICS_BYTES: usize = 2 * MAX_TEXT + 1024 * 1024;

pub struct GraphicsUpdate {
    pub boot: u64,
    pub bytes: Vec<u8>,
}

impl Decoder {
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    pub fn checkpoint(&self, boot: u64) -> GraphicsUpdate {
        let mut bytes = Vec::new();
        for (&(key_boot, pid, instance, sequence), m) in &self.pending {
            debug_assert_eq!(key_boot, boot);
            let h = &m.header;
            for (&index, part) in &m.parts {
                let payload = 42 + h.raw_level.len() + h.channel.len() + part.len();
                bytes.extend((payload as u16).to_le_bytes());
                bytes.extend(h.id.to_le_bytes());
                bytes.extend(pid.to_le_bytes());
                bytes.extend(h.tid.to_le_bytes());
                for value in [h.time, h.created, instance, sequence] {
                    bytes.extend(value.to_le_bytes());
                }
                bytes.extend((index as u32).to_le_bytes());
                bytes.extend((h.count as u32).to_le_bytes());
                bytes.extend(h.raw_level.as_bytes());
                bytes.push(0);
                bytes.extend(h.channel.as_bytes());
                bytes.push(0);
                bytes.extend(part);
            }
        }
        GraphicsUpdate { boot, bytes }
    }

    pub fn restore(boot: u64, mut bytes: &[u8]) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid graphics checkpoint");
        if bytes.len() > MAX_GRAPHICS_BYTES {
            return Err(invalid());
        }
        let mut decoder = Self::default();
        while !bytes.is_empty() {
            let (header, next) = bytes.split_at_checked(12).ok_or_else(invalid)?;
            let length = u16::from_le_bytes(header[0..2].try_into().unwrap()) as usize;
            let id = u16::from_le_bytes(header[2..4].try_into().unwrap());
            let pid = u32::from_le_bytes(header[4..8].try_into().unwrap());
            let tid = u32::from_le_bytes(header[8..12].try_into().unwrap());
            let (payload, next) = next.split_at_checked(length).ok_or_else(invalid)?;
            if length > CHUNK + 42 + 32 + 1024
                || decoder.decode(payload, boot, id, 1, 4, pid, tid)?.is_some()
                || decoder.evicted != 0
            {
                return Err(invalid());
            }
            bytes = next;
        }
        Ok(decoder)
    }
}
