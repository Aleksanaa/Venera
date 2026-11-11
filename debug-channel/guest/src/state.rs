use serde_json::json;
use std::{
    collections::VecDeque,
    sync::mpsc::SyncSender,
    time::{Duration, Instant},
};
use venera_debug_protocol::{
    Envelope, Filter, Message, RequestError, RequestResult, Status, Target,
};

pub(crate) mod serial {
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
                    "unsupported serial message",
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
                encoding_error("successful serial response requires a status snapshot")
            })?;
            json!({"jsonrpc":"2.0","id":result.request_id,"result":status_v1(status)?})
        } else {
            json!({"jsonrpc":"2.0","id":result.request_id,"error":{"code":-32000,"message":result.error.as_ref().map(|e| e.message.clone()).unwrap_or_else(|| "Request failed".into()),"data":result.error}})
        })
    }
    fn system_response(result: &venera_debug_protocol::SystemInfoResult) -> Value {
        if result.ok {
            json!({"jsonrpc":"2.0","id":result.request_id,"result":v1::SystemInfo { collected_at: result.collected_at.clone(), session_id: result.session_id, sections: result.sections.iter().map(|section| v1::SystemInfoSection { title: section.title.clone(), rows: section.rows.iter().map(|row| v1::SystemInfoRow { key: row.key.clone(), value: row.value.clone(), bad: row.bad, warning: row.warning.clone() }).collect() }).collect() }})
        } else {
            json!({"jsonrpc":"2.0","id":result.request_id,"error":{"code":-32000,"message":result.error.as_ref().map(|e| e.message.clone()).unwrap_or_else(|| "System report failed".into()),"data":result.error}})
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn filter_results_encode_current_health_and_loss() {
            let mut state = State::new("run-1".into(), true);
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
                // Exercise the queued representation used by the serial transport.
                state.queue_control(&response);
                let queued = state.controls.pop_back().unwrap();
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
                "successful serial response requires a status snapshot"
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
    pub controls: VecDeque<Vec<u8>>,
    pub logs: VecDeque<(u64, Vec<u8>)>,
    log_bytes: usize,
    pub subscribers: Vec<SyncSender<Vec<u8>>>,
    pub dropped: u64,
    pub parse_errors: u64,
    pub pending_gap: Option<(u64, u64, u64)>,
    #[cfg(windows)]
    pub helpers: std::collections::BTreeMap<u32, venera_debug_protocol::ProcessIdentity>,
    helper_drops: std::collections::BTreeMap<u32, (u32, u64)>,
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
            controls: VecDeque::new(),
            logs: VecDeque::new(),
            log_bytes: 0,
            subscribers: Vec::new(),
            dropped: 0,
            parse_errors: 0,
            pending_gap: None,
            #[cfg(windows)]
            helpers: Default::default(),
            helper_drops: Default::default(),
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
        if self.controls.len() < CONTROL_RECORDS {
            self.controls.push_back(bytes);
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
            self.queue_control(&Envelope::new(Message::Gap {
                stream_id: self.stream_id.clone(),
                reason: "record_size_exceeded".into(),
                count: Some(1),
                first_seq: Some(self.sequence),
                last_seq: Some(self.sequence),
            }));
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
    fn dropped(&mut self, sequence: u64) {
        self.dropped += 1;
        match &mut self.pending_gap {
            Some((_, last, count)) => {
                *last = sequence;
                *count += 1;
            }
            None => self.pending_gap = Some((sequence, sequence, 1)),
        }
    }
    pub fn clear_logs(&mut self) {
        while let Some((seq, _)) = self.logs.pop_front() {
            self.dropped(seq);
        }
        self.log_bytes = 0;
    }
    pub fn enqueue_log(&mut self, seq: u64, bytes: Vec<u8>) {
        if !self.status.forwarding_enabled {
            return;
        }
        while !self.logs.is_empty()
            && (self.logs.len() >= LOG_RECORDS || self.log_bytes + bytes.len() > LOG_BYTES)
        {
            let (seq, bytes) = self.logs.pop_front().unwrap();
            self.log_bytes -= bytes.len();
            self.dropped(seq);
        }
        if bytes.len() > LOG_BYTES {
            self.dropped(seq);
            return;
        }
        self.log_bytes += bytes.len();
        self.logs.push_back((seq, bytes));
    }
    pub fn next_line(&mut self) -> Option<(Option<u64>, Vec<u8>)> {
        if let Some(bytes) = self.controls.pop_front() {
            return Some((None, bytes));
        }
        if let Some((first, last, count)) = self.pending_gap.take() {
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
        self.logs.pop_front().map(|(_, bytes)| {
            self.log_bytes -= bytes.len();
            (Some(self.forwarding_generation), bytes)
        })
    }
    pub fn queue_control(&mut self, message: &Envelope) -> bool {
        if self.controls.len() == CONTROL_RECORDS {
            return false;
        }
        self.controls
            .push_back(venera_debug_protocol::encode(message).unwrap());
        true
    }
    pub fn observe_helper_drops(&mut self, session: u32, pid: u32, dropped: u64) -> bool {
        let reported = self
            .helper_drops
            .get(&session)
            .filter(|(previous_pid, _)| *previous_pid == pid)
            .map(|(_, count)| *count)
            .unwrap_or(0);
        if dropped > reported
            && self.queue_control(&Envelope::new(Message::Gap {
                stream_id: self.stream_id.clone(),
                reason: format!("session_{session}_queue_overflow"),
                count: Some(dropped - reported),
                first_seq: None,
                last_seq: None,
            }))
        {
            self.helper_drops.insert(session, (pid, dropped));
        } else if dropped == 0 && reported == 0 {
            self.helper_drops.insert(session, (pid, 0));
        }
        let reported_total = self
            .helper_drops
            .get(&session)
            .filter(|(previous_pid, _)| *previous_pid == pid)
            .map(|(_, count)| *count)
            .unwrap_or(0);
        let key = session.to_string();
        let snapshot = json!({"pid":pid,"dropped_total":dropped,"reported_total":reported_total});
        let changed = self.status.health["session_helper_loss"][&key] != snapshot;
        if changed {
            self.status.health["session_helper_loss"][&key] = snapshot;
        }
        changed
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
    #[test]
    fn ordinary_program_reaches_local_subscription_and_v1_serial_with_identity() {
        let mut state = State::new("run-1".into(), true);
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
        let (_, queued) = state.next_line().unwrap();
        assert_eq!(queued, bytes);
        let serial = serial::encode(&venera_debug_protocol::decode(&queued).unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&serial).unwrap(),
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
        while let Some((generation, bytes)) = state.next_line() {
            if generation.is_some() {
                let envelope = venera_debug_protocol::decode(&bytes).unwrap();
                let serial = serial::encode(&envelope).unwrap();
                forwarded.push(serde_json::from_slice::<serde_json::Value>(&serial).unwrap());
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
        assert_eq!(state.next_line(), None);
        state.status.forwarding_enabled = true;
        publish(&mut state, "ordinary program: forwarding resumed");
        let (_, bytes) = state.next_line().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            (value["seq"].clone(), value["text"].clone()),
            (json!(2), json!("ordinary program: forwarding resumed"))
        );
    }
    fn next_gap(state: &mut State) -> serde_json::Value {
        let (_, bytes) = state.next_line().unwrap();
        serde_json::to_value(venera_debug_protocol::decode(&bytes).unwrap()).unwrap()
    }
    #[test]
    fn selected_oversized_record_reports_its_wire_sequence_and_next_record_continues() {
        let mut state = State::new("run-1".into(), true);
        state.status.effective_filter.components = vec!["venus".into()];
        let oversized = "x".repeat(venera_debug_protocol::DEFAULT_MAX_LINE_BYTES);
        publish(&mut state, &oversized);
        assert_eq!(state.next_line(), None);
        publish(&mut state, &format!("MESA-VIRTIO: info: {oversized}"));
        assert_eq!(
            next_gap(&mut state),
            json!({"major":1,"type":"gap","stream_id":"run-1",
                "reason":"record_size_exceeded","count":1,"first_seq":1,"last_seq":1})
        );
        publish(&mut state, "MESA-VIRTIO: info: ready");
        let (_, bytes) = state.next_line().unwrap();
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
    fn helper_loss_retries_the_entire_unreported_delta_after_control_pressure() {
        let mut state = State::new("run-1".into(), true);
        while state.queue_control(&Envelope::new(Message::StatusGet {
            request_id: "queued".into(),
        })) {}
        assert!(state.observe_helper_drops(2, 10852, 10925));
        assert_eq!(
            serde_json::to_value(state.current()).unwrap()["health"]["session_helper_loss"],
            json!({"2":{"pid":10852,"dropped_total":10925,"reported_total":0}})
        );
        while state.next_line().is_some() {}
        assert!(state.observe_helper_drops(2, 10852, 10925));
        assert_eq!(
            serde_json::to_value(state.current()).unwrap()["health"]["session_helper_loss"],
            json!({"2":{"pid":10852,"dropped_total":10925,"reported_total":10925}})
        );
        assert_eq!(
            next_gap(&mut state),
            json!({"major":1,"type":"gap","stream_id":"run-1","reason":"session_2_queue_overflow","count":10925,"first_seq":null,"last_seq":null})
        );
        assert!(!state.observe_helper_drops(2, 10852, 10925));
        state.queue_control(&Envelope::new(Message::StatusGet {
            request_id: "after-repeat".into(),
        }));
        assert_eq!(
            next_gap(&mut state),
            json!({"major":1,"type":"status.get","request_id":"after-repeat"})
        );
    }
    #[test]
    fn helper_restart_uses_its_own_cumulative_loss_baseline() {
        let mut state = State::new("run-1".into(), true);
        state.observe_helper_drops(2, 100, 40);
        assert_eq!(next_gap(&mut state)["count"], 40);
        while state.queue_control(&Envelope::new(Message::StatusGet {
            request_id: "queued".into(),
        })) {}
        state.observe_helper_drops(2, 200, 3);
        assert_eq!(
            serde_json::to_value(state.current()).unwrap()["health"]["session_helper_loss"]["2"],
            json!({"pid":200,"dropped_total":3,"reported_total":0})
        );
        while state.next_line().is_some() {}
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
        publish(&mut state, "old");
        let (old, bytes) = state.next_line().unwrap();
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
        while let Some((generation, bytes)) = state.next_line() {
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
        for seq in 1..=2049 {
            state.enqueue_log(seq, vec![b'x']);
        }
        state.queue_control(&Envelope::new(Message::StatusGet {
            request_id: "q".into(),
        }));
        assert_eq!(
            String::from_utf8(state.next_line().unwrap().1).unwrap(),
            "{\"major\":1,\"type\":\"status.get\",\"request_id\":\"q\"}\n"
        );
        let gap = venera_debug_protocol::decode(&state.next_line().unwrap().1).unwrap();
        assert_eq!(serde_json::to_value(gap).unwrap()["count"], 1);
        assert_eq!(state.next_line(), Some((Some(0), vec![b'x'])));
    }

    #[test]
    fn local_forwarding_clear_and_host_filter_share_authoritative_state() {
        let mut state = State::new("run-1".into(), true);
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
        assert_eq!(state.pending_gap, Some((1, 1, 1)));
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
