use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
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
                json!({"jsonrpc":"2.0","method":"log","params": serde_json::to_value(record)?})
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

    pub(crate) fn status_v1(status: &Status) -> Result<v1::Status, serde_json::Error> {
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

    pub(crate) fn filter_v1(filter: &Filter) -> Result<v1::Filter, serde_json::Error> {
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
                boot_time_filetime,
                start_key,
            } => v1::Target::ProcessInstance {
                pid: *pid,
                start_time_filetime: filetime_v1(start_time_filetime)?,
                include_descendants: *include_descendants,
                boot_time_filetime: boot_time_filetime.as_ref().map(|value| filetime_v1(value)).transpose()?,
                start_key: start_key.as_ref().map(|value| filetime_v1(value)).transpose()?,
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
                boot_time_filetime,
                start_key,
            } => Target::ProcessInstance {
                pid,
                start_time_filetime: start_time_filetime.0.to_string(),
                include_descendants,
                boot_time_filetime: boot_time_filetime.map(|value| value.0.to_string()),
                start_key: start_key.map(|value| value.0.to_string()),
            },
        }
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
                state.queue_peer_control(1, &response);
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
                    boot_time_filetime: None, start_key: None,
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
                    boot_time_filetime: None, start_key: None,
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
                    structured: None,
                    ancestors: Vec::new(),
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
                        ..Default::default()
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
                    structured: None,
                    ancestors: Vec::new(),
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

const CONTROL_RECORDS: usize = 64;

#[cfg(windows)]
pub struct RegisteredHelper {
    pub identity: venera_debug_protocol::ProcessIdentity,
    pub delivered: std::sync::Arc<std::sync::Mutex<u64>>,
}

