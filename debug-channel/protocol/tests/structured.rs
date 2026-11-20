use serde_json::json;
use venera_debug_protocol::v1::{EventValue, LogRecord};

#[test]
fn canonical_record_round_trip_retains_typed_event_and_full_width_identity() {
    let value = json!({
        "stream_id":"service-1", "seq":"9", "observed_at":"2026-11-17T04:30:00.0000000Z",
        "source_time":"2026-11-17T04:29:51.2317080Z", "source":"wpp_recorder", "component":"kernel",
        "level":"error", "raw_level":"error", "session_id":null, "process":null, "tid":42, "text":"registration",
        "structured": {
            "provider":"6c5a2378-22b9-4c10-a91f-69e34c280b81", "event_id":3, "version":1,
            "boot_time_filetime":"134393633875000000", "producer_instance":"12345678", "producer_sequence":"18446744073709551615",
            "source_time_filetime":"134393633912317080",
            "fields":{"status":{"type":"int32","value":-1073741823}, "address":{"type":"uint64","value":"18446744073709551615"}}
        }
    });
    let record: LogRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        record.structured.as_ref().unwrap().fields["status"],
        EventValue::Int32(-1073741823)
    );
    assert_eq!(serde_json::to_value(record).unwrap(), value);
    let mut invalid = value;
    invalid["structured"]["fields"]["status"]["value"] = json!(2147483648u64);
    assert!(serde_json::from_value::<LogRecord>(invalid).is_err());
}
