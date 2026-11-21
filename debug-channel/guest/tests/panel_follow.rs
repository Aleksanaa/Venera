#[path = "../src/windows/follow_output.rs"]
mod follow_output;

use follow_output::{FollowPage, PanelOutput};
use serde_json::{json, Value};
use venera_debug_protocol::{
    client::{encode, Client, HistoryConsumer, Start},
    v1::*,
    Filter, Target,
};

fn cursor(n: u64) -> Value {
    json!({"journal_id":"journal","position":n.to_string()})
}

fn range(high_water: u64) -> Value {
    json!({"journal_id":"journal","oldest":"1","high_water":high_water.to_string()})
}

fn entry(n: u64, event: Value) -> Value {
    json!({"cursor":cursor(n),"boot_id":"boot","recorded_at_ms":"1741250000000","startup":true,"event":event})
}

fn log() -> Value {
    json!({"type":"log","record":{"stream_id":"capture","seq":"30","observed_at":"2026-11-20T00:00:00.123Z",
        "source_time":"original ticks","source":"etw","component":"umd","level":"info","raw_level":"4",
        "process":{"pid":42},"text":"original body 中文"}})
}

fn attribution() -> Value {
    json!({"process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\A.exe"},
        "ancestors":[{"pid":7,"start_time_filetime":"90","exe_path":"C:\\parent.exe"}]})
}

fn filter(pid: u32, creation: &str, descendants: bool) -> Filter {
    Filter {
        targets: vec![Target::ProcessInstance {
            pid,
            start_time_filetime: creation.into(),
            include_descendants: descendants,
            boot_time_filetime: None,
            start_key: None,
        }],
        ..Filter::default()
    }
}

fn read_jsonl(bytes: &[u8]) -> Vec<Value> {
    bytes
        .split(|c| *c == b'\n')
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_slice(s).unwrap())
        .collect()
}

fn write_client(client: &mut Client) -> Vec<Value> {
    let mut bytes = Vec::new();
    while client.has_output() {
        client.write(&mut bytes).unwrap();
    }
    read_jsonl(&bytes)
}

fn client(opt_in: bool, capable: bool) -> Client {
    let mut client = if opt_in {
        Client::with_history_attribution()
    } else {
        Client::default()
    };
    client.connect();
    let requests = write_client(&mut client);
    let guest = EndpointInfo::new(EndpointRole::Guest, "fixture");
    client.receive(&encode(&json!([
        {"jsonrpc":"2.0","id":"guest","method":"endpoint.info","params":{}},
        {"jsonrpc":"2.0","id":requests[0]["id"],"result": if capable {guest.with_history_attribution()} else {guest}},
        {"jsonrpc":"2.0","id":requests[1]["id"],"result":{
            "openrpc":"1.4.1","info":{"title":"Guest","version":"1.0.0"},"methods":[
                {"name":"history.read","paramStructure":"by-name","params":[]}
            ],"components":{"schemas":{}}}}
    ]))).unwrap();
    write_client(&mut client);
    client
}

fn read(client: &mut Client, consumer: &mut HistoryConsumer, page: Value) -> FollowPage {
    consumer
        .range(serde_json::from_value(page["range"].clone()).unwrap(), None)
        .unwrap();
    let result = wire_read(client, consumer.read_request(), page);
    FollowPage::decode(consumer, client.history_encoding(), result).unwrap()
}

fn wire_read(client: &mut Client, request: HistoryRead, page: Value) -> Value {
    let id = client
        .request(Method::HistoryRead, serde_json::to_value(request).unwrap())
        .unwrap();
    write_client(client);
    client
        .receive(&encode(&json!({"jsonrpc":"2.0","id":id,"result":page})))
        .unwrap()
        .pop()
        .unwrap()
        .result
        .unwrap()
}

#[test]
fn negotiated_follow_revises_the_original_row_after_reconnect() {
    for (selection, visible) in [
        (filter(42, "100", false), true),
        (filter(42, "200", false), false),
        (filter(7, "90", true), true),
        (filter(7, "90", false), false),
    ] {
        let mut connection = client(true, true);
        let mut consumer = HistoryConsumer::new(Start::Now);
        let mut panel = PanelOutput::default();
        let mut jsonl = Vec::new();
        let FollowPage::Attribution(first) = read(
            &mut connection,
            &mut consumer,
            json!({"range":range(1),"records":[],"missing":[],"next":cursor(1)}),
        ) else {
            panic!()
        };
        panel
            .page(&mut consumer, &first, selection.clone(), &mut jsonl)
            .unwrap();
        let FollowPage::Attribution(log_page) = read(
            &mut connection,
            &mut consumer,
            json!({"range":range(2),"records":[entry(2,log())],"missing":[],"next":cursor(2)}),
        ) else {
            panic!()
        };
        panel
            .page(&mut consumer, &log_page, selection, &mut jsonl)
            .unwrap();
        let FollowPage::Attribution(delta) = read(
            &mut client(true, true),
            &mut consumer,
            json!({"range":range(3),"records":[entry(3,json!({"type":"attribution_revision","target":cursor(2),"attribution":attribution()}))],
                "missing":[],"next":cursor(3)}),
        ) else {
            panic!()
        };
        panel
            .page(&mut consumer, &delta, Filter::default(), &mut jsonl)
            .unwrap();
        let lines = read_jsonl(&jsonl);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["type"], "log");
        assert_eq!(lines[0]["cursor"], cursor(2));
        assert_eq!(lines[0]["text"], "original body 中文");
        assert_eq!(lines[0]["observed_at"], "2026-11-20T00:00:00.123Z");
        assert_eq!(lines[0]["source_time"], "original ticks");
        assert_eq!(lines[0]["visible"], false);
        assert_eq!(
            lines[1],
            json!({"type":"attribution_revision","target":cursor(2),"attribution":attribution(),"visible":visible})
        );
        assert_eq!(serde_json::to_value(consumer.cursor()).unwrap(), cursor(3));
    }
}

