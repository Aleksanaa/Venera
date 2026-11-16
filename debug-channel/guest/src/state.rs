use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::mpsc::SyncSender,
    time::{Duration, Instant},
};
use venera_debug_protocol::{
    Envelope, Filter, Message, RequestError, RequestResult, Status, Target,
};

pub(crate) mod wire {
    use super::*;
    use serde_json::{json, Value};
    use venera_debug_protocol::v1;

    pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, serde_json::Error> {
        let value = match &envelope.body {
            Message::Status { status } => {
                json!({"jsonrpc":"2.0","method":"status.changed","params": status_v1(status)?})
            }
            Message::Log { record } => {
                json!({"jsonrpc":"2.0","method":"log","params": log_v1(record)?})
            }
            Message::Gap {
                stream_id,
                reason,
                count,
                first_seq,
                last_seq,
            } => json!({
                "jsonrpc":"2.0","method":"gap","params": v1::Gap { stream_id: stream_id.clone(), reason: reason.clone(), count: count.map(v1::DecimalU64), first_seq: first_seq.map(v1::DecimalU64), last_seq: last_seq.map(v1::DecimalU64) }
            }),
            Message::RequestResult { result } => response(result)?,
            Message::SystemInfoResult { result } => system_response(result),
            _ => {
                return Err(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsupported remote message",
                )))
            }
        };
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn encoding_error(message: &str) -> serde_json::Error {
        <serde_json::Error as serde::ser::Error>::custom(message)
    }

    fn status_v1(status: &Status) -> Result<v1::Status, serde_json::Error> {
        let mut health = status.health.clone();
        if let Some(sessions) = health
            .get_mut("session_helper_loss")
            .and_then(Value::as_object_mut)
        {
            for session in sessions.values_mut() {
                decimal_counter(session, "dropped_total")?;
                decimal_counter(session, "reported_total")?;
            }
        }
        let mut loss = status.loss.clone();
        for key in [
            "queue_dropped",
            "parse_errors",
            "process_event_parse_errors",
            "process_capture_queue_dropped",
        ] {
            decimal_counter(&mut loss, key)?;
        }
        Ok(v1::Status {
            forwarding_enabled: status.forwarding_enabled,
            revision: v1::DecimalU64(status.revision),
            effective_filter: filter_v1(&status.effective_filter)?,
            expires_at: status.expires_at.clone(),
            health,
            loss,
        })
    }

    fn decimal_counter(object: &mut Value, key: &str) -> Result<(), serde_json::Error> {
        if let Some(value) = object.get_mut(key) {
            let count = value
                .as_u64()
                .ok_or_else(|| encoding_error("invalid internal status counter"))?;
            *value = json!(v1::DecimalU64(count));
        }
        Ok(())
    }

    fn filter_v1(filter: &Filter) -> Result<v1::Filter, serde_json::Error> {
        Ok(v1::Filter {
            components: filter.components.clone(),
            levels: filter.levels.clone(),
            targets: filter
                .targets
                .iter()
                .map(target_v1)
                .collect::<Result<_, _>>()?,
        })
    }
    pub(crate) fn filter_legacy(filter: v1::Filter) -> Filter {
        Filter {
            components: filter.components,
            levels: filter.levels,
            targets: filter.targets.into_iter().map(target_legacy).collect(),
        }
    }
    fn filetime_v1(value: &str) -> Result<v1::DecimalU64, serde_json::Error> {
        value
            .parse()
            .map_err(|_| encoding_error("invalid internal FILETIME"))
    }

    fn target_v1(target: &Target) -> Result<v1::Target, serde_json::Error> {
        Ok(match target {
            Target::ExePath { path } => v1::Target::ExePath { path: path.clone() },
            Target::ProcessInstance {
                pid,
                start_time_filetime,
                include_descendants,
            } => v1::Target::ProcessInstance {
                pid: *pid,
                start_time_filetime: filetime_v1(start_time_filetime)?,
                include_descendants: *include_descendants,
            },
        })
    }
    fn target_legacy(target: v1::Target) -> Target {
        match target {
            v1::Target::ExePath { path } => Target::ExePath { path },
            v1::Target::ProcessInstance {
                pid,
                start_time_filetime,
                include_descendants,
            } => Target::ProcessInstance {
                pid,
                start_time_filetime: start_time_filetime.0.to_string(),
                include_descendants,
            },
        }
    }
    fn identity_v1(
        identity: &venera_debug_protocol::ProcessIdentity,
    ) -> Result<v1::ProcessIdentity, serde_json::Error> {
        Ok(v1::ProcessIdentity {
            pid: identity.pid,
            start_time_filetime: identity
                .start_time_filetime
                .as_deref()
                .map(filetime_v1)
                .transpose()?,
            exe_path: identity.exe_path.clone(),
        })
    }

    fn log_v1(
        record: &venera_debug_protocol::LogRecord,
    ) -> Result<v1::LogRecord, serde_json::Error> {
        Ok(v1::LogRecord {
            stream_id: record.stream_id.clone(),
            seq: v1::DecimalU64(record.seq),
            observed_at: record.observed_at.clone(),
            source_time: record.source_time.clone(),
            source: record.source.clone(),
            component: record.component.clone(),
            level: record.level.clone(),
            raw_level: record.raw_level.clone(),
            session_id: record.session_id,
            process: record.process.as_ref().map(identity_v1).transpose()?,
            tid: record.tid,
            text: record.text.clone(),
        })
    }

    fn response(result: &RequestResult) -> Result<Value, serde_json::Error> {
        Ok(if result.ok {
            let status = result.status.as_ref().ok_or_else(|| {
                encoding_error("successful remote response requires a status snapshot")
            })?;
            json!({"jsonrpc":"2.0","id":result.request_id,"result":status_v1(status)?})
        } else {
            json!({"jsonrpc":"2.0","id":result.request_id,"error":{"code":-32000,"message":result.error.as_ref().map(|e| e.message.clone()).unwrap_or_else(|| "Request failed".into()),"data":result.error}})
        })
    }
    fn system_response(result: &venera_debug_protocol::SystemInfoResult) -> Value {
        if result.ok {
            json!({"jsonrpc":"2.0","id":result.request_id,"result":v1::SystemInfo {
                collected_at: result.collected_at.clone(), session_id: result.session_id,
                sections: result.sections.iter().map(|section| v1::SystemInfoSection {
                    title: section.title.clone(), title_message: section.title_message.as_ref().map(localized),
                    rows: section.rows.iter().map(|row| v1::SystemInfoRow {
                        key: row.key.clone(), value: row.value.clone(), bad: row.bad, warning: row.warning.clone(),
                        key_message: row.key_message.as_ref().map(localized),
                        value_message: row.value_message.as_ref().map(localized),
                        warning_message: row.warning_message.as_ref().map(localized),
                    }).collect()
                }).collect()
            }})
        } else {
            json!({"jsonrpc":"2.0","id":result.request_id,"error":{"code":-32000,"message":result.error.as_ref().map(|e| e.message.clone()).unwrap_or_else(|| "System report failed".into()),"data":result.error}})
        }
    }
    fn localized(message: &venera_debug_protocol::LocalizedMessage) -> v1::LocalizedMessage {
        v1::LocalizedMessage {
            id: message.id.clone(),
            args: message.args.clone(),
            diagnostic: message.diagnostic.clone(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn filter_results_encode_current_health_and_loss() {
            let mut state = State::new("run-1".into(), true);
            state.add_peer();
            state.status.health = json!({"port":"connected","session_helper_loss":{"2":{"pid":10852,"dropped_total":18446744073709551615u64,"reported_total":9}}});
            state.status.loss = json!({"process_event_parse_errors":7,"process_capture_queue_dropped":8,"etw_events_lost":4,"process_tracking_overflow":true});
            state.dropped = u64::MAX;
            state.parse_errors = 3;
            for (request, revision) in [
                (
                    Message::FilterSet {
                        request_id: "set".into(),
                        filter: Filter::default(),
                        duration_ms: None,
                    },
                    "1",
                ),
                (
                    Message::FilterReset {
                        request_id: "reset".into(),
                    },
                    "2",
                ),
            ] {
                let response = state.control(request, None).unwrap();
                // Exercise the queued representation used by remote peers.
                state.queue_control(&response);
                let queued = state
                    .peers
                    .get_mut(&1)
                    .unwrap()
                    .controls
                    .pop_back()
                    .unwrap();
                let response = venera_debug_protocol::decode(&queued).unwrap();
                let value: Value = serde_json::from_slice(&encode(&response).unwrap()).unwrap();
                assert_eq!(
                    value["result"],
                    json!({
                        "forwarding_enabled":true,"revision":revision,"effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,
                        "health":{"port":"connected","session_helper_loss":{"2":{"pid":10852,"dropped_total":"18446744073709551615","reported_total":"9"}}},
                        "loss":{"queue_dropped":"18446744073709551615","parse_errors":"3","process_event_parse_errors":"7","process_capture_queue_dropped":"8","etw_events_lost":4,"process_tracking_overflow":true}
                    })
                );
                let local = serde_json::to_value(response).unwrap();
                assert_eq!(
                    local["status"]["loss"]["queue_dropped"],
                    json!(18446744073709551615u64)
                );
                assert_eq!(
                    local["status"]["health"]["session_helper_loss"]["2"]["dropped_total"],
                    json!(18446744073709551615u64)
                );
            }
        }

        #[test]
        fn successful_results_require_a_status_snapshot() {
            let mut state = State::new("run-1".into(), true);
            let mut response = state.result("set".into(), "filter.set");
            if let Message::RequestResult { result } = &mut response.body {
                result.status = None;
            }
            assert_eq!(
                encode(&response).unwrap_err().to_string(),
                "successful remote response requires a status snapshot"
            );
        }

        #[test]
        fn status_notifications_encode_known_counters_and_preserve_other_values() {
            let mut state = State::new("run-1".into(), true);
            state.status.health = json!({"sessions":[2],"session_helper_loss":{"2":{"pid":4294967295u32,"dropped_total":4,"reported_total":3}},"vendor":{"value":42}});
            state.dropped = 5;
            state.parse_errors = 6;
            let envelope = Envelope::new(Message::Status {
                status: state.current(),
            });
            let value: Value = serde_json::from_slice(&encode(&envelope).unwrap()).unwrap();
            assert_eq!(
                value,
                json!({"jsonrpc":"2.0","method":"status.changed","params":{
                    "forwarding_enabled":true,"revision":"0","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,
                    "health":{"sessions":[2],"session_helper_loss":{"2":{"pid":4294967295u32,"dropped_total":"4","reported_total":"3"}},"vendor":{"value":42}},
                    "loss":{"queue_dropped":"5","parse_errors":"6"}
                }})
            );
            let mut invalid = envelope;
            if let Message::Status { status } = &mut invalid.body {
                status.loss["queue_dropped"] = json!(-1);
            }
            assert_eq!(
                encode(&invalid).unwrap_err().to_string(),
                "invalid internal status counter"
            );
        }

        #[test]
        fn filetime_encoding_preserves_full_range_and_reports_invalid_values() {
            let mut state = State::new("run-1".into(), true);
            for (input, expected) in [("0", "0"), ("18446744073709551615", "18446744073709551615")]
            {
                state.status.effective_filter.targets = vec![Target::ProcessInstance {
                    pid: 7,
                    start_time_filetime: input.into(),
                    include_descendants: true,
                }];
                let value: Value = serde_json::from_slice(
                    &encode(&Envelope::new(Message::Status {
                        status: state.current(),
                    }))
                    .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    value["params"]["effective_filter"]["targets"][0],
                    json!({"kind":"process_instance","pid":7,"start_time_filetime":expected,"include_descendants":true})
                );
            }
            for input in ["yesterday", "18446744073709551616", "01", "+1"] {
                state.status.effective_filter.targets = vec![Target::ProcessInstance {
                    pid: 7,
                    start_time_filetime: input.into(),
                    include_descendants: true,
                }];
                assert_eq!(
                    encode(&Envelope::new(Message::Status {
                        status: state.current()
                    }))
                    .unwrap_err()
                    .to_string(),
                    "invalid internal FILETIME"
                );
            }
            let mut envelope = Envelope::new(Message::Log {
                record: venera_debug_protocol::LogRecord {
                    stream_id: "stream-1".into(),
                    seq: 9,
                    observed_at: "2026-11-11T00:00:00Z".into(),
                    source_time: None,
                    source: "dbwin".into(),
                    component: "venus".into(),
                    level: "info".into(),
                    raw_level: "info".into(),
                    session_id: Some(2),
                    process: Some(venera_debug_protocol::ProcessIdentity {
                        pid: 7,
                        start_time_filetime: None,
                        exe_path: None,
                    }),
                    tid: Some(8),
                    text: "ready".into(),
                },
            });
            for (input, expected) in [
                (None, Value::Null),
                (Some("18446744073709551615"), json!("18446744073709551615")),
            ] {
                if let Message::Log { record } = &mut envelope.body {
                    record.process.as_mut().unwrap().start_time_filetime = input.map(str::to_owned);
                }
                let value: Value = serde_json::from_slice(&encode(&envelope).unwrap()).unwrap();
                assert_eq!(
                    value["params"]["process"],
                    json!({"pid":7,"start_time_filetime":expected,"exe_path":null})
                );
            }
            if let Message::Log { record } = &mut envelope.body {
                record.process.as_mut().unwrap().start_time_filetime = Some("yesterday".into());
            }
            assert_eq!(
                encode(&envelope).unwrap_err().to_string(),
                "invalid internal FILETIME"
            );
        }

        #[test]
        fn v1_status_request_and_result_preserve_string_correlation() {
            let response = encode(&Envelope::new(Message::RequestResult {
                result: RequestResult {
                    request_id: "status-7".into(),
                    operation: "status.get".into(),
                    ok: true,
                    revision: Some(3),
                    effective_filter: Some(Filter::default()),
                    expires_at: None,
                    forwarding_enabled: Some(true),
                    status: Some(Status {
                        forwarding_enabled: true,
                        revision: 3,
                        effective_filter: Filter::default(),
                        expires_at: None,
                        health: json!({"port":"connected"}),
                        loss: json!({}),
                    }),
                    error: None,
                },
            }))
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&response).unwrap(),
                json!({
                "jsonrpc":"2.0","id":"status-7","result":{"forwarding_enabled":true,"revision":"3","effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{"port":"connected"},"loss":{}
            } })
            );
        }

        #[test]
        fn v1_notifications_use_method_names_and_decimal_sequences() {
            let log = encode(&Envelope::new(Message::Log {
                record: venera_debug_protocol::LogRecord {
                    stream_id: "stream-1".into(),
                    seq: 9,
                    observed_at: "2026-11-11T00:00:00Z".into(),
                    source_time: None,
                    source: "dbwin".into(),
                    component: "venus".into(),
                    level: "info".into(),
                    raw_level: "info".into(),
                    session_id: Some(u32::MAX),
                    process: None,
                    tid: Some(7),
                    text: "ready".into(),
                },
            }))
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&log).unwrap(),
                json!({
                    "jsonrpc":"2.0","method":"log","params":{"stream_id":"stream-1","seq":"9","observed_at":"2026-11-11T00:00:00Z","source_time":null,"source":"dbwin","component":"venus","level":"info","raw_level":"info","session_id":4294967295u64,"process":null,"tid":7,"text":"ready"}
                })
            );
            let gap = encode(&Envelope::new(Message::Gap {
                stream_id: "stream-1".into(),
                reason: "transport_interrupted".into(),
                count: Some(2),
                first_seq: Some(8),
                last_seq: Some(9),
            }))
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&gap).unwrap(),
                json!({
                    "jsonrpc":"2.0","method":"gap","params":{"stream_id":"stream-1","reason":"transport_interrupted","count":"2","first_seq":"8","last_seq":"9"}
                })
            );
        }
    }
}

