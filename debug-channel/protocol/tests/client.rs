use serde_json::{json, Value};
use venera_debug_protocol::{
    client::{Client, HistoryConsumer, Start},
    v1::*,
};

fn write(client: &mut Client) -> Vec<Value> {
    let mut bytes = Vec::new();
    client.write(&mut bytes).unwrap();
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn respond(
    client: &mut Client,
    request: &Value,
    value: Value,
) -> Vec<venera_debug_protocol::client::Reply> {
    client
        .receive(&venera_debug_protocol::client::encode(
            &json!({"jsonrpc":"2.0","id":request["id"],"result":value}),
        ))
        .unwrap()
}

fn ready(client: &mut Client) {
    client.connect();
    let requests = write(client);
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["endpoint.info", "rpc.discover"]
    );
    respond(
        client,
        &requests[1],
        json!({"openrpc":"1.4.1","info":{"title":"Guest","version":"1.0.0"},"methods":[
        {"name":"endpoint.info","paramStructure":"by-name","params":[]},{"name":"rpc.discover","paramStructure":"by-name","params":[]},
        {"name":"history.range","paramStructure":"by-name","params":[]},{"name":"history.read","paramStructure":"by-name","params":[]},{"name":"history.ack","paramStructure":"by-name","params":[]},
        {"name":"status.get","paramStructure":"by-name","params":[]}
    ],"components":{"schemas":{}}}),
    );
    respond(
        client,
        &requests[0],
        json!({"role":"guest","protocol_version":"1.0.0","software_version":"fixture"}),
    );
}

