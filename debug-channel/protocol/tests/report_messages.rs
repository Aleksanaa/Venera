use serde_json::json;
use venera_debug_protocol::{v1, SystemInfoSection};

#[test]
fn report_messages_round_trip_through_both_contracts() {
    let wire = json!({
        "title": "Guest", "title_message": {"id":"report.guest"},
        "rows": [{"key":"Memory", "value":"8 GB (7 GB usable)", "bad":false,
            "warning":"原始诊断\nC:\\GPU",
            "key_message":{"id":"report.memory","args":[]},
            "value_message":{"id":"report.memoryValue","args":["8 GB","7 GB"],"diagnostic":" 🚀"},
            "warning_message":{"id":"future.warning","args":[]}}]
    });
    let legacy: SystemInfoSection = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(legacy.title_message.unwrap().id, "report.guest");
    assert_eq!(legacy.rows[0].warning, "原始诊断\nC:\\GPU");
    assert_eq!(
        legacy.rows[0].value_message.as_ref().unwrap().args,
        ["8 GB", "7 GB"]
    );
    let current: v1::SystemInfoSection = serde_json::from_value(wire).unwrap();
    let encoded = serde_json::to_value(current).unwrap();
    assert_eq!(
        encoded["title_message"],
        json!({"id":"report.guest","args":[]})
    );
    assert_eq!(
        encoded["rows"][0]["value_message"],
        json!({"id":"report.memoryValue","args":["8 GB","7 GB"],"diagnostic":" 🚀"})
    );
    let decoded: SystemInfoSection = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.rows[0].value, "8 GB (7 GB usable)");
}

#[test]
fn report_from_earlier_producer_keeps_its_fields() {
    let wire = json!({"title":"系统 🚀","rows":[{"key":"GPU","value":"设备\nC:\\GPU","bad":false,"warning":""}]});
    let legacy: SystemInfoSection = serde_json::from_value(wire.clone()).unwrap();
    let current: v1::SystemInfoSection = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(legacy).unwrap(), wire);
    assert_eq!(serde_json::to_value(current).unwrap(), wire);
}
