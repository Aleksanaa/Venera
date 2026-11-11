use venera_debug_protocol::*;

const VECTORS: &str = include_str!("vectors.jsonl");

#[test]
fn panel_report_rows_preserve_warning_text_and_findings() {
    let report: serde_json::Value =
        serde_json::from_str(include_str!("panel-report.json")).unwrap();
    let sections: Vec<SystemInfoSection> =
        serde_json::from_value(report["sections"].clone()).unwrap();
    assert_eq!(
        sections,
        vec![SystemInfoSection {
            title: "Host".into(),
            rows: vec![
                SystemInfoRow {
                    key: "GPU".into(),
                    value: "Unknown: Venus did not answer".into(),
                    bad: true,
                    warning: "请检查图形驱动。\n重新启动 Windows 后重试。".into()
                },
                SystemInfoRow {
                    key: "Renderer".into(),
                    value: "Venus".into(),
                    bad: false,
                    warning: "".into()
                },
            ],
        }]
    );
    assert_eq!(serde_json::json!({"sections": sections}), report);
}

#[test]
fn shared_schema_cases_decode_to_literal_fields() {
    for line in include_str!("valid.jsonl").lines() {
        let case: serde_json::Value = serde_json::from_str(line).unwrap();
        // Extract the original input spelling so 1.0 also exercises the wire decoder.
        let input = line
            .strip_prefix("{\"input\":")
            .unwrap()
            .split(",\"expected\":")
            .next()
            .unwrap();
        let decoded = decode(input.as_bytes()).unwrap();
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            case["expected"],
            "{input}"
        );
        assert_eq!(serde_json::from_str::<Envelope>(input).unwrap(), decoded);
        assert_eq!(decode(&encode(&decoded).unwrap()).unwrap(), decoded);
    }
    for line in include_str!("invalid.jsonl").lines() {
        assert!(decode(line.as_bytes()).is_err(), "{line}");
    }
}

fn expected_vectors() -> Vec<Envelope> {
    vec![
        Envelope::new(Message::Hello {
            role: "guest".into(),
            software_version: "0.1.0".into(),
            capabilities: vec!["status".into(), "filter".into(), "gap".into()],
        }),
        Envelope::new(Message::StatusGet {
            request_id: "host-1".into(),
        }),
        Envelope::new(Message::Status {
            status: Status {
                forwarding_enabled: true,
                revision: 0,
                effective_filter: Filter::default(),
                expires_at: None,
                health: serde_json::json!({"port": "present"}),
                loss: serde_json::json!({"queue_dropped": 0}),
            },
        }),
        Envelope::new(Message::FilterSet {
            request_id: "host-2".into(),
            duration_ms: Some(60000),
            filter: Filter {
                components: vec!["dxvk".into()],
                levels: vec!["warning".into()],
                targets: vec![
                    Target::ProcessInstance {
                        pid: 4120,
                        start_time_filetime: "134124000000000000".into(),
                        include_descendants: true,
                    },
                    Target::ExePath {
                        path: "C:\\Games\\demo.exe".into(),
                    },
                ],
            },
        }),
        Envelope::new(Message::RequestResult {
            result: RequestResult {
                request_id: "host-2".into(),
                operation: "filter.set".into(),
                ok: true,
                revision: Some(1),
                effective_filter: Some(Filter {
                    components: vec!["dxvk".into()],
                    levels: vec!["warning".into()],
                    targets: vec![],
                }),
                expires_at: Some("2026-11-10T08:01:00Z".into()),
                forwarding_enabled: Some(true),
                status: None,
                error: None,
            },
        }),
        Envelope::new(Message::Log {
            record: LogRecord {
                stream_id: "guest-run-1".into(),
                seq: 42,
                observed_at: "2026-11-10T08:00:01.123Z".into(),
                source_time: None,
                source: "dbwin".into(),
                component: "dxvk".into(),
                level: "warning".into(),
                raw_level: "warn".into(),
                session_id: Some(2),
                process: Some(ProcessIdentity {
                    pid: 4120,
                    start_time_filetime: Some("134124000000000000".into()),
                    exe_path: Some("C:\\Games\\demo.exe".into()),
                }),
                tid: None,
                text: "中文警告\n下一行".into(),
            },
        }),
        Envelope::new(Message::Gap {
            stream_id: "guest-run-1".into(),
            reason: "queue_overflow".into(),
            count: Some(2),
            first_seq: Some(43),
            last_seq: Some(44),
        }),
        Envelope::new(Message::FilterReset {
            request_id: "host-3".into(),
        }),
        Envelope::new(Message::RequestResult {
            result: RequestResult {
                request_id: "host-3".into(),
                operation: "filter.reset".into(),
                ok: false,
                revision: None,
                effective_filter: None,
                expires_at: None,
                forwarding_enabled: None,
                status: None,
                error: Some(RequestError {
                    code: "busy".into(),
                    message: "Try again".into(),
                }),
            },
        }),
        Envelope::new(Message::SystemInfoGet {
            request_id: "sys-1".into(),
        }),
        Envelope::new(Message::SystemInfoResult {
            result: SystemInfoResult {
                request_id: "sys-1".into(),
                ok: true,
                collected_at: "2026-11-11T00:00:00Z".into(),
                session_id: Some(1),
                sections: vec![SystemInfoSection {
                    title: "Graphics".into(),
                    rows: vec![SystemInfoRow {
                        key: "Adapter".into(),
                        value: "Unavailable".into(),
                        bad: true,
                        warning: "请检查图形驱动。\n重新启动 Windows 后重试。".into(),
                    }],
                }],
                error: None,
            },
        }),
        Envelope::new(Message::SystemInfoResult {
            result: SystemInfoResult {
                request_id: "sys-2".into(),
                ok: false,
                collected_at: "2026-11-11T00:00:00Z".into(),
                session_id: None,
                sections: vec![],
                error: Some(RequestError {
                    code: "timeout".into(),
                    message: "System report collection timed out".into(),
                }),
            },
        }),
    ]
}

