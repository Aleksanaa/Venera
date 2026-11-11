use serde::Deserialize;
use serde_json::{json, Value};
use venera_debug_protocol::v1::*;

#[test]
fn shared_codec_samples() {
    let samples: Vec<Value> = serde_json::from_str(include_str!("v1_samples.json")).unwrap();
    for sample in samples {
        fn accepts<T: for<'de> Deserialize<'de>>(value: Value) -> bool {
            serde_json::from_value::<T>(value).is_ok()
        }
        let value = sample["value"].clone();
        let accepted = match sample["type"].as_str().unwrap() {
            "EndpointInfo" => accepts::<EndpointInfo>(value),
            "DecimalU64" => accepts::<DecimalU64>(value),
            "ProcessIdentity" => accepts::<ProcessIdentity>(value),
            "FilterSet" => accepts::<FilterSet>(value),
            "Gap" => accepts::<Gap>(value),
            "Empty" => accepts::<Empty>(value),
            "SystemInfo" => accepts::<SystemInfo>(value),
            other => panic!("unknown sample type {other}"),
        };
        assert_eq!(accepted, sample["serde"].as_bool().unwrap(), "{sample}");
    }
    let number: DecimalU64 = serde_json::from_str("\"18446744073709551615\"").unwrap();
    assert_eq!(number.0, 18_446_744_073_709_551_615);
    assert_eq!(
        serde_json::to_string(&number).unwrap(),
        "\"18446744073709551615\""
    );
    let filter: FilterSet =
        serde_json::from_str(r#"{"filter":{"components":[],"levels":[],"targets":[]}}"#).unwrap();
    assert_eq!(
        serde_json::to_value(filter).unwrap(),
        json!({"filter":{"components":[],"levels":[],"targets":[]},"duration_ms":null})
    );
}

fn status() -> Status {
    Status {
        forwarding_enabled: true,
        revision: DecimalU64(7),
        effective_filter: Filter::default(),
        expires_at: None,
        health: json!({}),
        loss: json!({}),
    }
}
#[derive(Default)]
struct Recorder {
    events: Vec<Value>,
    resets: u32,
}
impl Handler for Recorder {
    fn endpoint_info(&mut self, _: Empty) -> RpcResult<EndpointInfo> {
        Ok(EndpointInfo::new(EndpointRole::Guest, "0.9.0"))
    }
    fn status_get(&mut self, _: Empty) -> RpcResult<Status> {
        Ok(status())
    }
    fn filter_set(&mut self, params: FilterSet) -> RpcResult<Status> {
        let mut result = status();
        result.effective_filter = params.filter;
        Ok(result)
    }
    fn filter_reset(&mut self, _: Empty) -> RpcResult<Status> {
        self.resets += 1;
        Err(RpcError {
            code: -32000,
            message: "forwarding disabled".into(),
            data: Some(json!({"revision":"7"})),
        })
    }
    fn system_info_get(&mut self, _: Empty) -> RpcResult<SystemInfo> {
        Ok(SystemInfo {
            collected_at: "2026-11-11T00:00:00Z".into(),
            session_id: Some(2),
            sections: vec![],
        })
    }
    fn discover(&mut self, _: Empty) -> RpcResult<Discovery> {
        Ok(serde_json::from_str(include_str!("../v1/guest.openrpc.json")).unwrap())
    }
    fn status_changed(&mut self, params: Status) {
        self.events.push(json!({"revision": params.revision}));
    }
    fn log(&mut self, params: LogRecord) {
        self.events
            .push(json!({"seq": params.seq, "text": params.text}));
    }
    fn gap(&mut self, params: Gap) {
        self.events.push(json!({"count": params.count}));
    }
}

#[test]
fn typed_handlers_dispatch_requests_and_events() {
    let mut handler = Recorder::default();
    let request =
        decode(r#"{"jsonrpc":"2.0","id":"s","method":"status.get","params":{}}"#).unwrap();
    assert_eq!(
        request,
        Incoming::StatusGet {
            id: "s".into(),
            params: Empty {}
        }
    );
    let reply = request.dispatch(&mut handler).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(reply).unwrap(),
        json!({"jsonrpc":"2.0","id":"s","result":{"forwarding_enabled":true,"revision":"7","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{},"loss":{}}})
    );
    let request = decode(r#"{"jsonrpc":"2.0","id":"f","method":"filter.set","params":{"filter":{"components":["umd"],"levels":[],"targets":[]},"duration_ms":"100"}}"#).unwrap();
    let reply = serde_json::to_value(request.dispatch(&mut handler).unwrap().unwrap()).unwrap();
    assert_eq!(
        reply["result"]["effective_filter"],
        json!({"components":["umd"],"levels":[],"targets":[]})
    );
    let request =
        decode(r#"{"jsonrpc":"2.0","id":"r","method":"filter.reset","params":{}}"#).unwrap();
    assert_eq!(
        serde_json::to_value(request.dispatch(&mut handler).unwrap().unwrap()).unwrap(),
        json!({"jsonrpc":"2.0","id":"r","error":{"code":-32000,"message":"forwarding disabled","data":{"revision":"7"}}})
    );
    let request =
        decode(r#"{"jsonrpc":"2.0","id":"i","method":"system.info.get","params":{}}"#).unwrap();
    assert_eq!(
        serde_json::to_value(request.dispatch(&mut handler).unwrap().unwrap()).unwrap(),
        json!({"jsonrpc":"2.0","id":"i","result":{"collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[]}})
    );
    for event in [
        r#"{"jsonrpc":"2.0","method":"status.changed","params":{"forwarding_enabled":true,"revision":"9","effective_filter":{"components":[],"levels":[],"targets":[]},"health":{},"loss":{}}}"#,
        r#"{"jsonrpc":"2.0","method":"log","params":{"stream_id":"stream","seq":"18446744073709551615","observed_at":"now","source":"dbwin","component":"umd","level":"debug","raw_level":"TRACE","text":"ready"}}"#,
        r#"{"jsonrpc":"2.0","method":"gap","params":{"stream_id":"stream","reason":"overflow","count":"3"}}"#,
    ] {
        assert_eq!(decode(event).unwrap().dispatch(&mut handler).unwrap(), None);
    }
    assert_eq!(
        handler.events,
        vec![
            json!({"revision":"9"}),
            json!({"seq":"18446744073709551615","text":"ready"}),
            json!({"count":"3"})
        ]
    );
    let request =
        decode(r#"{"jsonrpc":"2.0","id":"d","method":"rpc.discover","params":{}}"#).unwrap();
    let reply = serde_json::to_value(request.dispatch(&mut handler).unwrap().unwrap()).unwrap();
    assert_eq!(
        reply["result"]["info"],
        json!({"title":"Venera guest","version":"1.0.0"})
    );
}

#[test]
fn wire_shape_is_checked_before_dispatch() {
    for text in [
        r#"{"jsonrpc":"1.0","id":"a","method":"status.get","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"status.get","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"status.get","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"status.get","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":"a","method":"status.get","params":[]}"#,
        r#"{"jsonrpc":"2.0","id":"a","method":"status.get"}"#,
        r#"{"jsonrpc":"2.0","id":"a","method":"unknown","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":"a","method":"gap","params":{"stream_id":"s","reason":"overflow"}}"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"gap","params":{"stream_id":"s","reason":"overflow"}}"#,
    ] {
        assert!(decode(text).is_err(), "{text}");
    }
    assert_eq!(
        decode(r#"{"jsonrpc":"2.0","id":"a","method":"filter.reset","params":{}}"#)
            .unwrap()
            .method(),
        "filter.reset"
    );
    let response: Response<Value> =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":"a","result":null}"#).unwrap();
    assert_eq!(
        response,
        Response::Success {
            jsonrpc: Version::V2,
            id: "a".into(),
            result: Value::Null
        }
    );
    let response: Response<Value> = serde_json::from_str(r#"{"jsonrpc":"2.0","id":"a","error":{"code":-32602,"message":"invalid parameters","data":{"field":"filter"}}}"#).unwrap();
    assert_eq!(
        response,
        Response::Error {
            jsonrpc: Version::V2,
            id: "a".into(),
            error: RpcError {
                code: -32602,
                message: "invalid parameters".into(),
                data: Some(json!({"field":"filter"}))
            }
        }
    );
    for text in [
        r#"{"jsonrpc":"2.0","id":"a","result":null,"error":{"code":-32603,"message":"error"}}"#,
        r#"{"jsonrpc":"2.0","id":"a"}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":null}"#,
        r#"{"jsonrpc":"1.0","id":"a","result":null}"#,
        r#"{"jsonrpc":"2.0","id":"a","error":{"code":-32602}}"#,
        r#"{"jsonrpc":"2.0","id":"a","error":{"code":1.5,"message":"invalid"}}"#,
        r#"{"jsonrpc":"2.0","id":"a","error":{"code":2147483648,"message":"invalid"}}"#,
        r#"{"jsonrpc":"2.0","id":"a","error":null}"#,
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32602,"message":"invalid"}}"#,
    ] {
        assert!(
            serde_json::from_str::<Response<Value>>(text).is_err(),
            "{text}"
        );
    }
}

#[cfg(feature = "schema")]
#[test]
fn discovery_methods_decode_to_the_registered_handler_branches() {
    for receiver in [Receiver::Guest, Receiver::Host] {
        let document = schema::document(receiver);
        let generated = serde_json::to_value(&document).unwrap();
        let committed: Value = serde_json::from_str(match receiver {
            Receiver::Guest => include_str!("../v1/guest.openrpc.json"),
            Receiver::Host => include_str!("../v1/host.openrpc.json"),
        })
        .unwrap();
        assert_eq!(generated, committed);
        let mut names = Vec::new();
        for method in document.methods {
            let params = match method.name.as_str() {
                "filter.set" => json!({"filter":{"components":[],"levels":[],"targets":[]}}),
                "status.changed" => serde_json::to_value(status()).unwrap(),
                "log" => {
                    json!({"stream_id":"s","seq":"0","observed_at":"now","source":"dbwin","component":"umd","level":"debug","raw_level":"TRACE","text":"ready"})
                }
                "gap" => json!({"stream_id":"s","reason":"overflow"}),
                _ => json!({}),
            };
            let mut wire = json!({"jsonrpc":"2.0","method":method.name,"params":params});
            if method.result.is_some() {
                wire["id"] = json!("check");
            }
            let incoming = decode(&wire.to_string()).unwrap();
            assert_eq!(incoming.method(), method.name);
            assert!(incoming.receiver().is_none_or(|role| role == receiver));
            names.push(method.name);
        }
        assert_eq!(
            names,
            match receiver {
                Receiver::Guest => vec![
                    "endpoint.info",
                    "status.get",
                    "filter.set",
                    "filter.reset",
                    "system.info.get",
                    "rpc.discover"
                ],
                Receiver::Host => vec![
                    "endpoint.info",
                    "rpc.discover",
                    "status.changed",
                    "log",
                    "gap"
                ],
            }
        );
    }
}

fn execute_packet(text: &str, handler: &mut Recorder) -> Option<Value> {
    let RpcPacket { items, mut batch } = RpcPacket::decode(text.as_bytes(), Receiver::Guest);
    for item in items {
        if let PacketItem::Request(request) = item {
            let (correlation, result) = request.dispatch(handler).unwrap();
            if let Some(correlation) = correlation {
                batch.complete(correlation, result).unwrap();
            }
        }
    }
    batch.take_response()
}

#[test]
fn standard_packet_errors_and_ids() {
    let mut handler = Recorder::default();
    for text in [
        "[]",
        "1",
        "null",
        "true",
        "{}",
        r#"{"jsonrpc":"1.0","id":"a","method":"status.get"}"#,
        r#"{"jsonrpc":"2.0","id":{},"method":"status.get"}"#,
    ] {
        assert_eq!(
            execute_packet(text, &mut handler),
            Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}})
            ),
            "{text}"
        );
    }
    assert_eq!(
        execute_packet("{", &mut handler),
        Some(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}))
    );
    assert_eq!(
        execute_packet(
            r#"{"jsonrpc":"2.0","id":"x","method":"missing"}"#,
            &mut handler
        ),
        Some(
            json!({"jsonrpc":"2.0","id":"x","error":{"code":-32601,"message":"Method not found"}})
        )
    );
    assert_eq!(
        execute_packet(
            r#"{"jsonrpc":"2.0","id":"x","method":"filter.set","params":{}}"#,
            &mut handler
        ),
        Some(json!({"jsonrpc":"2.0","id":"x","error":{"code":-32602,"message":"Invalid params"}}))
    );
    assert_eq!(
        execute_packet(
            r#"{"jsonrpc":"2.0","id":null,"method":"endpoint.info","params":[]}"#,
            &mut handler
        ),
        Some(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32602,"message":"Invalid params"}}))
    );
    for (text, expected) in [
        (
            r#"{"jsonrpc":"2.0","id":42,"method":"endpoint.info"}"#,
            json!({"jsonrpc":"2.0","id":42,"result":{"role":"guest","software_version":"0.9.0","protocol_version":"1.0.0"}}),
        ),
        (
            r#"{"jsonrpc":"2.0","id":null,"method":"endpoint.info","params":{"future":true},"future":true}"#,
            json!({"jsonrpc":"2.0","id":null,"result":{"role":"guest","software_version":"0.9.0","protocol_version":"1.0.0"}}),
        ),
        (
            r#"{"jsonrpc":"2.0","id":"venera-1","method":"endpoint.info"}"#,
            json!({"jsonrpc":"2.0","id":"venera-1","result":{"role":"guest","software_version":"0.9.0","protocol_version":"1.0.0"}}),
        ),
    ] {
        assert_eq!(execute_packet(text, &mut handler), Some(expected));
    }
}

#[test]
fn notifications_execute_and_batch_collects_mixed_results() {
    let mut handler = Recorder::default();
    let RpcPacket { items, mut batch } = RpcPacket::decode(br#"[{"jsonrpc":"2.0","method":"endpoint.info"},{"jsonrpc":"2.0","method":"unknown"},{"jsonrpc":"2.0","method":"filter.set","params":[]}]"#, Receiver::Guest);
    assert_eq!(items.len(), 1);
    for item in items {
        let PacketItem::Request(request) = item else {
            panic!("expected request")
        };
        assert!(request.is_notification());
        let (correlation, result) = request.dispatch(&mut handler).unwrap();
        assert_eq!(correlation, None);
        assert_eq!(
            result.unwrap(),
            json!({"role":"guest","software_version":"0.9.0","protocol_version":"1.0.0"})
        );
    }
    assert_eq!(batch.take_response(), None);
    let RpcPacket { items, mut batch } = RpcPacket::decode(br#"[{"jsonrpc":"2.0","id":"slow","method":"system.info.get"},false,{"jsonrpc":"2.0","id":7,"method":"endpoint.info"},{"jsonrpc":"2.0","id":"bad","method":"status.get","params":[]},{"jsonrpc":"2.0","method":"filter.reset"}]"#, Receiver::Guest);
    let mut pending = Vec::new();
    for item in items {
        let PacketItem::Request(request) = item else {
            continue;
        };
        let (correlation, result) = request.dispatch(&mut handler).unwrap();
        if let Some(correlation) = correlation {
            pending.push((correlation, result));
        }
    }
    assert_eq!(batch.take_response(), None);
    let (correlation, result) = pending.pop().unwrap();
    batch.complete(correlation, result).unwrap();
    assert_eq!(batch.take_response(), None);
    let (correlation, result) = pending.pop().unwrap();
    batch.complete(correlation, result).unwrap();
    assert_eq!(
        batch.take_response(),
        Some(json!([
            {"jsonrpc":"2.0","id":"slow","result":{"collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[]}},
            {"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}},
            {"jsonrpc":"2.0","id":7,"result":{"role":"guest","software_version":"0.9.0","protocol_version":"1.0.0"}},
            {"jsonrpc":"2.0","id":"bad","error":{"code":-32602,"message":"Invalid params"}}
        ]))
    );
    assert_eq!(batch.take_response(), None);
}

#[test]
fn notification_business_call_has_observable_side_effect() {
    let RpcPacket { items, mut batch } = RpcPacket::decode(br#"{"jsonrpc":"2.0","method":"gap","params":{"stream_id":"s","reason":"overflow","count":"2"}}"#, Receiver::Host);
    let mut handler = Recorder::default();
    for item in items {
        let PacketItem::Request(request) = item else {
            panic!("expected request")
        };
        let (correlation, result) = request.dispatch(&mut handler).unwrap();
        assert_eq!(correlation, None);
        assert_eq!(result.unwrap(), Value::Null);
    }
    assert_eq!(handler.events, vec![json!({"count":"2"})]);
    assert_eq!(batch.take_response(), None);
    assert_eq!(
        execute_packet(r#"{"jsonrpc":"2.0","method":"filter.reset"}"#, &mut handler),
        None
    );
    assert_eq!(handler.resets, 1);
}

#[test]
fn packet_response_validation_and_forward_fields() {
    for (text, expected) in [
        (
            r#"{"jsonrpc":"2.0","id":1.5,"result":null,"future":true}"#,
            json!({"jsonrpc":"2.0","id":1.5,"result":null}),
        ),
        (
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32601,"message":"Method not found","future":true}}"#,
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32601,"message":"Method not found"}}),
        ),
    ] {
        let response: RpcResponse = serde_json::from_str(text).unwrap();
        assert_eq!(serde_json::to_value(response).unwrap(), expected);
        let packet = RpcPacket::decode(text.as_bytes(), Receiver::Host);
        let PacketItem::Response(response) = &packet.items[0] else {
            panic!("expected response")
        };
        assert_eq!(serde_json::to_value(response).unwrap(), expected);
    }
    for text in [
        r#"{"jsonrpc":"2.0","id":"a","result":null,"error":{"code":1,"message":"x"}}"#,
        r#"{"jsonrpc":"2.0","id":{},"result":null}"#,
        r#"{"jsonrpc":"2.0","result":null}"#,
        r#"{"jsonrpc":"1.0","id":"a","result":null}"#,
        r#"{"jsonrpc":"2.0","id":true,"error":{"code":1,"message":"x"}}"#,
    ] {
        assert!(serde_json::from_str::<RpcResponse>(text).is_err(), "{text}");
    }
}

#[test]
fn jsonl_preserves_utf8_bytes_and_recovers_after_overflow() {
    let mut framer = JsonlFramer::default();
    assert!(framer
        .push(&[b' ', b'\n', b'\r', b'\n', b'{', b'"', 0xe4])
        .is_empty());
    let lines = framer.push(&[0xb8, 0xad, b'"', b'}', b'\r', b'\n', 0xff, b'\n']);
    assert_eq!(lines[0].as_ref().unwrap().bytes(), "{\"中\"}\r".as_bytes());
    assert_eq!(lines[0].as_ref().unwrap().text(), "{\"中\"}\r");
    assert_eq!(lines[1], Err(RpcError::new(-32700, "Parse error")));
    assert_eq!(
        framer.push(&vec![b'x'; 1024 * 1024 + 1]),
        vec![Err(RpcError::new(-32700, "Frame exceeds 1 MiB"))]
    );
    assert_eq!(framer.buffered_len(), 0);
    assert!(framer.push(&vec![b'x'; 2 * 1024 * 1024]).is_empty());
    assert_eq!(framer.buffered_len(), 0);
    let lines = framer.push(b"ignored\n{}\n");
    assert_eq!(lines[0].as_ref().unwrap().bytes(), b"{}");
    assert_eq!(lines.len(), 1);
    assert!(framer.push(&vec![b' '; 1024 * 1024]).is_empty());
    assert!(framer.push(b"\n").is_empty());
    framer.push(b"partial");
    framer.clear();
    assert_eq!(framer.push(b"[]\n")[0].as_ref().unwrap().bytes(), b"[]");
    let mut maximum = vec![b'x'; 1024 * 1024];
    maximum.push(b'\n');
    let accepted = framer.push(&maximum);
    assert_eq!(
        accepted[0].as_ref().unwrap().bytes(),
        &maximum[..1024 * 1024]
    );
    assert_eq!(accepted.len(), 1);
    for split in 0..=9 {
        let mut framer = JsonlFramer::default();
        let wire = "{\"中\":1}\n".as_bytes();
        let mut frames = framer.push(&wire[..split]);
        frames.extend(framer.push(&wire[split..]));
        assert_eq!(frames[0].as_ref().unwrap().bytes(), "{\"中\":1}".as_bytes());
        assert_eq!(frames.len(), 1);
    }
}

#[cfg(feature = "schema")]
#[test]
fn peer_negotiates_version_and_actual_registered_methods_per_connection() {
    let mut peer = PeerProtocol::default();
    assert_eq!(
        peer.require(Method::StatusGet),
        Err(RpcError::new(-32001, "not_ready"))
    );
    peer.accept_endpoint(EndpointInfo::new(EndpointRole::Guest, "999.0.0"))
        .unwrap();
    assert_eq!(
        peer.require(Method::StatusGet),
        Err(RpcError::new(-32001, "not_ready"))
    );
    let mut doc = schema::document(Receiver::Guest);
    doc.methods
        .retain(|method| method.name != "system.info.get");
    peer.accept_discovery(doc).unwrap();
    assert_eq!(peer.require(Method::StatusGet), Ok(()));
    assert_eq!(
        peer.require(Method::SystemInfoGet),
        Err(RpcError::new(-32002, "unsupported"))
    );
    assert_eq!(
        peer.require(Method::Log),
        Err(RpcError::new(-32002, "unsupported"))
    );
    peer.disconnect();
    assert_eq!(peer.endpoint(), None);
    assert_eq!(peer.discovery(), None);
    peer.connect();
    peer.accept_discovery(schema::document(Receiver::Guest))
        .unwrap();
    assert_eq!(
        peer.require(Method::StatusGet),
        Err(RpcError::new(-32001, "not_ready"))
    );
    let mut endpoint = EndpointInfo::new(EndpointRole::Guest, "0.0.1");
    endpoint.protocol_version = "1.1.0".into();
    assert_eq!(
        peer.accept_endpoint(endpoint.clone()),
        Err(RpcError::new(-32003, "incompatible_protocol"))
    );
    let mut doc = schema::document(Receiver::Guest);
    doc.info.version = "1.1.0".into();
    peer.accept_discovery(doc).unwrap();
    assert_eq!(peer.require(Method::StatusGet), Ok(()));
    endpoint.protocol_version = "2.0.0".into();
    assert_eq!(
        peer.accept_endpoint(endpoint),
        Err(RpcError::new(-32003, "incompatible_protocol"))
    );
    assert_eq!(
        peer.require(Method::StatusGet),
        Err(RpcError::new(-32003, "incompatible_protocol"))
    );
    peer.connect();
    assert_eq!(
        peer.require(Method::StatusGet),
        Err(RpcError::new(-32001, "not_ready"))
    );
}

#[test]
fn endpoint_version_parsing_uses_stable_semver() {
    let endpoint: EndpointInfo = serde_json::from_str(
        r#"{"role":"host","software_version":"dev","protocol_version":"1.12.300","future":true}"#,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(endpoint).unwrap(),
        json!({"role":"host","software_version":"dev","protocol_version":"1.12.300"})
    );
    for version in [
        "1",
        "1.0",
        "01.0.0",
        "1.0.0-beta",
        "1.0.0+build",
        "1.0.0\n",
        "1.-1.0",
    ] {
        assert!(
            serde_json::from_value::<EndpointInfo>(
                json!({"role":"host","software_version":"dev","protocol_version":version})
            )
            .is_err(),
            "{version}"
        );
    }
}