pub struct State {
    pub status: Status,
    forwarding_generation: u64,
    pub stream_id: String,
    local_sequence: u64,
    deadline: Option<Instant>,
    pub filter_started_filetime: u64,
    pub peers: BTreeMap<u64, PeerQueue>,
    next_peer: u64,
    // Counts discarded deliveries across all peer queues, rather than unique captures.
    pub dropped: u64,
    pub parse_errors: u64,
    #[cfg(windows)]
    pub helpers: std::collections::BTreeMap<u32, RegisteredHelper>,
    pub processes: crate::ancestry::Processes,
    #[cfg(windows)]
    pub assembler: crate::collector::Assembler,
    #[cfg(windows)]
    pub history: Option<crate::history::History>,
    #[cfg(windows)]
    pub boot_id: String,
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
            local_sequence: 0,
            deadline: None,
            filter_started_filetime: 0,
            peers: BTreeMap::new(),
            next_peer: 0,
            dropped: 0,
            parse_errors: 0,
            #[cfg(windows)]
            helpers: Default::default(),
            processes: Default::default(),
            #[cfg(windows)]
            assembler: Default::default(),
            #[cfg(windows)]
            history: None,
            #[cfg(windows)]
            boot_id: String::new(),
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
    pub fn capture_record(
        &mut self,
        capture: crate::collector::Capture,
        class: Option<crate::collector::Classification>,
    ) -> Option<venera_debug_protocol::LogRecord> {
        let class = class.unwrap_or_else(|| crate::collector::Classification {
            component: String::new(),
            level: "unknown".into(),
            raw_level: "unknown".into(),
            tid: None,
        });
        if class.component == "probe" {
            return None;
        }
        self.local_sequence += 1;
        Some(venera_debug_protocol::LogRecord {
            structured: capture.structured,
            ancestors: capture
                .process
                .as_ref()
                .map(|identity| self.processes.ancestors(identity, 0))
                .unwrap_or_default(),
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
        })
    }
    pub fn set_forwarding(&mut self, enabled: bool) {
        if !enabled {
            self.forwarding_generation += 1;
        }
        self.status.forwarding_enabled = enabled;
        self.notify();
    }
    pub fn write_allowed(&self, generation: Option<u64>) -> bool {
        generation.is_none_or(|generation| {
            self.status.forwarding_enabled && generation == self.forwarding_generation
        })
    }
    pub fn history_generation(&self) -> u64 {
        self.forwarding_generation
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
                        boot_time_filetime, start_key, ..
                    } => start_time_filetime.parse::<u64>().is_err()
                        || boot_time_filetime.as_ref().is_some_and(|value| value.parse::<u64>().is_err())
                        || start_key.as_ref().is_some_and(|value| value.parse::<u64>().is_err() || boot_time_filetime.is_none()),
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
    pub fn add_peer(&mut self) -> u64 {
        self.insert_peer(true)
    }
    pub fn add_local_peer(&mut self) -> u64 {
        self.insert_peer(false)
    }
    fn insert_peer(&mut self, remote: bool) -> u64 {
        self.next_peer += 1;
        self.peers.insert(
            self.next_peer,
            PeerQueue {
                remote,
                ..PeerQueue::default()
            },
        );
        self.update_host_health();
        self.next_peer
    }
    pub fn remove_peer(&mut self, peer: u64) {
        self.peers.remove(&peer);
        self.update_host_health();
    }
    fn update_host_health(&mut self) {
        self.status.health["host"] = json!(if self.peers.values().any(|peer| peer.remote) {
            "connected"
        } else {
            "waiting"
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
        None
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
        self.queue_reply(peer, None, bytes)
    }
    pub fn queue_reply(&mut self, peer: u64, generation: Option<u64>, bytes: Vec<u8>) -> bool {
        let Some(peer) = self.peers.get_mut(&peer) else {
            return false;
        };
        if peer.closed || !peer.control_room() {
            return false;
        }
        peer.replies.push_back((generation, bytes));
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
    pub fn next_reply(&mut self, peer: u64) -> Option<(Option<u64>, Vec<u8>)> {
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
    remote: bool,
    closed: bool,
    pub controls: VecDeque<Vec<u8>>,
    replies: VecDeque<(Option<u64>, Vec<u8>)>,
    helper_drops: BTreeMap<u32, (u32, u64)>,
}
impl PeerQueue {
    fn control_room(&self) -> bool {
        self.controls.len() + self.replies.len() < CONTROL_RECORDS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_capture_keeps_body_before_forwarding_and_filter_selection() {
        let mut state = State::new("capture".into(), false);
        state.status.effective_filter.levels = vec!["error".into()];
        state.status.effective_filter.components = vec!["venus".into()];
        let record = state
            .capture_record(program_capture("ordinary program"), None)
            .unwrap();
        assert_eq!(
            serde_json::to_value(record).unwrap(),
            json!({"stream_id":"capture","seq":"1","observed_at":"2026-11-12T00:00:00Z","source_time":null,"source":"dbwin","component":"","level":"unknown","raw_level":"unknown","session_id":2,"process":{"pid":42,"start_time_filetime":"100","exe_path":"C:\\Tools\\ordinary.exe"},"tid":null,"text":"ordinary program"})
        );
    }
    fn program_capture(text: &str) -> crate::collector::Capture {
        crate::collector::Capture {
            structured: None,
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
                ..Default::default()
            }),
            component: None,
        }
    }

    #[test]
    fn captured_kernel_fields_reach_the_canonical_record() {
        let mut state = State::new("capture-1".into(), true);
        let mut capture = program_capture("VDBG1|probe|info|default|42|7|1|1|78");
        capture.source = "etw".into();
        capture.component = Some("kernel".into());
        capture.raw_level = Some("error".into());
        capture.structured = Some(serde_json::from_value(serde_json::json!({
            "provider":"6c5a2378-22b9-4c10-a91f-69e34c280b81", "event_id":3, "version":1,
            "boot_time_filetime":"100", "producer_instance":"20", "producer_sequence":"18446744073709551615",
            "source_time_filetime":"123", "fields":{"status":{"type":"int32","value":-1073741823}}
        })).unwrap());
        let (capture, lost) = crate::collector::Assembler::default().ingest(capture);
        assert_eq!(lost, 0);
        let capture = capture.unwrap();
        let class = crate::collector::classify_capture(&capture);
        let record = state.capture_record(capture, class).unwrap();
        let value = serde_json::to_value(record).unwrap();
        assert_eq!(value["structured"]["producer_sequence"], "18446744073709551615");
        assert_eq!(value["structured"]["fields"]["status"], serde_json::json!({"type":"int32","value":-1073741823}));
        assert_eq!(value["component"], "kernel");
        assert_eq!(value["level"], "error");
        assert_eq!(value["text"], "VDBG1|probe|info|default|42|7|1|1|78");
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
    fn retained_capture_matches_original_ancestors_after_exit_and_pid_reuse() {
        let mut state = State::new("capture".into(), false);
        state.status.effective_filter.components = vec!["venus".into()];
        let identity = |pid, start: &str| venera_debug_protocol::ProcessIdentity {
            pid,
            start_time_filetime: Some(start.into()),
            exe_path: None,
            ..Default::default()
        };
        state.processes.started(identity(10, "10"), 0, Some(2));
        state.processes.started(identity(20, "20"), 10, Some(2));
        state.processes.started(identity(42, "100"), 20, Some(2));
        let record = state
            .capture_record(program_capture("historical child"), None)
            .unwrap();
        state.processes.ended(20, 20, 110);
        state.processes.ended(42, 100, 110);
        state.processes.started(identity(20, "120"), 0, Some(2));
        state.processes.retire_before(130);
        let root =
            std::env::temp_dir().join(format!("venera-ancestry-history-{}", std::process::id()));
        let mut journal = crate::journal::Journal::open(
            &root,
            "ancestry",
            crate::journal::RetentionPolicy::default(),
        )
        .unwrap();
        journal
            .append(
                "boot",
                1,
                true,
                venera_debug_protocol::v1::HistoryEvent::Log { record },
            )
            .unwrap();
        drop(journal);
        let journal = crate::journal::Journal::open(
            &root,
            "ancestry",
            crate::journal::RetentionPolicy::default(),
        )
        .unwrap();
        let page = journal
            .read(
                venera_debug_protocol::v1::HistoryRead {
                    after: None,
                    through: None,
                    limit: 256,
                },
                1024 * 1024,
            )
            .unwrap();
        let venera_debug_protocol::v1::HistoryEvent::Log { record } = &page.records[0].event else {
            panic!("log")
        };
        assert_eq!(
            record.ancestors,
            vec![identity(20, "20"), identity(10, "10")]
        );
        assert_eq!(
            venera_debug_protocol::matches_filter(
                &state.status.effective_filter,
                record,
                &record.ancestors
            ),
            false
        );
        let filter = |start: &str, include_descendants| Filter {
            targets: vec![Target::ProcessInstance {
                pid: 20,
                start_time_filetime: start.into(),
                include_descendants,
                boot_time_filetime: None, start_key: None,
            }],
            ..Filter::default()
        };
        assert_eq!(
            venera_debug_protocol::matches_filter(&filter("20", true), record, &record.ancestors),
            true
        );
        assert_eq!(
            venera_debug_protocol::matches_filter(&filter("120", true), record, &record.ancestors),
            false
        );
        assert_eq!(
            venera_debug_protocol::matches_filter(&filter("20", false), record, &record.ancestors),
            false
        );
        assert_eq!(record.text, "historical child");
        drop(journal);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_connections_preserve_remote_host_health() {
        let mut state = State::new("run".into(), false);
        let local = state.add_local_peer();
        assert_eq!(state.current().health["host"], "waiting");
        let remote = state.add_peer();
        assert_eq!(state.current().health["host"], "connected");
        state.remove_peer(remote);
        assert_eq!(state.current().health["host"], "waiting");
        state.remove_peer(local);
        assert_eq!(state.current().health["host"], "waiting");
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
        assert_eq!(state.next_reply(b), Some((None, b"other\n".to_vec())));
        assert_eq!(state.next_reply(a), Some((None, b"reply\n".to_vec())));
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
    fn next_gap(state: &mut State) -> serde_json::Value {
        let (_, bytes) = state.next_line(1).unwrap();
        serde_json::to_value(venera_debug_protocol::decode(&bytes).unwrap()).unwrap()
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
        state.queue_peer_control(
            b,
            &Envelope::new(Message::StatusGet {
                request_id: "after-repeat".into(),
            }),
        );
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
    fn forwarding_resume_admits_fresh_history_replies() {
        let mut state = State::new("run-1".into(), true);
        let peer = state.add_peer();
        let old = state.history_generation();
        assert!(state.queue_reply(peer, Some(old), b"history one\n".to_vec()));
        let queued = state.next_reply(peer).unwrap();
        assert_eq!(queued.1, b"history one\n");
        assert!(state.write_allowed(queued.0));
        state.set_forwarding(false);
        assert!(!state.write_allowed(queued.0));
        let captured = state
            .capture_record(program_capture("captured during pause"), None)
            .unwrap();
        assert_eq!(captured.text, "captured during pause");
        state.set_forwarding(true);
        assert!(!state.write_allowed(queued.0));
        assert!(state.queue_reply(
            peer,
            Some(state.history_generation()),
            b"history resumed\n".to_vec()
        ));
        let resumed = state.next_reply(peer).unwrap();
        assert_eq!(resumed.1, b"history resumed\n");
        assert!(state.write_allowed(resumed.0));
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
    fn forwarding_and_filter_controls_return_authoritative_state() {
        let mut state = State::new("run-1".into(), true);
        state.add_peer();
        state.set_forwarding(false);
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
                            boot_time_filetime: None, start_key: None,
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
}