#[test]
fn vectors_roundtrip_and_every_byte_split() {
    let expected = expected_vectors();
    assert_eq!(VECTORS.lines().count(), expected.len());
    for (line, expected) in VECTORS.lines().zip(expected) {
        assert_eq!(decode(line.as_bytes()), Ok(expected.clone()));
        assert_eq!(decode(&encode(&expected).unwrap()).unwrap(), expected);
        let wire = format!("{line}\n");
        for split in 0..=wire.len() {
            let mut parser = Framer::new(4096);
            let mut result = parser.push(&wire.as_bytes()[..split]);
            result.extend(parser.push(&wire.as_bytes()[split..]));
            assert_eq!(result, vec![Ok(expected.clone())]);
        }
    }
    let Message::Log { record } = decode(VECTORS.lines().nth(5).unwrap().as_bytes())
        .unwrap()
        .body
    else {
        panic!("log")
    };
    assert_eq!(record.text, "中文警告\n下一行");
    assert_eq!(record.process.unwrap().pid, 4120);
}

#[test]
fn recovery_and_compatibility() {
    let good = b"{\"major\":1,\"type\":\"status.get\",\"request_id\":\"x\",\"future\":true}\n";
    let expected = Envelope::new(Message::StatusGet {
        request_id: "x".into(),
    });
    let mut parser = Framer::new(100);
    let mut bytes = b"\npartial\n\xff\n".to_vec();
    bytes.extend(vec![b'x'; 101]);
    bytes.push(b'\n');
    bytes.extend(good);
    let results = parser.push(&bytes);
    assert!(matches!(&results[0], Err(DecodeError::Json(_))));
    assert_eq!(
        &results[1..],
        &[
            Err(DecodeError::Utf8),
            Err(DecodeError::TooLong),
            Ok(expected.clone())
        ]
    );
    assert_eq!(
        decode(b"{\"major\":1,\"type\":\"future\"}"),
        Err(DecodeError::UnknownType("future".into()))
    );
    assert_eq!(
        decode(b"{\"major\":2,\"type\":\"status.get\",\"request_id\":\"x\"}"),
        Err(DecodeError::Major(2))
    );
    parser.push(b"partial");
    parser.reset();
    assert_eq!(parser.push(good), vec![Ok(expected)]);
}

#[test]
fn filters_intersect_dimensions_and_use_process_instances() {
    let Message::Log { mut record } = decode(VECTORS.lines().nth(5).unwrap().as_bytes())
        .unwrap()
        .body
    else {
        panic!("log")
    };
    assert!(matches_filter(&Filter::default(), &record, &[]));
    let mut filter = Filter {
        components: vec!["dxvk".into()],
        levels: vec!["warning".into()],
        targets: vec![Target::ProcessInstance {
            pid: 4120,
            start_time_filetime: "134124000000000000".into(),
            include_descendants: false,
        }],
    };
    assert!(matches_filter(&filter, &record, &[]));
    filter.components = vec!["vkd3d-proton".into()];
    assert!(!matches_filter(&filter, &record, &[]));
    filter.components = vec!["dxvk".into()];
    record.process.as_mut().unwrap().start_time_filetime = Some("134124000000000001".into());
    assert!(!matches_filter(&filter, &record, &[]));
    filter.targets = vec![Target::ExePath {
        path: "c:\\games\\DEMO.exe".into(),
    }];
    assert!(matches_filter(&filter, &record, &[]));
    filter.levels = vec!["error".into()];
    assert!(!matches_filter(&filter, &record, &[]));
    filter.levels.clear();
    filter.targets = vec![Target::ProcessInstance {
        pid: 1,
        start_time_filetime: "10".into(),
        include_descendants: true,
    }];
    assert!(matches_filter(
        &filter,
        &record,
        &[ProcessIdentity {
            pid: 1,
            start_time_filetime: Some("10".into()),
            exe_path: None
        }]
    ));
    if let Target::ProcessInstance {
        include_descendants,
        ..
    } = &mut filter.targets[0]
    {
        *include_descendants = false;
    }
    assert!(!matches_filter(
        &filter,
        &record,
        &[ProcessIdentity {
            pid: 1,
            start_time_filetime: Some("10".into()),
            exe_path: None
        }]
    ));
    record.process = None;
    assert!(!matches_filter(&filter, &record, &[]));
}
