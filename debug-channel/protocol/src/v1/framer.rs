use super::RpcError;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// An accepted frame retains the bytes received before the line feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonLine(Vec<u8>);
impl JsonLine {
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.0).expect("validated UTF-8 frame")
    }
}

#[derive(Default, Debug)]
pub struct JsonlFramer {
    bytes: Vec<u8>,
    dropping: bool,
}
impl JsonlFramer {
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.dropping = false;
    }
    pub fn buffered_len(&self) -> usize {
        self.bytes.len()
    }
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Result<JsonLine, RpcError>> {
        let mut frames = Vec::new();
        for &byte in chunk {
            if self.dropping {
                if byte == b'\n' {
                    self.dropping = false;
                }
                continue;
            }
            if byte == b'\n' {
                let bytes = std::mem::take(&mut self.bytes);
                match std::str::from_utf8(&bytes) {
                    Err(_) => frames.push(Err(RpcError::new(-32700, "Parse error"))),
                    Ok(_)
                        if bytes
                            .iter()
                            .all(|byte| matches!(byte, b' ' | b'\t' | b'\r')) =>
                    {
                        ()
                    }
                    Ok(_) => frames.push(Ok(JsonLine(bytes))),
                }
            } else if self.bytes.len() == MAX_FRAME_BYTES {
                self.bytes.clear();
                self.dropping = true;
                frames.push(Err(RpcError::new(-32700, "Frame exceeds 1 MiB")));
            } else {
                self.bytes.push(byte);
            }
        }
        frames
    }
}
