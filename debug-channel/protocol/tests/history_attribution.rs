use serde_json::{json, Value};
use venera_debug_protocol::{
    client::{encode, Client, HistoryConsumer, Start},
    v1::*,
};

fn cursor(position: u64) -> HistoryCursor {
    HistoryCursor {
        journal_id: "j".into(),
        position: DecimalU64(position),
    }
}

fn late_page() -> AttributionHistoryPage {
    serde_json::from_value(json!({
        "range":{"journal_id":"j","oldest":"41","high_water":"43"},
        "records":[
            {"cursor":cursor(41), "boot_id":"b", "recorded_at_ms":"70", "startup":true,
             "event":{"type":"log","record":{"stream_id":"s","seq":"301","observed_at":"obs",
                 "source_time":"src","source":"etw","component":"umd","level":"info","raw_level":"4",
                 "process":{"pid":42},"text":"retained body"}}},
            {"cursor":cursor(43), "boot_id":"b", "recorded_at_ms":"80", "startup":false,
             "event":{"type":"attribution_revision","target":cursor(41),"attribution":{
                 "process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\old.exe"},
                 "ancestors":[{"pid":7,"start_time_filetime":"90","exe_path":"C:\\parent.exe"}]}}}
        ],
        "missing":[{"first":"42","last":"42","reason":"retention_expired"}], "next":cursor(43)
    })).unwrap()
}

#[test]
fn revisions_replay_as_metadata_at_their_own_position() {
    let page = late_page();
    let wire = HistoryEncoding::Attribution
        .encode_page(page.clone(), |_| Ok(None))
        .unwrap();
    let decoded: AttributionHistoryPage = serde_json::from_value(wire.clone()).unwrap();
    let AttributionHistoryEvent::Log { record } = &decoded.records[0].event else {
        panic!()
    };
    let AttributionHistoryEvent::AttributionRevision {
        target,
        attribution,
    } = &decoded.records[1].event
    else {
        panic!()
    };
    assert_eq!(wire["records"][1]["cursor"]["position"], "43");
    assert_eq!(target, &cursor(41));
    let mut projected = record.clone();
    attribution.apply_to(&mut projected);
    attribution.apply_to(&mut projected);
    assert_eq!(
        serde_json::to_value(projected).unwrap(),
        json!({
            "stream_id":"s","seq":"301","observed_at":"obs","source_time":"src","source":"etw",
            "component":"umd","level":"info","raw_level":"4","session_id":null,"tid":null,"text":"retained body",
            "process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\old.exe"},
            "ancestors":[{"pid":7,"start_time_filetime":"90","exe_path":"C:\\parent.exe"}]
        })
    );
    assert_eq!(
        wire["records"][0]["event"]["record"]["process"]["start_time_filetime"],
        Value::Null
    );
    let mut consumer = HistoryConsumer::new(Start::Beginning);
    consumer.range(page.range, Some(cursor(40))).unwrap();
    consumer.attribution_page(&decoded).unwrap();
    assert_eq!(consumer.cursor(), Some(&cursor(40)));
    assert_eq!(consumer.consumed().unwrap().cursor, cursor(43));
}

#[test]
fn compatibility_log_snapshot_and_marker_preserve_cursor_coverage() {
    let page = late_page();
    let AttributionHistoryEvent::AttributionRevision { attribution, .. } = &page.records[1].event
    else {
        panic!()
    };
    let mut snapshot = late_page();
    snapshot.records.truncate(1);
    snapshot.missing.clear();
    snapshot.next = cursor(41);
    let wire = HistoryEncoding::Compatibility
        .encode_page(snapshot, |target| {
            Ok((target == &cursor(41)).then(|| attribution.clone()))
        })
        .unwrap();
    assert_eq!(
        wire["records"][0]["event"]["record"]["text"],
        "retained body"
    );
    assert_eq!(
        wire["records"][0]["event"]["record"]["process"]["start_time_filetime"],
        "100"
    );
    assert_eq!(
        wire["records"][0]["event"]["record"]["ancestors"][0]["pid"],
        7
    );

    let mut revision_only = late_page();
    revision_only.records.remove(0);
    revision_only.missing.clear();
    let wire = HistoryEncoding::Compatibility
        .encode_page(revision_only, |_| Ok(None))
        .unwrap();
    assert_eq!(
        wire["records"][0]["event"],
        json!({"type":"gap","gap": {
            "stream_id":"j","reason":"attribution_revision","count":"0","first_seq":null,"last_seq":null
        }})
    );
    assert_eq!(wire["missing"], json!([]));
    let legacy: HistoryPage = serde_json::from_value(wire).unwrap();
    let mut consumer = HistoryConsumer::new(Start::Beginning);
    consumer.range(page.range, Some(cursor(42))).unwrap();
    let mut display = Vec::new();
    let filter = venera_debug_protocol::Filter::default();
    assert_eq!(
        consumer
            .display(&legacy, &filter, &mut display)
            .unwrap()
            .cursor,
        cursor(43)
    );
    assert_eq!(display, b"");
}

