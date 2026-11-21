static NEXT_PIPE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub(super) fn field_follow(state: &Shared) -> Result<(), Box<dyn std::error::Error>> {
    let mut owner = PipeWorkers::new();
    field_panel_loop(state, &mut owner)
}

fn field_connect(
    state: &Shared,
    owner: &mut PipeWorkers,
    client_rpc: venera_debug_protocol::client::Client,
) -> io::Result<(venera_debug_protocol::client::Client, ClientPipe)> {
    let (server, mut file) = helper_pair();
    let state = state.clone();
    let stop = owner.local_stop.clone();
    owner.reap_finished();
    owner.threads.push(std::thread::spawn(move || {
        let _ = client(server, state, &mut false, &stop);
    }));
    let client_rpc = rpc_negotiate_with(&mut file, client_rpc)?;
    Ok((client_rpc, file))
}

fn field_pipe_check(
    client: &mut venera_debug_protocol::client::Client,
    file: &mut ClientPipe,
    through: u64,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use venera_debug_protocol::{
        client::{HistoryConsumer, Start},
        v1::*,
    };
    assert_eq!(client.history_encoding(), HistoryEncoding::Attribution);
    let range: HistoryRange = serde_json::from_value(rpc_call(
        client,
        file,
        Method::HistoryRange,
        serde_json::json!({}),
    )?)?;
    assert_eq!(range.high_water.0, through);
    assert!(range.journal_id.starts_with("field-attribution-"));
    let mut consumer = HistoryConsumer::new(Start::Beginning);
    consumer.range(range, None).map_err(|e| e.message)?;
    let page: AttributionHistoryPage = serde_json::from_value(rpc_call(
        client,
        file,
        Method::HistoryRead,
        serde_json::to_value(consumer.read_request())?,
    )?)?;
    consumer.attribution_page(&page).map_err(|e| e.message)?;
    assert_eq!(page.records.len(), through as usize);
    assert!(page.missing.is_empty());
    for (i, (text, key)) in [
        ("A: before PID reuse", None),
        ("B: after PID reuse", Some("4202")),
    ]
    .into_iter()
    .enumerate()
    {
        let item = &page.records[i];
        assert_eq!(item.cursor.position.0, i as u64 + 1);
        let AttributionHistoryEvent::Log { record } = &item.event else {
            panic!("missing fixture log")
        };
        assert_eq!(record.text, text);
        let process = record.process.as_ref().unwrap();
        assert_eq!(process.pid, 42);
        assert_eq!(process.start_key.as_deref(), key);
    }
    if through == 3 {
        let AttributionHistoryEvent::AttributionRevision {
            target,
            attribution,
        } = &page.records[2].event
        else {
            panic!("missing revision")
        };
        assert_eq!(target, &page.records[0].cursor);
        assert_eq!(attribution.process.start_key.as_deref(), Some("4201"));
    }
    let ack = consumer.consumed().map_err(|e| e.message)?;
    let sent = serde_json::to_value(ack)?;
    let received = rpc_call(client, file, Method::HistoryAck, sent.clone())?;
    assert_eq!(sent, received);
    Ok(format!(
        "{}\n{}\n",
        serde_json::to_string(&page)?,
        serde_json::json!({"pipe_ack": received})
    )
    .into_bytes())
}