#[test]
fn expired_delta_recovers_the_projection_from_the_original_cursor() {
    let mut consumer = HistoryConsumer::new(Start::Now);
    let mut panel = PanelOutput::default();
    let mut connection = client(true, true);
    consumer
        .range(serde_json::from_value(range(1)).unwrap(), None)
        .unwrap();
    let FollowPage::Attribution(log_page) = read(
        &mut connection,
        &mut consumer,
        json!({"range":range(2),"records":[entry(2,log())],"missing":[],"next":cursor(2)}),
    ) else {
        panic!()
    };
    let mut jsonl = Vec::new();
    panel
        .page(&mut consumer, &log_page, filter(7, "90", true), &mut jsonl)
        .unwrap();
    let FollowPage::Attribution(gap) = read(
        &mut connection,
        &mut consumer,
        json!({"range":range(4),"records":[],"missing":[{"first":"3","last":"4","reason":"retention_expired"}],"next":cursor(4)}),
    ) else {
        panic!()
    };
    panel
        .page(&mut consumer, &gap, Filter::default(), &mut jsonl)
        .unwrap();
    let mut snapshot = log();
    snapshot["record"]["process"] = attribution()["process"].clone();
    snapshot["record"]["ancestors"] = attribution()["ancestors"].clone();
    let bounds = panel.recovery_bounds("journal").unwrap();
    for _ in 0..2 {
        panel.recover(serde_json::from_value(range(6)).unwrap(), bounds, &mut jsonl, |request| {
            // The snapshot incorporates revisions already committed past the live reader.
            assert_eq!(serde_json::to_value(&request).unwrap(),json!({"after":cursor(1),"through":"2","limit":256}));
            Ok(serde_json::from_value(wire_read(&mut client(false,true), request,
                json!({"range":range(6),"records":[entry(2,snapshot.clone())],"missing":[],"next":cursor(2)})))?)
        }).unwrap();
    }
    let mut superseded = attribution();
    superseded["ancestors"] = json!([]);
    let FollowPage::Attribution(older) = read(
        &mut connection,
        &mut consumer,
        json!({"range":range(5),"records":[entry(5,json!({"type":"attribution_revision","target":cursor(2),"attribution":superseded}))],
            "missing":[],"next":cursor(5)}),
    ) else {
        panic!()
    };
    panel
        .page(&mut consumer, &older, Filter::default(), &mut jsonl)
        .unwrap();
    let lines = read_jsonl(&jsonl);
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[1],
        json!({"type":"gap","reason":"retention_expired","first_position":"3","last_position":"4"})
    );
    assert_eq!(
        lines[2],
        json!({"type":"attribution_revision","target":cursor(2),"attribution":attribution(),"visible":true})
    );
    assert_eq!(serde_json::to_value(consumer.cursor()).unwrap(), cursor(5));
}

#[test]
fn legacy_follow_emits_existing_jsonl() {
    for (opt_in, capable) in [(true, false), (false, true)] {
        let mut consumer = HistoryConsumer::new(Start::Now);
        consumer
            .range(serde_json::from_value(range(1)).unwrap(), None)
            .unwrap();
        let FollowPage::Compatibility(page) = read(
            &mut client(opt_in, capable),
            &mut consumer,
            json!({"range":range(2),"records":[entry(2,log())],"missing":[],"next":cursor(2)}),
        ) else {
            panic!()
        };
        let mut jsonl = Vec::new();
        consumer
            .display(&page, &Filter::default(), &mut jsonl)
            .unwrap();
        assert_eq!(
            read_jsonl(&jsonl),
            vec![
                json!({"type":"log","cursor":cursor(2),"boot_id":"boot","recorded_at_ms":"1741250000000","startup":true,
            "stream_id":"capture","seq":"30","observed_at":"2026-11-20T00:00:00.123Z","source_time":"original ticks","source":"etw",
            "component":"umd","level":"info","raw_level":"4","session_id":null,"tid":null,"process":{"pid":42,"start_time_filetime":null,"exe_path":null},
            "text":"original body 中文"})
            ]
        );
    }
}
