use serde_json::json;
use venera_debug_protocol::v1::*;

#[test]
fn persisted_log_and_gap_keep_original_sequences() {
    let log = json!({
        "cursor":{"journal_id":"journal-1","position":"42"},
        "boot_id":"boot-2","recorded_at_ms":"1794873600123","startup":true,
        "event":{"type":"log","record":{
            "stream_id":"old-stream","seq":"7","observed_at":"now",
            "source_time":null,"source":"dbwin","component":"umd","level":"debug",
            "raw_level":"TRACE","session_id":null,"process":{"pid":42,"start_time_filetime":"100","exe_path":null},
            "ancestors":[{"pid":20,"start_time_filetime":"20","exe_path":"C:\\parent.exe"}],"tid":null,"text":"ready"
        }}
    });
    let gap = json!({
        "cursor":{"journal_id":"journal-1","position":"43"},
        "boot_id":"boot-2","recorded_at_ms":"1794873600124","startup":false,
        "event":{"type":"gap","gap":{
            "stream_id":"old-stream","reason":"overflow","count":"3",
            "first_seq":"8","last_seq":"10"
        }}
    });
    let record: HistoryRecord = serde_json::from_value(log.clone()).unwrap();
    assert_eq!(record.cursor.position, DecimalU64(42));
    let HistoryEvent::Log { record: original } = &record.event else {
        panic!("expected log")
    };
    assert_eq!(original.seq, 7);
    assert_eq!(serde_json::to_value(record).unwrap(), log);
    let record: HistoryRecord = serde_json::from_value(gap.clone()).unwrap();
    assert_eq!(record.cursor.position, DecimalU64(43));
    let HistoryEvent::Gap { gap: original } = &record.event else {
        panic!("expected gap")
    };
    assert_eq!(
        (original.first_seq, original.last_seq),
        (Some(DecimalU64(8)), Some(DecimalU64(10)))
    );
    assert_eq!(serde_json::to_value(record).unwrap(), gap);
}

#[test]
fn history_wire_roundtrips_pages_and_canonical_numbers() {
    let page = json!({
        "range":{"journal_id":"j","oldest":"4","high_water":"9"},
        "records":[],"missing":[{"first":"1","last":"3","reason":"retention_expired"}],
        "next":{"journal_id":"j","position":"3"}
    });
    assert_eq!(
        serde_json::to_value(serde_json::from_value::<HistoryPage>(page.clone()).unwrap()).unwrap(),
        page
    );
    let request: HistoryRead = serde_json::from_value(json!({"limit":256})).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({"after":null,"through":null,"limit":256})
    );
    let cursor: HistoryCursor =
        serde_json::from_value(json!({"journal_id":"j","position":"18446744073709551615"}))
            .unwrap();
    assert_eq!(cursor.position, DecimalU64(u64::MAX));
    for invalid in [
        json!(0),
        json!("01"),
        json!("+1"),
        json!("-1"),
        json!("1.0"),
        json!("1\n"),
        json!("18446744073709551616"),
    ] {
        assert!(serde_json::from_value::<HistoryCursor>(
            json!({"journal_id":"j","position":invalid})
        )
        .is_err());
        assert!(
            serde_json::from_value::<HistoryRead>(json!({"through":invalid,"limit":1})).is_err()
        );
        assert!(serde_json::from_value::<HistoryRecord>(json!({
            "cursor":{"journal_id":"j","position":"1"},"boot_id":"b","startup":false,
            "recorded_at_ms":invalid,"event":{"type":"gap","gap":{"stream_id":"s","reason":"overflow"}}
        })).is_err());
    }
}

