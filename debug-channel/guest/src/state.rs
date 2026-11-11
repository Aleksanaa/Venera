use serde_json::json;
use std::{
    collections::VecDeque,
    sync::mpsc::SyncSender,
    time::{Duration, Instant},
};
use venera_debug_protocol::{
    Envelope, Filter, Message, RequestError, RequestResult, Status, Target,
};

pub const LOG_RECORDS: usize = 2048;
const LOG_BYTES: usize = 8 * 1024 * 1024;
const CONTROL_RECORDS: usize = 64;

pub struct State {
    pub status: Status,
    forwarding_generation: u64,
    pub stream_id: String,
    #[cfg(windows)]
    pub sequence: u64,
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
            #[cfg(windows)]
            sequence: 0,
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
    fn next_gap(state: &mut State) -> serde_json::Value {
        let (_, bytes) = state.next_line().unwrap();
        serde_json::to_value(venera_debug_protocol::decode(&bytes).unwrap()).unwrap()
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
        state.enqueue_log(1, b"old\n".to_vec());
        let (old, bytes) = state.next_line().unwrap();
        assert_eq!(bytes, b"old\n");
        assert!(state.write_allowed(old));
        state.set_forwarding(false);
        assert!(!state.write_allowed(old));
        state.set_forwarding(true);
        assert!(!state.write_allowed(old));
        assert!(state.write_allowed(None));
        state.enqueue_log(2, b"new\n".to_vec());
        while let Some((generation, bytes)) = state.next_line() {
            if generation.is_some() {
                assert_eq!(bytes, b"new\n");
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