pub const LOG_RECORDS: usize = 2048;
const LOG_BYTES: usize = 8 * 1024 * 1024;
const CONTROL_RECORDS: usize = 64;

pub struct State {
    pub status: Status,
    forwarding_generation: u64,
    pub stream_id: String,
    pub sequence: u64,
    local_sequence: u64,
    deadline: Option<Instant>,
    pub filter_started_filetime: u64,
    pub peers: BTreeMap<u64, PeerQueue>,
    next_peer: u64,
    pub subscribers: Vec<SyncSender<Vec<u8>>>,
    // Counts discarded deliveries across all peer queues, rather than unique captures.
    pub dropped: u64,
    pub parse_errors: u64,
    #[cfg(windows)]
    pub helpers: std::collections::BTreeMap<u32, venera_debug_protocol::ProcessIdentity>,
    #[cfg(windows)]
    pub processes: crate::ancestry::Processes,
    #[cfg(windows)]
    pub assembler: crate::collector::Assembler,
    #[cfg(windows)]
    pub pending_captures: VecDeque<(Instant, crate::collector::Capture)>,
}
impl State {
    pub fn new(stream_id: String, forwarding_enabled: bool) -> Self {
        Self {
            status: Status {
                forwarding_enabled,
                revision: 0,
                effective_filter: Filter::default(),
                expires_at: None,
                health: json!({"driver":"probing","controller":"probing","port":"waiting","host":"waiting","dbwin":"starting","etw":"starting","sessions":[]}),
                loss: json!({"queue_dropped":0,"parse_errors":0}),
            },
            stream_id,
            forwarding_generation: 0,
            sequence: 0,
            local_sequence: 0,
            deadline: None,
            filter_started_filetime: 0,
            peers: BTreeMap::new(),
            next_peer: 0,
            subscribers: Vec::new(),
            dropped: 0,
            parse_errors: 0,
            #[cfg(windows)]
            helpers: Default::default(),
            #[cfg(windows)]
            processes: Default::default(),
            #[cfg(windows)]
            assembler: Default::default(),
            #[cfg(windows)]
            pending_captures: Default::default(),
        }
    }
    pub fn current(&mut self) -> Status {
        self.status.loss["queue_dropped"] = json!(self.dropped);
        self.status.loss["parse_errors"] = json!(self.parse_errors);
        self.status.clone()
    }
    pub fn notify(&mut self) {
        let status = self.current();
        let bytes =
            venera_debug_protocol::encode(&Envelope::new(Message::Status { status })).unwrap();
        self.broadcast(bytes.clone());
        for peer in self.peers.values_mut() {
            if peer.closed {
                continue;
            }
            if peer.control_room() {
                peer.controls.push_back(bytes.clone());
            } else {
                peer.closed = true;
            }
        }
    }
    pub fn broadcast(&mut self, bytes: Vec<u8>) {
        self.subscribers
            .retain(|subscriber| subscriber.try_send(bytes.clone()).is_ok());
    }
    pub fn publish_capture(
        &mut self,
        capture: crate::collector::Capture,
        class: Option<crate::collector::Classification>,
        ancestors: &[venera_debug_protocol::ProcessIdentity],
    ) {
        let class = class.unwrap_or_else(|| crate::collector::Classification {
            component: String::new(),
            level: "unknown".into(),
            raw_level: "unknown".into(),
            tid: None,
        });
        if class.component == "probe" {
            return;
        }
        self.local_sequence += 1;
        let record = venera_debug_protocol::LogRecord {
            stream_id: self.stream_id.clone(),
            seq: self.local_sequence,
            observed_at: capture.observed_at,
            source_time: capture.source_time,
            source: capture.source,
            component: class.component,
            level: class.level,
            raw_level: capture.raw_level.unwrap_or(class.raw_level),
            session_id: capture.session_id,
            process: capture.process,
            tid: capture.tid.or(class.tid),
            text: capture.text,
        };
        let forward = venera_debug_protocol::matches_filter(
            &self.status.effective_filter,
            &record,
            ancestors,
        );
        let mut envelope = Envelope::new(Message::Log { record });
        let bytes = venera_debug_protocol::encode(&envelope).unwrap();
        if bytes.len() <= venera_debug_protocol::DEFAULT_MAX_LINE_BYTES {
            self.broadcast(bytes);
        }
        if !forward {
            return;
        }
        self.sequence += 1;
        if let Message::Log { record } = &mut envelope.body {
            record.seq = self.sequence;
        }
        let bytes = venera_debug_protocol::encode(&envelope).unwrap();
        if bytes.len() > venera_debug_protocol::DEFAULT_MAX_LINE_BYTES {
            let gap = Envelope::new(Message::Gap {
                stream_id: self.stream_id.clone(),
                reason: "record_size_exceeded".into(),
                count: Some(1),
                first_seq: Some(self.sequence),
                last_seq: Some(self.sequence),
            });
            self.enqueue_log(self.sequence, venera_debug_protocol::encode(&gap).unwrap());
            return;
        }
        self.enqueue_log(self.sequence, bytes);
    }
    pub fn set_forwarding(&mut self, enabled: bool) {
        if !enabled {
            self.forwarding_generation += 1;
            self.clear_logs();
        }
        self.status.forwarding_enabled = enabled;
        self.notify();
    }
    pub fn write_allowed(&self, generation: Option<u64>) -> bool {
        generation.is_none_or(|generation| {
            self.status.forwarding_enabled && generation == self.forwarding_generation
        })
    }
    pub fn apply(&mut self, filter: Filter, duration_ms: Option<u64>, expires_at: Option<String>) {
        self.status.effective_filter = filter;
        self.status.revision += 1;
        self.status.expires_at = expires_at;
        self.deadline =
            duration_ms.and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)));
        self.filter_started_filetime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .saturating_div(100) as u64
            + 116444736000000000;
        self.notify();
    }
    pub fn expire(&mut self) {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.apply(Filter::default(), None, None);
        }
    }
    pub fn result(&mut self, request_id: String, operation: &str) -> Envelope {
        Envelope::new(Message::RequestResult {
            result: RequestResult {
                request_id,
                operation: operation.into(),
                ok: true,
                revision: Some(self.status.revision),
                effective_filter: Some(self.status.effective_filter.clone()),
                expires_at: self.status.expires_at.clone(),
                forwarding_enabled: Some(self.status.forwarding_enabled),
                status: Some(self.current()),
                error: None,
            },
        })
    }
    pub fn control(&mut self, message: Message, expires_at: Option<String>) -> Option<Envelope> {
        self.expire();
        match message {
            Message::StatusGet { request_id } => Some(self.result(request_id, "status.get")),
            Message::FilterSet {
                request_id,
                mut filter,
                duration_ms,
            } => {
                for target in &mut filter.targets {
                    if let Target::ExePath { path } = target {
                        *path = path.replace('/', "\\");
                    }
                }
                let invalid = duration_ms.is_some_and(|ms| {
                    Instant::now()
                        .checked_add(Duration::from_millis(ms))
                        .is_none()
                }) || filter.targets.iter().any(|target| match target {
                    Target::ProcessInstance {
                        start_time_filetime,
                        ..
                    } => start_time_filetime.parse::<u64>().is_err(),
                    Target::ExePath { path } => {
                        !(path.starts_with("\\\\")
                            || (path.as_bytes().get(1) == Some(&b':')
                                && path.as_bytes().get(2) == Some(&b'\\')))
                            || path.contains('\0')
                    }
                });
                if invalid {
                    let mut response = self.result(request_id, "filter.set");
                    if let Message::RequestResult { result } = &mut response.body {
                        result.ok = false;
                        result.error = Some(RequestError { code:"invalid_filter".into(), message:"Check process creation times, complete executable paths and duration".into() });
                    }
                    Some(response)
                } else {
                    self.apply(filter, duration_ms, expires_at);
                    Some(self.result(request_id, "filter.set"))
                }
            }
            Message::FilterReset { request_id } => {
                self.apply(Filter::default(), None, None);
                Some(self.result(request_id, "filter.reset"))
            }
            _ => None,
        }
    }
    pub fn clear_logs(&mut self) {
        for peer in self.peers.values_mut() {
            if peer.closed {
                continue;
            }
            while let Some((seq, _)) = peer.logs.pop_front() {
                peer.dropped(seq);
                self.dropped += 1;
            }
            peer.log_bytes = 0;
        }
    }
    pub fn enqueue_log(&mut self, seq: u64, bytes: Vec<u8>) {
        if !self.status.forwarding_enabled {
            return;
        }
        for peer in self.peers.values_mut() {
            if peer.closed {
                continue;
            }
            while !peer.logs.is_empty()
                && (peer.logs.len() >= LOG_RECORDS || peer.log_bytes + bytes.len() > LOG_BYTES)
            {
                let (seq, bytes) = peer.logs.pop_front().unwrap();
                peer.log_bytes -= bytes.len();
                peer.dropped(seq);
                self.dropped += 1;
            }
            if bytes.len() > LOG_BYTES {
                peer.dropped(seq);
                self.dropped += 1;
                continue;
            }
            peer.log_bytes += bytes.len();
            peer.logs.push_back((seq, bytes.clone()));
        }
    }
    pub fn add_peer(&mut self) -> u64 {
        self.next_peer += 1;
        self.peers.insert(self.next_peer, PeerQueue::default());
        self.status.health["host"] = json!("connected");
        self.next_peer
    }
    pub fn remove_peer(&mut self, peer: u64) {
        self.peers.remove(&peer);
        self.status.health["host"] = json!(if self.peers.is_empty() {
            "waiting"
        } else {
            "connected"
        });
    }
    pub fn next_line(&mut self, peer: u64) -> Option<(Option<u64>, Vec<u8>)> {
        let peer = self.peers.get_mut(&peer)?;
        if peer.closed {
            return None;
        }
        if let Some(bytes) = peer.controls.pop_front() {
            return Some((None, bytes));
        }
        if let Some((first, last, count)) = peer.pending_gap.take() {
            return Some((
                None,
                venera_debug_protocol::encode(&Envelope::new(Message::Gap {
                    stream_id: self.stream_id.clone(),
                    reason: "queue_overflow_or_local_clear".into(),
                    count: Some(count),
                    first_seq: Some(first),
                    last_seq: Some(last),
                }))
                .unwrap(),
            ));
        }
        peer.logs.pop_front().map(|(_, bytes)| {
            peer.log_bytes -= bytes.len();
            (Some(self.forwarding_generation), bytes)
        })
    }
    pub fn queue_control(&mut self, message: &Envelope) -> bool {
        let bytes = venera_debug_protocol::encode(message).unwrap();
        let mut complete = true;
        for peer in self.peers.values_mut() {
            if peer.closed {
                continue;
            }
            if !peer.control_room() {
                complete = false;
                peer.closed = true;
            } else {
                peer.controls.push_back(bytes.clone());
            }
        }
        complete
    }
    pub fn queue_peer_control(&mut self, peer: u64, message: &Envelope) -> bool {
        let Some(peer) = self.peers.get_mut(&peer) else {
            return true;
        };
        if peer.closed {
            return true;
        }
        if !peer.control_room() {
            return false;
        }
        peer.controls
            .push_back(venera_debug_protocol::encode(message).unwrap());
        true
    }
    pub fn queue_peer_reply(&mut self, peer: u64, bytes: Vec<u8>) -> bool {
        let Some(peer) = self.peers.get_mut(&peer) else {
            return false;
        };
        if peer.closed || !peer.control_room() {
            return false;
        }
        peer.replies.push_back(bytes);
        true
    }
    pub fn peer_control_room(&self, peer: u64) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(|peer| !peer.closed && peer.control_room())
    }
    pub fn peer_closed(&self, peer: u64) -> bool {
        self.peers.get(&peer).is_none_or(|peer| peer.closed)
    }
    pub fn next_reply(&mut self, peer: u64) -> Option<Vec<u8>> {
        let peer = self.peers.get_mut(&peer)?;
        if peer.closed {
            return None;
        }
        peer.replies.pop_front()
    }
    pub fn observe_helper_drops(&mut self, session: u32, pid: u32, dropped: u64) -> bool {
        for peer in self.peers.values_mut() {
            if peer.closed {
                continue;
            }
            let reported = peer
                .helper_drops
                .get(&session)
                .filter(|(previous_pid, _)| *previous_pid == pid)
                .map(|(_, count)| *count)
                .unwrap_or(0);
            if dropped > reported {
                if !peer.control_room() {
                    peer.closed = true;
                    continue;
                }
                let message = Envelope::new(Message::Gap {
                    stream_id: self.stream_id.clone(),
                    reason: format!("session_{session}_queue_overflow"),
                    count: Some(dropped - reported),
                    first_seq: None,
                    last_seq: None,
                });
                peer.controls
                    .push_back(venera_debug_protocol::encode(&message).unwrap());
                peer.helper_drops.insert(session, (pid, dropped));
            } else if dropped == 0 && reported == 0 {
                peer.helper_drops.insert(session, (pid, 0));
            }
        }
        // Minimum cumulative loss admitted to active peers' queues; with no
        // active peer the current total is the completed reporting baseline.
        let reported_total = self
            .peers
            .values()
            .filter(|peer| !peer.closed)
            .map(|peer| {
                peer.helper_drops
                    .get(&session)
                    .filter(|(previous_pid, _)| *previous_pid == pid)
                    .map(|(_, count)| *count)
                    .unwrap_or(0)
            })
            .min()
            .unwrap_or(dropped);
        let key = session.to_string();
        let snapshot = json!({"pid":pid,"dropped_total":dropped,"reported_total":reported_total});
        let changed = self.status.health["session_helper_loss"][&key] != snapshot;
        if changed {
            self.status.health["session_helper_loss"][&key] = snapshot;
        }
        changed
    }
}