#[test]
fn rpc_matches_out_of_order_results_and_negotiates_each_connection() {
    let mut client = Client::default();
    ready(&mut client);
    let range_id = client.request(Method::HistoryRange, json!({})).unwrap();
    let status_id = client.request(Method::StatusGet, json!({})).unwrap();
    let requests = write(&mut client);
    let reply = respond(
        &mut client,
        &requests[1],
        json!({"forwarding_enabled":true,"revision":"7","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}),
    );
    assert_eq!(
        (
            &reply[0].id,
            reply[0].method,
            reply[0].result.as_ref().unwrap()["revision"].clone()
        ),
        (&status_id, Method::StatusGet, json!("7"))
    );
    let reply = respond(
        &mut client,
        &requests[0],
        json!({"journal_id":"j","oldest":"1","high_water":"12"}),
    );
    assert_eq!(
        (
            &reply[0].id,
            reply[0].method,
            reply[0].result.as_ref().unwrap()["high_water"].clone()
        ),
        (&range_id, Method::HistoryRange, json!("12"))
    );
    client.disconnect();
    assert_eq!(
        client
            .request(Method::HistoryRange, json!({}))
            .unwrap_err()
            .message,
        "not_ready"
    );
    ready(&mut client);
    assert_eq!(
        client.peer().endpoint().unwrap().software_version,
        "fixture"
    );
}

fn range(high_water: u64) -> HistoryRange {
    HistoryRange {
        journal_id: "j".into(),
        oldest: DecimalU64(1),
        high_water: DecimalU64(high_water),
    }
}
fn page() -> HistoryPage {
    serde_json::from_value(json!({"range":{"journal_id":"j","oldest":"1","high_water":"8"},
        "records":[{"cursor":{"journal_id":"j","position":"7"},"boot_id":"boot","recorded_at_ms":"100","startup":false,
        "event":{"type":"log","record":{"stream_id":"s","seq":"7","observed_at":"original observation","source_time":"original source","source":"etw","component":"kmd","level":"info","raw_level":"4","text":"seven"}}}],
        "missing":[{"first":"6","last":"6","reason":"retention_expired"},{"first":"8","last":"8","reason":"retention_expired"}],
        "next":{"journal_id":"j","position":"8"}})).unwrap()
}

#[test]
fn live_follow_starts_at_high_water_and_reconnects_from_consumed_output() {
    let mut consumer = HistoryConsumer::new(Start::Now);
    consumer.range(range(5), None).unwrap();
    assert_eq!(
        serde_json::to_value(consumer.read_request()).unwrap(),
        json!({"after":{"journal_id":"j","position":"5"},"through":"5","limit":256})
    );
    consumer.range(range(8), None).unwrap();
    consumer.page(&page()).unwrap();
    assert_eq!(consumer.cursor().unwrap().position, DecimalU64(5));
    consumer.range(range(8), None).unwrap();
    assert_eq!(
        consumer.read_request().after.unwrap().position,
        DecimalU64(5)
    );
    consumer.page(&page()).unwrap();
    assert_eq!(
        serde_json::to_value(consumer.consumed().unwrap()).unwrap(),
        json!({"cursor":{"journal_id":"j","position":"8"}})
    );
    consumer.range(range(10), None).unwrap();
    assert_eq!(
        serde_json::to_value(consumer.read_request()).unwrap(),
        json!({"after":{"journal_id":"j","position":"8"},"through":"10","limit":256})
    );
}

#[test]
fn filtered_display_consumes_every_position_and_replays_after_output_failure() {
    use std::io::{self, Write};
    let mut consumer = HistoryConsumer::new(Start::Now);
    consumer.range(range(5), None).unwrap();
    consumer.range(range(8), None).unwrap();
    let filter = venera_debug_protocol::Filter {
        components: vec!["app".into()],
        ..Default::default()
    };
    let mut output = Vec::new();
    let ack = consumer.display(&page(), &filter, &mut output).unwrap();
    assert_eq!(ack.cursor.position.0, 8);
    let displayed: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        displayed,
        vec![
            json!({"type":"gap","reason":"retention_expired","first_position":"6","last_position":"6"}),
            json!({"type":"gap","reason":"retention_expired","first_position":"8","last_position":"8"})
        ]
    );
    consumer.range(range(9), None).unwrap();
    assert_eq!(consumer.read_request().after.unwrap().position.0, 8);
    let mut next = page();
    next.range = range(9);
    next.missing.clear();
    next.records[0].cursor.position = DecimalU64(9);
    next.next.position = DecimalU64(9);
    struct FlushFailure;
    impl Write for FlushFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    assert_eq!(
        consumer
            .display(&next, &Default::default(), &mut FlushFailure)
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    consumer.range(range(9), None).unwrap();
    assert_eq!(consumer.read_request().after.unwrap().position.0, 8);
    let mut output = Vec::new();
    assert_eq!(
        consumer
            .display(&next, &Default::default(), &mut output)
            .unwrap()
            .cursor
            .position
            .0,
        9
    );
    let displayed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(displayed["text"], "seven");
    assert_eq!(
        displayed["cursor"],
        json!({"journal_id":"j","position":"9"})
    );
    consumer.range(range(10), None).unwrap();
    next.range = range(10);
    next.next.position = DecimalU64(10);
    next.records[0].cursor.position = DecimalU64(10);
    let mut output = Vec::new();
    assert_eq!(
        consumer
            .display(&next, &filter, &mut output)
            .unwrap()
            .cursor
            .position
            .0,
        10
    );
    assert_eq!(output, b"");
}

#[test]
fn display_uses_persisted_process_instances_for_descendant_selection() {
    let mut consumer = HistoryConsumer::new(Start::Now);
    consumer.range(range(5), None).unwrap();
    consumer.range(range(8), None).unwrap();
    let mut page = page();
    page.missing.truncate(1);
    let HistoryEvent::Log { record } = &mut page.records[0].event else {
        panic!("log")
    };
    record.process = Some(ProcessIdentity {
        pid: 42,
        start_time_filetime: Some("100".into()),
        exe_path: None,
        boot_time_filetime: Some("10".into()),
        start_key: Some("456".into()),
    });
    record.ancestors = vec![ProcessIdentity {
        pid: 20,
        start_time_filetime: Some("20".into()),
        exe_path: None,
        boot_time_filetime: Some("10".into()),
        start_key: Some("123".into()),
    }];
    let mut reused = page.records[0].clone();
    reused.cursor.position = DecimalU64(8);
    let HistoryEvent::Log { record } = &mut reused.event else {
        panic!("log")
    };
    record.text = "reused parent".into();
    record.ancestors[0].start_key = Some("124".into());
    page.records.push(reused);
    let filter = venera_debug_protocol::Filter {
        components: vec!["kmd".into()],
        levels: vec!["info".into()],
        targets: vec![venera_debug_protocol::Target::ProcessInstance {
            pid: 20,
            start_time_filetime: "20".into(),
            include_descendants: true,
            boot_time_filetime: Some("10".into()), start_key: Some("123".into()),
        }],
    };
    let mut output = Vec::new();
    assert_eq!(
        consumer
            .display(&page, &filter, &mut output)
            .unwrap()
            .cursor
            .position
            .0,
        8
    );
    let displayed: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(displayed.len(), 2);
    assert_eq!(displayed[0]["type"], "gap");
    assert_eq!(displayed[1]["text"], "seven");
    assert_eq!(
        displayed[1]["ancestors"],
        json!([{"pid":20,"start_time_filetime":"20","exe_path":null,"boot_time_filetime":"10","start_key":"123"}])
    );
}

#[test]
fn archive_consumers_have_independent_progress_and_require_complete_coverage() {
    let mut archive = HistoryConsumer::new(Start::Beginning);
    let durable = HistoryCursor {
        journal_id: "j".into(),
        position: DecimalU64(5),
    };
    archive.range(range(8), Some(durable)).unwrap();
    let mut incomplete = page();
    incomplete.missing.remove(0);
    assert_eq!(
        archive.page(&incomplete).unwrap_err().message,
        "History page does not cover the requested range"
    );
    archive.page(&page()).unwrap();
    archive.consumed().unwrap();
    let mut second = HistoryConsumer::new(Start::Beginning);
    second.range(range(8), None).unwrap();
    assert_eq!(
        (
            archive.cursor().unwrap().position.0,
            second.cursor().unwrap().position.0
        ),
        (8, 0)
    );
    let HistoryEvent::Log { record } = page().records.remove(0).event else {
        panic!("log")
    };
    assert_eq!(
        (
            record.observed_at.as_str(),
            record.source_time.as_deref(),
            record.text.as_str()
        ),
        ("original observation", Some("original source"), "seven")
    );
}