struct HistoryHandler;
impl Handler for HistoryHandler {
    fn endpoint_info(&mut self, _: Empty) -> RpcResult<EndpointInfo> {
        Ok(EndpointInfo::new(EndpointRole::Guest, "test"))
    }
    fn status_get(&mut self, _: Empty) -> RpcResult<Status> {
        unreachable!()
    }
    fn filter_set(&mut self, _: FilterSet) -> RpcResult<Status> {
        unreachable!()
    }
    fn filter_reset(&mut self, _: Empty) -> RpcResult<Status> {
        unreachable!()
    }
    fn system_info_get(&mut self, _: Empty) -> RpcResult<SystemInfo> {
        unreachable!()
    }
    fn discover(&mut self, _: Empty) -> RpcResult<Discovery> {
        unreachable!()
    }
    fn status_changed(&mut self, _: Status) {
        unreachable!()
    }
    fn log(&mut self, _: LogRecord) {
        unreachable!()
    }
    fn gap(&mut self, _: Gap) {
        unreachable!()
    }
    fn history_range(&mut self, _: Empty) -> RpcResult<HistoryRange> {
        Ok(HistoryRange {
            journal_id: "j".into(),
            oldest: DecimalU64(4),
            high_water: DecimalU64(9),
        })
    }
    fn history_read(&mut self, params: HistoryRead) -> RpcResult<HistoryPage> {
        assert_eq!(
            params,
            HistoryRead {
                after: Some(HistoryCursor {
                    journal_id: "j".into(),
                    position: DecimalU64(3)
                }),
                through: Some(DecimalU64(9)),
                limit: 2
            }
        );
        Ok(HistoryPage {
            range: self.history_range(Empty {})?,
            records: vec![],
            missing: vec![HistoryMissing {
                first: DecimalU64(4),
                last: DecimalU64(9),
                reason: "retention_expired".into(),
            }],
            next: HistoryCursor {
                journal_id: "j".into(),
                position: DecimalU64(9),
            },
        })
    }
    fn history_ack(&mut self, params: HistoryAck) -> RpcResult<HistoryAck> {
        Ok(params)
    }
}

#[test]
fn history_requests_decode_and_dispatch_typed_results() {
    for (method, params, result) in [
        (
            "history.range",
            json!({}),
            json!({"journal_id":"j","oldest":"4","high_water":"9"}),
        ),
        (
            "history.read",
            json!({"after":{"journal_id":"j","position":"3"},"through":"9","limit":2}),
            json!({
                "range":{"journal_id":"j","oldest":"4","high_water":"9"},"records":[],
                "missing":[{"first":"4","last":"9","reason":"retention_expired"}],"next":{"journal_id":"j","position":"9"}
            }),
        ),
        (
            "history.ack",
            json!({"cursor":{"journal_id":"j","position":"9"}}),
            json!({"cursor":{"journal_id":"j","position":"9"}}),
        ),
    ] {
        let request =
            decode(&json!({"jsonrpc":"2.0","id":"h","method":method,"params":params}).to_string())
                .unwrap();
        assert_eq!(request.receiver(), Some(Receiver::Guest));
        assert_eq!(
            serde_json::to_value(request.dispatch(&mut HistoryHandler).unwrap().unwrap()).unwrap(),
            json!({"jsonrpc":"2.0","id":"h","result":result})
        );
    }
    let incoming =
        decode(r#"{"jsonrpc":"2.0","id":"h","method":"history.read","params":{"limit":1}}"#)
            .unwrap();
    assert_eq!(
        incoming,
        Incoming::HistoryRead {
            id: "h".into(),
            params: HistoryRead {
                after: None,
                through: None,
                limit: 1
            }
        }
    );
}

#[cfg(feature = "schema")]
#[test]
fn discovery_negotiates_history_on_each_connection() {
    let mut peer = PeerProtocol::default();
    peer.accept_endpoint(EndpointInfo::new(EndpointRole::Guest, "test"))
        .unwrap();
    let mut discovery = schema::document(Receiver::Guest);
    peer.accept_discovery(discovery.clone()).unwrap();
    for method in [
        Method::HistoryRange,
        Method::HistoryRead,
        Method::HistoryAck,
    ] {
        assert_eq!(peer.require(method), Ok(()));
    }
    discovery
        .methods
        .retain(|method| !method.name.starts_with("history."));
    peer.accept_discovery(discovery).unwrap();
    assert_eq!(
        peer.require(Method::HistoryRead),
        Err(RpcError::new(-32002, "unsupported"))
    );
    assert_eq!(peer.require(Method::StatusGet), Ok(()));
}