#[derive(Default)]
pub struct PeerQueue {
    closed: bool,
    pub controls: VecDeque<Vec<u8>>,
    replies: VecDeque<Vec<u8>>,
    logs: VecDeque<(u64, Vec<u8>)>,
    log_bytes: usize,
    pending_gap: Option<(u64, u64, u64)>,
    helper_drops: BTreeMap<u32, (u32, u64)>,
}
impl PeerQueue {
    fn control_room(&self) -> bool {
        self.controls.len() + self.replies.len() < CONTROL_RECORDS
    }
    fn dropped(&mut self, sequence: u64) {
        match &mut self.pending_gap {
            Some((_, last, count)) => {
                *last = sequence;
                *count += 1;
            }
            None => self.pending_gap = Some((sequence, sequence, 1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn program_capture(text: &str) -> crate::collector::Capture {
        crate::collector::Capture {
            pid: Some(42),
            tid: None,
            session_id: Some(2),
            observed_at: "2026-11-12T00:00:00Z".into(),
            observed_filetime: 100,
            source_time: None,
            source: "dbwin".into(),
            text: text.into(),
            raw_level: None,
            process: Some(venera_debug_protocol::ProcessIdentity {
                pid: 42,
                start_time_filetime: Some("100".into()),
                exe_path: Some("C:\\Tools\\ordinary.exe".into()),
            }),
            component: None,
        }
    }
    fn publish(state: &mut State, text: &str) {
        let capture = program_capture(text);
        let class = crate::collector::classify_capture(&capture);
        state.publish_capture(capture, class, &[]);
    }
    fn peer_value(state: &mut State, peer: u64) -> serde_json::Value {
        let (_, bytes) = state.next_line(peer).unwrap();
        let envelope = venera_debug_protocol::decode(&bytes).unwrap();
        serde_json::from_slice(&wire::encode(&envelope).unwrap()).unwrap()
    }
    fn fill_controls(state: &mut State, peer: u64) {
        let response = state.result("queued".into(), "status.get");
        for _ in 0..64 {
            assert!(state.queue_peer_control(peer, &response));
        }
    }
    #[test]
    fn status_pressure_closes_slow_peer_and_normal_peer_receives_each_revision() {
        let mut state = State::new("run-1".into(), true);
        let slow = state.add_peer();
        let normal = state.add_peer();
        fill_controls(&mut state, slow);
        state.apply(Filter::default(), None, None);
        assert!(state.peer_closed(slow));
        assert!(!state.peer_closed(normal));
        assert_eq!(peer_value(&mut state, normal)["params"]["revision"], "1");
        state.apply(
            Filter {
                levels: vec!["error".into()],
                ..Filter::default()
            },
            None,
            None,
        );
        let value = peer_value(&mut state, normal);
        assert_eq!(value["params"]["revision"], "2");
        assert_eq!(
            value["params"]["effective_filter"]["levels"],
            json!(["error"])
        );
        assert_eq!(state.next_line(normal), None);
        state.remove_peer(slow);
        let reconnected = state.add_peer();
        let response = state
            .control(
                Message::StatusGet {
                    request_id: "reconnected".into(),
                },
                None,
            )
            .unwrap();
        assert!(state.queue_peer_control(reconnected, &response));
        assert_eq!(
            peer_value(&mut state, reconnected)["result"]["revision"],
            "2"
        );
        assert_eq!(state.current().health["host"], "connected");
    }
    #[test]
    fn capture_loss_pressure_closes_slow_peer_and_normal_peer_receives_each_gap_once() {
        let mut state = State::new("run-1".into(), true);
        let slow = state.add_peer();
        let normal = state.add_peer();
        fill_controls(&mut state, slow);
        for reason in ["native_capture_incomplete", "process_identity_unavailable"] {
            state.queue_control(&Envelope::new(Message::Gap {
                stream_id: "run-1".into(),
                reason: reason.into(),
                count: Some(2),
                first_seq: None,
                last_seq: None,
            }));
            assert!(state.peer_closed(slow));
            assert!(!state.peer_closed(normal));
            assert_eq!(
                peer_value(&mut state, normal),
                json!({"jsonrpc":"2.0","method":"gap","params":{
                    "stream_id":"run-1","reason":reason,"count":"2","first_seq":null,"last_seq":null
                }})
            );
            assert_eq!(state.next_line(normal), None);
        }
    }
    #[test]
    fn oversized_record_gap_follows_earlier_logs_for_both_peers_under_control_pressure() {
        let mut state = State::new("run-1".into(), true);
        let slow = state.add_peer();
        let normal = state.add_peer();
        fill_controls(&mut state, slow);
        publish(&mut state, "first");
        publish(&mut state, "second");
        publish(
            &mut state,
            &"x".repeat(venera_debug_protocol::DEFAULT_MAX_LINE_BYTES),
        );
        publish(&mut state, "fourth");
        for peer in [normal, slow] {
            if peer == slow {
                for _ in 0..64 {
                    assert_eq!(peer_value(&mut state, peer)["id"], "queued");
                }
            }
            assert!(!state.peer_closed(peer));
            let first = peer_value(&mut state, peer);
            assert_eq!(
                json!([first["params"]["seq"], first["params"]["text"]]),
                json!(["1", "first"])
            );
            let second = peer_value(&mut state, peer);
            assert_eq!(
                json!([second["params"]["seq"], second["params"]["text"]]),
                json!(["2", "second"])
            );
            assert_eq!(
                peer_value(&mut state, peer),
                json!({"jsonrpc":"2.0","method":"gap","params":{
                    "stream_id":"run-1","reason":"record_size_exceeded","count":"1","first_seq":"3","last_seq":"3"
                }})
            );
            let fourth = peer_value(&mut state, peer);
            assert_eq!(
                json!([fourth["params"]["seq"], fourth["params"]["text"]]),
                json!(["4", "fourth"])
            );
            assert_eq!(state.next_line(peer), None);
        }
    }
    #[test]
    fn one_capture_reaches_local_and_both_peers_with_one_remote_sequence() {
        let mut state = State::new("run-1".into(), true);
        let a = state.add_peer();
        let b = state.add_peer();
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        state.subscribers.push(sender);
        publish(&mut state, "hello");
        let local: serde_json::Value =
            serde_json::from_slice(&receiver.try_recv().unwrap()).unwrap();
        assert_eq!(
            (
                local["stream_id"].clone(),
                local["seq"].clone(),
                local["text"].clone()
            ),
            (json!("run-1"), json!(1), json!("hello"))
        );
        for peer in [a, b] {
            let value = peer_value(&mut state, peer);
            assert_eq!(
                (
                    value["params"]["stream_id"].clone(),
                    value["params"]["seq"].clone(),
                    value["params"]["text"].clone()
                ),
                (json!("run-1"), json!("1"), json!("hello"))
            );
            assert_eq!(state.next_line(peer), None);
        }
        assert_eq!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        );
    }
    #[test]
    fn slow_peer_overflow_preserves_the_other_peers_contiguous_delivery() {
        let mut state = State::new("run-1".into(), true);
        let a = state.add_peer();
        let b = state.add_peer();
        let mut delivered = Vec::new();
        for _ in 1..=2050 {
            publish(&mut state, "ready");
            let value = peer_value(&mut state, b);
            delivered.push(
                value["params"]["seq"]
                    .as_str()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
            );
            assert_eq!(value["params"]["text"], "ready");
        }
        assert_eq!(delivered, (1..=2050).collect::<Vec<u64>>());
        assert_eq!(
            peer_value(&mut state, a),
            json!({"jsonrpc":"2.0","method":"gap","params":{
                "stream_id":"run-1","reason":"queue_overflow_or_local_clear","count":"2","first_seq":"1","last_seq":"2"
            }})
        );
        assert_eq!(peer_value(&mut state, a)["params"]["seq"], "3");
        assert_eq!(state.current().loss["queue_dropped"], 2);
        assert_eq!(state.next_line(b), None);
    }
    #[test]
    fn byte_capacity_and_control_capacity_belong_to_each_peer() {
        let mut state = State::new("run-1".into(), true);
        let a = state.add_peer();
        let b = state.add_peer();
        let payload = vec![b'x'; 1024 * 1024];
        for sequence in 1..=9 {
            state.enqueue_log(sequence, payload.clone());
            assert_eq!(state.next_line(b), Some((Some(0), payload.clone())));
        }
        assert_eq!(
            peer_value(&mut state, a)["params"],
            json!({
                "stream_id":"run-1","reason":"queue_overflow_or_local_clear","count":"1","first_seq":"1","last_seq":"1"
            })
        );
        let response = state.result("reply".into(), "status.get");
        for _ in 0..64 {
            assert!(state.queue_peer_control(a, &response));
        }
        assert!(!state.queue_peer_control(a, &response));
        assert!(state.queue_peer_control(b, &response));
        assert_eq!(peer_value(&mut state, b)["id"], "reply");
    }
    #[test]
    fn host_health_tracks_all_connected_peers() {
        let mut state = State::new("run-1".into(), true);
        let a = state.add_peer();
        let b = state.add_peer();
        assert_eq!(state.current().health["host"], "connected");
        state.remove_peer(a);
        assert_eq!(state.current().health["host"], "connected");
        state.remove_peer(b);
        assert_eq!(state.current().health["host"], "waiting");
    }
    #[test]
    fn replies_and_notifications_share_each_peers_control_capacity() {
        let mut state = State::new("run-1".into(), true);
        let a = state.add_peer();
        let b = state.add_peer();
        let response = state.result("status".into(), "status.get");
        for _ in 0..63 {
            assert!(state.queue_peer_control(a, &response));
        }
        assert!(state.queue_peer_reply(a, b"reply\n".to_vec()));
        assert!(!state.peer_control_room(a));
        assert!(!state.queue_peer_control(a, &response));
        assert!(!state.queue_peer_reply(a, b"later\n".to_vec()));
        assert!(state.peer_control_room(b));
        assert!(state.queue_peer_reply(b, b"other\n".to_vec()));
        assert_eq!(state.next_reply(b), Some(b"other\n".to_vec()));
        assert_eq!(state.next_reply(a), Some(b"reply\n".to_vec()));
        assert!(state.peer_control_room(a));
        assert!(state.queue_peer_control(a, &response));
        assert_eq!(peer_value(&mut state, a)["id"], "status");
    }
    #[test]
    fn helper_loss_closes_congested_peer_and_reports_each_delta_once_to_normal_peer() {
        let mut state = State::new("run-1".into(), true);
        let a = state.add_peer();
        let b = state.add_peer();
        let response = state.result("queued".into(), "status.get");
        for _ in 0..64 {
            assert!(state.queue_peer_control(a, &response));
        }
        state.observe_helper_drops(2, 100, 3);
        assert_eq!(
            peer_value(&mut state, b)["params"],
            json!({
                "stream_id":"run-1","reason":"session_2_queue_overflow","count":"3","first_seq":null,"last_seq":null
            })
        );
        assert!(state.peer_closed(a));
        assert_eq!(
            state.current().health["session_helper_loss"]["2"],
            json!({"pid":100,"dropped_total":3,"reported_total":3})
        );
        state.observe_helper_drops(2, 100, 3);
        assert_eq!(state.next_line(b), None);
        state.observe_helper_drops(2, 100, 5);
        assert_eq!(peer_value(&mut state, b)["params"]["count"], "2");
        assert_eq!(state.next_line(b), None);
    }
    #[test]
    fn forwarding_restart_invalidates_both_peer_frames_and_local_capture_continues() {
        let mut state = State::new("run-1".into(), true);
        let a = state.add_peer();
        let b = state.add_peer();
        let (sender, receiver) = std::sync::mpsc::sync_channel(16);
        state.subscribers.push(sender);
        publish(&mut state, "old");
        let old_a = state.next_line(a).unwrap();
        let old_b = state.next_line(b).unwrap();
        for (_, bytes) in [&old_a, &old_b] {
            let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(
                (value["seq"].clone(), value["text"].clone()),
                (json!(1), json!("old"))
            );
        }
        state.set_forwarding(false);
        publish(&mut state, "during");
        state.set_forwarding(true);
        assert!(!state.write_allowed(old_a.0));
        assert!(!state.write_allowed(old_b.0));
        publish(&mut state, "new");
        for peer in [a, b] {
            assert_eq!(
                peer_value(&mut state, peer)["params"]["forwarding_enabled"],
                false
            );
            assert_eq!(
                peer_value(&mut state, peer)["params"]["forwarding_enabled"],
                true
            );
            let value = peer_value(&mut state, peer);
            assert_eq!(
                (
                    value["params"]["seq"].clone(),
                    value["params"]["text"].clone()
                ),
                (json!("3"), json!("new"))
            );
        }
        let logs: Vec<serde_json::Value> = receiver
            .try_iter()
            .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).unwrap())
            .filter(|value| value["type"] == "log")
            .map(|value| json!([value["seq"], value["text"]]))
            .collect();
        assert_eq!(
            logs,
            vec![json!([1, "old"]), json!([2, "during"]), json!([3, "new"])]
        );
    }
    #[test]
    fn ordinary_program_reaches_local_subscription_and_v1_wire_with_identity() {
        let mut state = State::new("run-1".into(), true);
        state.add_peer();
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        state.subscribers.push(sender);
        publish(&mut state, "ordinary program: hello 中文");
        let bytes = receiver.recv().unwrap();
        let local = venera_debug_protocol::decode(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(&local).unwrap(),
            json!({"major":1,"type":"log","stream_id":"run-1","seq":1,
                "observed_at":"2026-11-12T00:00:00Z","source_time":null,"source":"dbwin",
                "component":"","level":"unknown","raw_level":"unknown","session_id":2,
                "process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\Tools\\ordinary.exe"},
                "tid":null,"text":"ordinary program: hello 中文"})
        );
        let (_, queued) = state.next_line(1).unwrap();
        assert_eq!(queued, bytes);
        let wire = wire::encode(&venera_debug_protocol::decode(&queued).unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
            json!({"jsonrpc":"2.0","method":"log","params":{
                "stream_id":"run-1","seq":"1","observed_at":"2026-11-12T00:00:00Z",
                "source_time":null,"source":"dbwin","component":"","level":"unknown",
                "raw_level":"unknown","session_id":2,
                "process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\Tools\\ordinary.exe"},
                "tid":null,"text":"ordinary program: hello 中文"}})
        );
    }
    #[test]
    fn component_filter_forwards_selected_producer_and_local_subscription_keeps_all() {
        let mut state = State::new("run-1".into(), true);
        state.add_peer();
        state.apply(
            Filter {
                components: vec!["venus".into()],
                ..Filter::default()
            },
            None,
            None,
        );
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        state.subscribers.push(sender);
        publish(&mut state, "ordinary program: hello");
        publish(&mut state, "MESA: info: mesa ready");
        publish(&mut state, "MESA-VIRTIO: debug: venus ready");
        let local: Vec<_> = receiver
            .try_iter()
            .map(|bytes| {
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                (
                    value["seq"].clone(),
                    value["component"].clone(),
                    value["text"].clone(),
                )
            })
            .collect();
        assert_eq!(
            local,
            vec![
                (json!(1), json!(""), json!("ordinary program: hello")),
                (json!(2), json!("mesa"), json!("MESA: info: mesa ready")),
                (
                    json!(3),
                    json!("venus"),
                    json!("MESA-VIRTIO: debug: venus ready"),
                ),
            ]
        );
        publish(&mut state, "MESA-VIRTIO: info: venus next");
        let mut forwarded = Vec::new();
        while let Some((generation, bytes)) = state.next_line(1) {
            if generation.is_some() {
                let envelope = venera_debug_protocol::decode(&bytes).unwrap();
                let wire = wire::encode(&envelope).unwrap();
                forwarded.push(serde_json::from_slice::<serde_json::Value>(&wire).unwrap());
            } else {
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(value["type"], "status");
            }
        }
        assert_eq!(
            forwarded,
            vec![
                json!({"jsonrpc":"2.0","method":"log","params":{
                    "stream_id":"run-1","seq":"1","observed_at":"2026-11-12T00:00:00Z",
                    "source_time":null,"source":"dbwin","component":"venus","level":"debug",
                    "raw_level":"debug","session_id":2,
                    "process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\Tools\\ordinary.exe"},
                    "tid":null,"text":"MESA-VIRTIO: debug: venus ready"}}),
                json!({"jsonrpc":"2.0","method":"log","params":{
                    "stream_id":"run-1","seq":"2","observed_at":"2026-11-12T00:00:00Z",
                    "source_time":null,"source":"dbwin","component":"venus","level":"info",
                    "raw_level":"info","session_id":2,
                    "process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\Tools\\ordinary.exe"},
                    "tid":null,"text":"MESA-VIRTIO: info: venus next"}}),
            ]
        );
    }
    #[test]
    fn local_subscription_keeps_program_output_with_host_level_target_and_forwarding_selection() {
        let mut state = State::new("run-1".into(), true);
        state.add_peer();
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        state.subscribers.push(sender);
        for filter in [
            Filter {
                levels: vec!["error".into()],
                ..Filter::default()
            },
            Filter {
                targets: vec![Target::ExePath {
                    path: "C:\\Other.exe".into(),
                }],
                ..Filter::default()
            },
        ] {
            state.status.effective_filter = filter;
            publish(&mut state, "ordinary program: selected locally");
        }
        state.status.effective_filter = Filter::default();
        state.status.forwarding_enabled = false;
        publish(&mut state, "ordinary program: selected locally");
        let local: Vec<_> = receiver
            .try_iter()
            .map(|bytes| {
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                (value["seq"].clone(), value["text"].clone())
            })
            .collect();
        assert_eq!(
            local,
            vec![
                (json!(1), json!("ordinary program: selected locally")),
                (json!(2), json!("ordinary program: selected locally")),
                (json!(3), json!("ordinary program: selected locally")),
            ]
        );
        assert_eq!(state.next_line(1), None);
        state.status.forwarding_enabled = true;
        publish(&mut state, "ordinary program: forwarding resumed");
        let (_, bytes) = state.next_line(1).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            (value["seq"].clone(), value["text"].clone()),
            (json!(2), json!("ordinary program: forwarding resumed"))
        );
    }
    fn next_gap(state: &mut State) -> serde_json::Value {
        let (_, bytes) = state.next_line(1).unwrap();
        serde_json::to_value(venera_debug_protocol::decode(&bytes).unwrap()).unwrap()
    }
    #[test]
    fn selected_oversized_record_reports_its_wire_sequence_and_next_record_continues() {
        let mut state = State::new("run-1".into(), true);
        state.add_peer();
        state.status.effective_filter.components = vec!["venus".into()];
        let oversized = "x".repeat(venera_debug_protocol::DEFAULT_MAX_LINE_BYTES);
        publish(&mut state, &oversized);
        assert_eq!(state.next_line(1), None);
        publish(&mut state, &format!("MESA-VIRTIO: info: {oversized}"));
        assert_eq!(
            next_gap(&mut state),
            json!({"major":1,"type":"gap","stream_id":"run-1",
                "reason":"record_size_exceeded","count":1,"first_seq":1,"last_seq":1})
        );
        publish(&mut state, "MESA-VIRTIO: info: ready");
        let (_, bytes) = state.next_line(1).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            (
                value["seq"].clone(),
                value["component"].clone(),
                value["text"].clone(),
            ),
            (json!(2), json!("venus"), json!("MESA-VIRTIO: info: ready"))
        );
    }
    #[test]
    fn helper_loss_reports_current_total_to_reconnected_peer() {
        let mut state = State::new("run-1".into(), true);
        let a = state.add_peer();
        let response = Envelope::new(Message::StatusGet {
            request_id: "queued".into(),
        });
        for _ in 0..64 {
            assert!(state.queue_peer_control(a, &response));
        }
        assert!(state.observe_helper_drops(2, 10852, 10925));
        assert!(state.peer_closed(a));
        assert_eq!(
            serde_json::to_value(state.current()).unwrap()["health"]["session_helper_loss"],
            json!({"2":{"pid":10852,"dropped_total":10925,"reported_total":10925}})
        );
        state.remove_peer(a);
        let b = state.add_peer();
        state.observe_helper_drops(2, 10852, 10925);
        assert_eq!(
            serde_json::to_value(state.current()).unwrap()["health"]["session_helper_loss"],
            json!({"2":{"pid":10852,"dropped_total":10925,"reported_total":10925}})
        );
        assert_eq!(
            peer_value(&mut state, b),
            json!({"jsonrpc":"2.0","method":"gap","params":{"stream_id":"run-1","reason":"session_2_queue_overflow","count":"10925","first_seq":null,"last_seq":null}})
        );
        assert!(!state.observe_helper_drops(2, 10852, 10925));
        state.queue_control(&Envelope::new(Message::StatusGet {
            request_id: "after-repeat".into(),
        }));
        assert_eq!(
            venera_debug_protocol::decode(&state.next_line(b).unwrap().1)
                .map(|envelope| serde_json::to_value(envelope).unwrap())
                .unwrap(),
            json!({"major":1,"type":"status.get","request_id":"after-repeat"})
        );
    }
    #[test]
    fn helper_restart_uses_its_own_cumulative_loss_baseline() {
        let mut state = State::new("run-1".into(), true);
        state.add_peer();
        state.observe_helper_drops(2, 100, 40);
        assert_eq!(next_gap(&mut state)["count"], 40);
        state.observe_helper_drops(2, 200, 3);
        assert_eq!(
            serde_json::to_value(state.current()).unwrap()["health"]["session_helper_loss"]["2"],
            json!({"pid":200,"dropped_total":3,"reported_total":3})
        );
        assert_eq!(next_gap(&mut state)["count"], 3);
        state.observe_helper_drops(2, 200, 8);
        assert_eq!(next_gap(&mut state)["count"], 5);
        state.observe_helper_drops(2, 300, 0);
        state.observe_helper_drops(2, 300, 2);
        assert_eq!(next_gap(&mut state)["count"], 2);
    }
    #[test]
    fn forwarding_restart_admits_new_lines_and_invalidates_inflight_generation() {
        let mut state = State::new("run-1".into(), true);
        state.add_peer();
        publish(&mut state, "old");
        let (old, bytes) = state.next_line(1).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            (value["seq"].clone(), value["text"].clone()),
            (json!(1), json!("old"))
        );
        assert!(state.write_allowed(old));
        state.set_forwarding(false);
        assert!(!state.write_allowed(old));
        publish(&mut state, "selected while forwarding is off");
        state.set_forwarding(true);
        assert!(!state.write_allowed(old));
        assert!(state.write_allowed(None));
        publish(&mut state, "new");
        while let Some((generation, bytes)) = state.next_line(1) {
            if generation.is_some() {
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    (value["seq"].clone(), value["text"].clone()),
                    (json!(3), json!("new"))
                );
                assert!(state.write_allowed(generation));
                assert_ne!(generation, old);
                return;
            }
            assert!(state.write_allowed(generation));
        }
        panic!("Expected the newly admitted log line");
    }
    #[test]
    fn executable_paths_return_authoritative_windows_separators() {
        let mut state = State::new("run-1".into(), true);
        for (input, expected) in [
            ("C:/Program Files/应用.exe", "C:\\Program Files\\应用.exe"),
            ("//server/share/应用.exe", "\\\\server\\share\\应用.exe"),
        ] {
            let response = state
                .control(
                    Message::FilterSet {
                        request_id: "path".into(),
                        filter: Filter {
                            targets: vec![Target::ExePath { path: input.into() }],
                            ..Filter::default()
                        },
                        duration_ms: None,
                    },
                    None,
                )
                .unwrap();
            let value = serde_json::to_value(response).unwrap();
            assert_eq!(value["ok"], true);
            assert_eq!(value["effective_filter"]["targets"][0]["path"], expected);
            assert_eq!(
                value["status"]["effective_filter"]["targets"][0]["path"],
                expected
            );
        }
        let response = state
            .control(
                Message::FilterSet {
                    request_id: "relative".into(),
                    filter: Filter {
                        targets: vec![Target::ExePath {
                            path: "Program Files/应用.exe".into(),
                        }],
                        ..Filter::default()
                    },
                    duration_ms: None,
                },
                None,
            )
            .unwrap();
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid_filter");
        assert_eq!(
            value["effective_filter"]["targets"][0]["path"],
            "\\\\server\\share\\应用.exe"
        );
    }
    #[test]
    fn service_revision_and_expiry() {
        let mut state = State::new("run-1".into(), true);
        state.control(
            Message::FilterSet {
                request_id: "a".into(),
                filter: Filter {
                    components: vec!["venus".into()],
                    ..Filter::default()
                },
                duration_ms: Some(0),
            },
            Some("expired".into()),
        );
        state.expire();
        let value = serde_json::to_value(state.current()).unwrap();
        assert_eq!(value["revision"], 2);
        assert_eq!(
            value["effective_filter"],
            json!({"components":[],"levels":[],"targets":[]})
        );
        assert_eq!(value["forwarding_enabled"], true);
        assert_eq!(value["expires_at"], serde_json::Value::Null);
    }
    #[test]
    fn controls_precede_queued_logs_and_overflow_reports_actual_range() {
        let mut state = State::new("run-1".into(), true);
        state.add_peer();
        for seq in 1..=2049 {
            state.enqueue_log(seq, vec![b'x']);
        }
        state.queue_control(&Envelope::new(Message::StatusGet {
            request_id: "q".into(),
        }));
        assert_eq!(
            String::from_utf8(state.next_line(1).unwrap().1).unwrap(),
            "{\"major\":1,\"type\":\"status.get\",\"request_id\":\"q\"}\n"
        );
        let gap = venera_debug_protocol::decode(&state.next_line(1).unwrap().1).unwrap();
        assert_eq!(serde_json::to_value(gap).unwrap()["count"], 1);
        assert_eq!(state.next_line(1), Some((Some(0), vec![b'x'])));
    }

    #[test]
    fn local_forwarding_clear_and_host_filter_share_authoritative_state() {
        let mut state = State::new("run-1".into(), true);
        state.add_peer();
        state.enqueue_log(1, b"first\n".to_vec());
        state.status.forwarding_enabled = false;
        state.clear_logs();
        let response = state
            .control(
                Message::FilterReset {
                    request_id: "host-1".into(),
                },
                None,
            )
            .unwrap();
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["forwarding_enabled"], false);
        assert_eq!(value["revision"], 1);
        assert_eq!(state.dropped, 1);
        assert_eq!(state.peers[&1].pending_gap, Some((1, 1, 1)));
    }

    #[test]
    fn invalid_process_instance_preserves_effective_configuration() {
        let mut state = State::new("run-1".into(), true);
        let response = state
            .control(
                Message::FilterSet {
                    request_id: "bad".into(),
                    filter: Filter {
                        targets: vec![Target::ProcessInstance {
                            pid: 4,
                            start_time_filetime: "yesterday".into(),
                            include_descendants: true,
                        }],
                        ..Filter::default()
                    },
                    duration_ms: None,
                },
                None,
            )
            .unwrap();
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["error"]["code"], "invalid_filter");
        assert_eq!(value["revision"], 0);
        assert_eq!(
            value["effective_filter"],
            json!({"components":[],"levels":[],"targets":[]})
        );
    }
    #[test]
    fn slow_subscriber_receives_queued_record_then_reconnects() {
        let mut state = State::new("run-1".into(), true);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        state.subscribers.push(sender);
        state.broadcast(b"first\n".to_vec());
        state.broadcast(b"second\n".to_vec());
        assert_eq!(receiver.recv().unwrap(), b"first\n");
        assert_eq!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        );
        state.apply(
            Filter {
                components: vec!["venus".into()],
                ..Filter::default()
            },
            None,
            None,
        );
        assert_eq!(state.current().effective_filter.components, vec!["venus"]);
    }
}