fn write(client: &mut Client) -> Vec<Value> {
    let mut bytes = Vec::new();
    client.write(&mut bytes).unwrap();
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn handshake(client: &mut Client, guest: EndpointInfo) -> EndpointInfo {
    client.connect();
    let requests = write(client);
    client
        .receive(&encode(&json!([
            {"jsonrpc":"2.0","id":"guest","method":"endpoint.info","params":{}},
            {"jsonrpc":"2.0","id":requests[0]["id"],"result":guest},
            {"jsonrpc":"2.0","id":requests[1]["id"],"result":{
                "openrpc":"1.4.1","info":{"title":"Guest","version":"1.0.0"},"methods":[
                    {"name":"history.read","paramStructure":"by-name","params":[]}
                ],"components":{"schemas":{}}}}
        ])))
        .unwrap();
    serde_json::from_value(write(client)[0][0]["result"].clone()).unwrap()
}

#[test]
fn history_read_uses_bilateral_capabilities_on_each_connection() {
    let guest = EndpointInfo::new(EndpointRole::Guest, "test");
    let enabled = guest.clone().with_history_attribution();
    let mut opted_in = Client::with_history_attribution();
    let mut default = Client::default();
    for (endpoint, expected, sends_capability) in [
        (enabled.clone(), HistoryEncoding::Attribution, true),
        (enabled, HistoryEncoding::Compatibility, false),
        (guest, HistoryEncoding::Compatibility, true),
    ] {
        let client = if sends_capability {
            &mut opted_in
        } else {
            &mut default
        };
        let host = handshake(client, endpoint.clone());
        assert_eq!(
            host.capabilities
                .contains(&"history.attribution".to_owned()),
            sends_capability
        );
        let mut peer = PeerProtocol::default();
        peer.accept_endpoint(host).unwrap();
        assert_eq!(
            HistoryEncoding::negotiate(&endpoint, &peer),
            HistoryEncoding::Compatibility
        );
        peer.accept_discovery(
            serde_json::from_value(json!({
                "openrpc":"1.4.1","info":{"title":"Host","version":"1.0.0"},"methods":[
                    {"name":"endpoint.info","paramStructure":"by-name","params":[]}
                ],"components":{"schemas":{}}
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(HistoryEncoding::negotiate(&endpoint, &peer), expected);
        let id = client
            .request(Method::HistoryRead, json!({"after":cursor(40),"limit":3}))
            .unwrap();
        let value = HistoryEncoding::Attribution
            .encode_page(late_page(), |_| Ok(None))
            .unwrap();
        let replies = client
            .receive(&encode(&json!({"jsonrpc":"2.0","id":id,"result":value})))
            .unwrap();
        match expected {
            HistoryEncoding::Attribution => assert_eq!(
                replies[0].result.as_ref().unwrap()["next"],
                json!(cursor(43))
            ),
            HistoryEncoding::Compatibility => {
                assert_eq!(replies[0].result.as_ref().unwrap_err().code, -32004)
            }
        }
    }
}

#[test]
fn revision_progress_requires_an_earlier_target_and_resolved_attribution() {
    let wire = serde_json::to_value(late_page()).unwrap();
    for (path, value) in [
        ("/target/position", json!("0")),
        ("/target/position", json!("43")),
        ("/target/journal_id", json!("other")),
        ("/attribution/process/start_time_filetime", Value::Null),
        ("/attribution/ancestors/0/start_time_filetime", Value::Null),
    ] {
        let mut wire = wire.clone();
        *wire
            .pointer_mut(&format!("/records/1/event{path}"))
            .unwrap() = value;
        let page: AttributionHistoryPage = serde_json::from_value(wire).unwrap();
        let mut consumer = HistoryConsumer::new(Start::Beginning);
        consumer
            .range(page.range.clone(), Some(cursor(40)))
            .unwrap();
        assert_eq!(
            consumer.attribution_page(&page).unwrap_err().message,
            "Invalid history attribution revision"
        );
        assert_eq!(consumer.cursor(), Some(&cursor(40)));
    }
}
