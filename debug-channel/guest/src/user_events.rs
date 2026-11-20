use std::{collections::BTreeMap, io};
use venera_debug_protocol::v1::{DecimalU64, EventValue, StructuredEvent};

pub struct UserEvent<'a> {
    pub event_id: u16,
    pub level: u8,
    pub source_time: u64,
    pub created: u64,
    pub sequence: u64,
    pub component: &'static str,
    pub message: &'a [u8],
}

pub fn decode(bytes: &[u8], event_id: u16, version: u8, level: u8) -> io::Result<UserEvent<'_>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid Venera user-mode event");
    let component = match event_id {
        5 => "venera_d3d11",
        6 => "venera_d3d12",
        _ => return Err(invalid()),
    };
    if version != 1 || !(2..=5).contains(&level) || bytes.len() < 26 || bytes.len() > 4096 {
        return Err(invalid());
    }
    let component_end = 24
        + bytes[24..]
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(invalid)?;
    if &bytes[24..component_end] != component.as_bytes() {
        return Err(invalid());
    }
    let message = &bytes[component_end + 1..];
    if message.last() != Some(&0)
        || message.len() > 1024
        || message[..message.len() - 1].contains(&0)
    {
        return Err(invalid());
    }
    let source_time = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let created = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let sequence = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    if source_time == 0 || created == 0 || sequence == 0 {
        return Err(invalid());
    }
    Ok(UserEvent {
        event_id,
        level,
        source_time,
        created,
        sequence,
        component,
        message: &message[..message.len() - 1],
    })
}

impl UserEvent<'_> {
    pub fn level_name(&self) -> &'static str {
        match self.level {
            2 => "error",
            3 => "warning",
            4 => "info",
            5 => "debug",
            _ => "unknown",
        }
    }

    pub fn structured(&self, boot: u64, pid: u32) -> StructuredEvent {
        StructuredEvent {
            provider: crate::kernel_events::PROVIDER.into(),
            event_id: self.event_id,
            version: 1,
            boot_time_filetime: DecimalU64(boot),
            producer_instance: DecimalU64(self.created),
            producer_sequence: DecimalU64(self.sequence),
            source_time_filetime: DecimalU64(self.source_time),
            fields: BTreeMap::from([
                ("level".into(), EventValue::Uint8(self.level)),
                (
                    "component".into(),
                    EventValue::AnsiString(self.component.into()),
                ),
                ("process_id".into(), EventValue::Uint32(pid)),
                (
                    "process_create_time_filetime".into(),
                    EventValue::Uint64(DecimalU64(self.created)),
                ),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_payload_validates_complete_strings_descriptor_and_identity() {
        let bytes =
            b"\x03\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x02\0\0\0\0\0\0\0venera_d3d11\0same message\0";
        for length in 0..bytes.len() {
            assert!(decode(&bytes[..length], 5, 1, 4).is_err());
        }
        for (level, name) in [(2, "error"), (3, "warning"), (4, "info"), (5, "debug")] {
            let event = decode(bytes, 5, 1, level).unwrap();
            assert_eq!(event.level_name(), name);
            assert_eq!(event.message, b"same message");
            assert_eq!(
                (event.source_time, event.created, event.sequence),
                (3, 1, 2)
            );
        }
        assert!(decode(bytes, 6, 1, 4).is_err());
        assert!(decode(bytes, 5, 2, 4).is_err());
        assert!(decode(bytes, 5, 1, 1).is_err());
        let mut invalid = bytes.to_vec();
        invalid[8] = 0;
        assert!(decode(&invalid, 5, 1, 4).is_err());
    }
}
