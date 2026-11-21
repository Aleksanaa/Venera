use std::{collections::BTreeMap, io};
use venera_debug_protocol::v1::{DecimalU64, EventValue, StructuredEvent};

pub const PROVIDER: &str = "6c5a2378-22b9-4c10-a91f-69e34c280b81";
pub const RECORDER_TMF: &str = include_str!("../../../kmd/logging/generated/venera-recorder.tmf");

#[derive(Debug, PartialEq)]
pub struct KernelEvent<'a> {
    pub instance: u64,
    pub sequence: u64,
    pub source_time: u64,
    pub event_id: u16,
    pub version: u8,
    pub level: u8,
    pub line: u32,
    pub status: i32,
    pub function: &'a [u8],
    pub message: &'a [u8],
}

pub fn decode(bytes: &[u8]) -> io::Result<KernelEvent<'_>> {
    decode_payload(bytes, false)
}

pub fn decode_recorder(bytes: &[u8]) -> io::Result<KernelEvent<'_>> {
    decode_payload(bytes, true)
}

fn decode_payload(bytes: &[u8], recorder: bool) -> io::Result<KernelEvent<'_>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid Venera kernel event");
    if bytes.len() < 38 || bytes.len() > 4096 {
        return Err(invalid());
    }
    let function_end = bytes[36..]
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(invalid)?
        + 36;
    let message = &bytes[function_end + 1..];
    let message_end = message.iter().position(|byte| *byte == 0).ok_or_else(invalid)?;
    let end = function_end + 1 + message_end + 1;
    // rcdrlogsave includes the DWORD alignment bytes in UserDataLength.
    if bytes.len() != end && !(recorder && bytes.len() == end.next_multiple_of(4)) {
        return Err(invalid());
    }
    let event = KernelEvent {
        instance: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
        sequence: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
        source_time: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
        event_id: u16::from_le_bytes(bytes[24..26].try_into().unwrap()),
        version: bytes[26],
        level: bytes[27],
        line: u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
        status: i32::from_le_bytes(bytes[32..36].try_into().unwrap()),
        function: &bytes[36..function_end],
        message: &message[..message_end],
    };
    if event.version != 1
        || !(1..=4).contains(&event.event_id)
        || event.level > 5
        || event.sequence == 0
        || event.source_time == 0
    {
        return Err(invalid());
    }
    Ok(event)
}

impl KernelEvent<'_> {
    pub fn structured(&self, boot: u64, function: String) -> StructuredEvent {
        StructuredEvent {
            provider: PROVIDER.into(),
            event_id: self.event_id,
            version: self.version,
            boot_time_filetime: DecimalU64(boot),
            producer_instance: DecimalU64(self.instance),
            producer_sequence: DecimalU64(self.sequence),
            source_time_filetime: DecimalU64(self.source_time),
            fields: BTreeMap::from([
                ("level".into(), EventValue::Uint8(self.level)),
                ("line".into(), EventValue::Uint32(self.line)),
                ("status".into(), EventValue::Int32(self.status)),
                ("function".into(), EventValue::AnsiString(function)),
            ]),
        }
    }

    pub fn level_name(&self) -> &'static str {
        match self.level {
            1 => "fatal",
            2 => "error",
            3 => "warning",
            4 => "info",
            5 => "trace",
            _ => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Wire layout: instance, sequence, FILETIME, descriptor, line, NTSTATUS, strings.
    const EVENT: &[u8] = b"\x01\0\0\0\0\0\0\0\x02\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\x03\0\x01\x02\x29\0\0\0\x01\0\0\xc0DxgkInitialize\0Registration failed\0";

    #[test]
    fn recorder_driver_entry_preserves_business_payload() {
        // rcdrlogsave: first WPP record, UserDataLength = 80.
        let bytes = b"\x26\x51\x5a\x02\0\0\0\0\x01\0\0\0\0\0\0\0\xbe\x96\xa4\x9a\xe4\x77\xdd\x01\x02\0\x01\x04\0\0\0\0\0\0\0\0DriverEntry\0Venera kernel initialization\0\0\0\0";
        assert!(decode(bytes).is_err());
        let event = decode_recorder(bytes).unwrap();
        assert_eq!((event.instance, event.sequence, event.source_time), (39473446, 1, 134395387621709502));
        assert_eq!((event.event_id, event.version, event.level, event.line, event.status), (2, 1, 4, 0, 0));
        assert_eq!(event.function, b"DriverEntry");
        assert_eq!(event.message, b"Venera kernel initialization");
        assert_eq!(decode_recorder(&bytes[..77]).unwrap(), event);
        for size in [36, 47, 76, 78, 79] {
            assert!(decode_recorder(&bytes[..size]).is_err());
        }
        assert!(decode_recorder(&[bytes.as_slice(), &[0; 4]].concat()).is_err());
        let mut bad = *bytes;
        bad[26] = 2;
        assert!(decode_recorder(&bad).is_err());
    }

    #[test]
    fn recorder_padding_preserves_message_for_each_alignment() {
        for (message, pad) in [(b"a".as_slice(), b"\xd1\xaa".as_slice()), (b"ab", b"\xaa"), (b"abc", b""), (b"abcd", b"\x13\xd1\xaa")] {
            let bytes = [&EVENT[..36], b"fn!\0", message, b"\0", pad].concat();
            assert_eq!(decode_recorder(&bytes).unwrap().message, message);
            assert_eq!(decode(&bytes).is_ok(), pad.is_empty());
        }
    }

    #[test]
    fn kernel_failure_preserves_identity_and_typed_status() {
        let event = decode(EVENT).unwrap();
        assert_eq!(event.message, b"Registration failed");
        assert_eq!(event.level_name(), "error");
        assert_eq!(
            serde_json::to_value(event.structured(100, "DxgkInitialize".into())).unwrap(),
            json!({
                "provider":"6c5a2378-22b9-4c10-a91f-69e34c280b81", "event_id":3,"version":1,
                "boot_time_filetime":"100", "producer_instance":"1", "producer_sequence":"2", "source_time_filetime":"3",
                "fields":{"level":{"type":"uint8","value":2},"line":{"type":"uint32","value":41},
                    "status":{"type":"int32","value":-1073741823},"function":{"type":"ansi_string","value":"DxgkInitialize"}}
            })
        );
    }

    #[test]
    fn complete_payload_and_supported_version_are_required() {
        for size in 0..EVENT.len() {
            assert!(decode(&EVENT[..size]).is_err(), "prefix {size}");
        }
        let mut bytes = EVENT.to_vec();
        bytes[26] = 2;
        assert!(decode(&bytes).is_err());
        bytes[26] = 1;
        bytes[8] = 3;
        assert_eq!(decode(&bytes).unwrap().sequence, 3);
        assert_eq!(decode(EVENT).unwrap().sequence, 2);
    }
}
