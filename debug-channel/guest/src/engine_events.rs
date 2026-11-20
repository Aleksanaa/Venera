use crate::user_events::UserEvent;
use std::{borrow::Cow, collections::BTreeMap, io};

mod fragments;
pub use fragments::{GraphicsUpdate, MAX_GRAPHICS_BYTES};

const CHUNK: usize = 60 * 1024;
const MAX_TEXT: usize = 16 * 1024 * 1024;

type Key = (u64, u32, u64, u64); // boot, PID, producer instance, sequence
#[derive(Default)]
pub struct Decoder {
    pending: BTreeMap<Key, Message>,
    evicted: u64,
}
struct Message {
    header: Header,
    parts: BTreeMap<usize, Vec<u8>>,
    bytes: usize,
}
#[derive(PartialEq)]
struct Header {
    id: u16,
    tid: u32,
    created: u64,
    time: u64,
    count: usize,
    raw_level: String,
    channel: String,
}

impl Decoder {
    pub fn incomplete(&self) -> u64 {
        self.evicted + self.pending.len() as u64
    }

    pub fn decode(
        &mut self,
        bytes: &[u8],
        boot: u64,
        id: u16,
        version: u8,
        level: u8,
        pid: u32,
        tid: u32,
    ) -> io::Result<Option<UserEvent<'static>>> {
        let invalid =
            || io::Error::new(io::ErrorKind::InvalidData, "Invalid graphics text fragment");
        if bytes.len() < 42 || version != 1 || level != 4 || pid == 0 {
            return Err(invalid());
        }
        let word = |i| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let (time, created, instance, sequence) = (word(0), word(8), word(16), word(24));
        let index = u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize;
        let count = u32::from_le_bytes(bytes[36..40].try_into().unwrap()) as usize;
        let mut fields = bytes[40..].splitn(3, |b| *b == 0);
        let raw_level = std::str::from_utf8(fields.next().unwrap()).map_err(|_| invalid())?;
        let channel =
            std::str::from_utf8(fields.next().ok_or_else(invalid)?).map_err(|_| invalid())?;
        let text = fields.next().ok_or_else(invalid)?;
        if time == 0
            || created == 0
            || instance == 0
            || sequence == 0
            || index >= count
            || count > MAX_TEXT.div_ceil(CHUNK)
            || text.len() > CHUNK
            || (index + 1 < count && text.len() != CHUNK)
            || (count > 1 && text.is_empty())
            || index * CHUNK + text.len() > MAX_TEXT
            || channel.len() > 1024
            || raw_level.len() > 32
        {
            return Err(invalid());
        }
        let component = match id {
            7 if channel == "default" => "dxvk",
            8 if matches!(channel, "api" | "shader") => "vkd3d-proton",
            9 if channel == "default" => "mesa",
            10 if channel == "MESA-VIRTIO" => "venus",
            10 => "mesa",
            _ => return Err(invalid()),
        };
        let key = (boot, pid, instance, sequence);
        let body = if count == 1 {
            if self.pending.remove(&key).is_some() {
                return Err(invalid());
            }
            text.to_vec()
        } else {
            let header = Header {
                id,
                tid,
                created,
                time,
                count,
                raw_level: raw_level.into(),
                channel: channel.into(),
            };
            if let Some(previous) = self.pending.get(&key) {
                match (previous.header == header, previous.parts.get(&index)) {
                    (true, Some(part)) if part == text => return Ok(None),
                    (true, None) => (),
                    _ => {
                        self.pending.remove(&key);
                        return Err(invalid());
                    }
                }
            }
            // Bound retained data even when a trace repeatedly loses its last fragment.
            while (self.pending.len() >= 64 && !self.pending.contains_key(&key))
                || self.pending.values().map(|m| m.bytes).sum::<usize>() + text.len() > 2 * MAX_TEXT
            {
                let oldest = *self
                    .pending
                    .iter()
                    .filter(|(k, _)| **k != key)
                    .min_by_key(|(_, m)| m.header.time)
                    .unwrap()
                    .0;
                self.pending.remove(&oldest);
                self.evicted += 1;
            }
            let m = self.pending.entry(key).or_insert_with(|| Message {
                header,
                parts: BTreeMap::new(),
                bytes: 0,
            });
            m.bytes += text.len();
            m.parts.insert(index, text.to_vec());
            if m.parts.len() != count {
                return Ok(None);
            }
            let m = self.pending.remove(&key).unwrap();
            let mut body = Vec::with_capacity(m.bytes);
            for part in m.parts.into_values() {
                body.extend(part);
            }
            body
        };
        Ok(Some(UserEvent {
            event_id: id,
            level,
            source_time: time,
            created,
            instance,
            sequence,
            component,
            message: Cow::Owned(body),
            engine: Some((raw_level.into(), channel.into())),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn payload(
        instance: u64,
        index: u32,
        count: u32,
        level_channel: &[u8],
        body: &[u8],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [123u64, 10, instance, 7] {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(index.to_le_bytes());
        bytes.extend(count.to_le_bytes());
        bytes.extend(level_channel);
        bytes.extend(body);
        bytes
    }
    #[test]
    fn emitting_site_identifies_identical_text() {
        let mut d = Decoder::default();
        for (id, level, channel, expected) in [
            (7, "warn", "default", "dxvk"),
            (8, "fixme", "api", "vkd3d-proton"),
            (8, "trace", "shader", "vkd3d-proton"),
            (9, "unknown", "default", "mesa"),
            (10, "debug", "MESA-VIRTIO", "venus"),
            (10, "warning", "app", "mesa"),
        ] {
            let p = payload(
                5,
                0,
                1,
                format!("{level}\0{channel}\0").as_bytes(),
                b"same\0text\n",
            );
            let e = d.decode(&p, 1, id, 1, 4, 42, 43).unwrap().unwrap();
            assert_eq!(
                (e.component, e.level_name(), e.message.as_ref()),
                (expected, level, &b"same\0text\n"[..])
            );
            assert_eq!(
                e.structured(1, 42).fields["channel"],
                venera_debug_protocol::v1::EventValue::AnsiString(channel.into())
            );
            assert!(d.decode(&p, 1, id, 2, 4, 42, 43).is_err());
        }
    }
    #[test]
    fn interleaved_shader_messages_complete_under_their_own_instance() {
        let mut d = Decoder::default();
        for (instance, line) in [
            (10, b"mov r0, r1\n".as_slice()),
            (20, b"add r1, r2\n"),
            (30, b"incomplete\n"),
        ] {
            let p = payload(
                instance,
                0,
                2,
                b"trace\0shader\0",
                &line.iter().copied().cycle().take(61440).collect::<Vec<_>>(),
            );
            assert!(d.decode(&p, 1, 8, 1, 4, 42, 1).unwrap().is_none());
        }
        for (instance, line) in [(20, b"add r1, r2\n".as_slice()), (10, b"mov r0, r1\n")] {
            let p = payload(
                instance,
                1,
                2,
                b"trace\0shader\0",
                &b"shader end\n".repeat(1000),
            );
            let e = d.decode(&p, 1, 8, 1, 4, 42, 1).unwrap().unwrap();
            let expected: Vec<u8> = line
                .iter()
                .copied()
                .cycle()
                .take(61440)
                .chain(b"shader end\n".repeat(1000))
                .collect();
            assert_eq!(e.message.as_ref(), expected);
        }
        assert_eq!(d.incomplete(), 1);
    }

    #[test]
    fn adjacent_sources_replay_full_dxvk_text_by_boot_and_producer() {
        let make = |instance, index, byte| {
            let len = if index == 0 { 61_440 } else { 26_560 };
            payload(instance, index, 2, b"warn\0default\0", &vec![byte; len])
        };
        let first_source = [make(5, 0, b'a'), make(6, 0, b'c'), make(7, 0, b'e')];
        let next_source = [make(6, 1, b'd'), make(5, 1, b'b')];
        let next_boot = make(7, 1, b'f');
        // Each replay starts from the same immutable sources, as when rebuilding context.
        for _ in 0..2 {
            let mut decoder = Decoder::default();
            for fragment in &first_source {
                assert!(decoder
                    .decode(fragment, 100, 7, 1, 4, 42, 43)
                    .unwrap()
                    .is_none());
            }
            assert!(decoder
                .decode(&first_source[0], 100, 7, 1, 4, 42, 43)
                .unwrap()
                .is_none());
            for (fragment, first, last) in
                [(&next_source[0], b'c', b'd'), (&next_source[1], b'a', b'b')]
            {
                let event = decoder
                    .decode(fragment, 100, 7, 1, 4, 42, 43)
                    .unwrap()
                    .unwrap();
                assert_eq!(event.message.len(), 88_000);
                assert!(event.message[..61_440].iter().all(|b| *b == first));
                assert!(event.message[61_440..].iter().all(|b| *b == last));
            }
            assert_eq!(decoder.incomplete(), 1);
            assert!(decoder
                .decode(&next_boot, 200, 7, 1, 4, 42, 43)
                .unwrap()
                .is_none());
            let event = decoder
                .decode(&make(7, 0, b'g'), 200, 7, 1, 4, 42, 43)
                .unwrap()
                .unwrap();
            assert!(event.message[..61_440].iter().all(|b| *b == b'g'));
            assert!(event.message[61_440..].iter().all(|b| *b == b'f'));
            assert_eq!(decoder.incomplete(), 1);
        }
    }
}
