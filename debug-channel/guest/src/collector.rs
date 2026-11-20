use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capture {
    pub pid: Option<u32>,
    pub tid: Option<u32>,
    pub session_id: Option<u32>,
    pub observed_at: String,
    pub observed_filetime: u64,
    pub source_time: Option<String>,
    pub source: String,
    pub text: String,
    pub raw_level: Option<String>,
    #[serde(default)]
    pub process: Option<venera_debug_protocol::ProcessIdentity>,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<venera_debug_protocol::v1::StructuredEvent>,
}

/// Enriches an observed instance with fields from a later authoritative query.
pub fn merge_identity(
    captured: Option<venera_debug_protocol::ProcessIdentity>,
    candidate: Option<venera_debug_protocol::ProcessIdentity>,
) -> Option<venera_debug_protocol::ProcessIdentity> {
    let Some(mut candidate) = candidate else {
        return captured;
    };
    let Some(mut captured) = captured else {
        return Some(candidate);
    };
    if captured.pid != candidate.pid {
        return Some(candidate);
    }
    if captured.boot_time_filetime.as_ref().zip(candidate.boot_time_filetime.as_ref()).is_some_and(|(left, right)| left != right)
        || captured.start_key.as_ref().zip(candidate.start_key.as_ref()).is_some_and(|(left, right)| left != right) {
        return Some(captured);
    }
    match (
        &captured.start_time_filetime,
        &candidate.start_time_filetime,
    ) {
        (Some(left), Some(right)) if left == right => {
            if captured.boot_time_filetime.is_none() { captured.boot_time_filetime = candidate.boot_time_filetime.take(); }
            if captured.start_key.is_none() { captured.start_key = candidate.start_key.take(); }
            if captured.exe_path.is_none() {
                captured.exe_path = candidate.exe_path.take();
            }
            Some(captured)
        }
        (Some(_), None) => Some(captured),
        _ => Some(candidate),
    }
}

pub fn needs_identity(
    filter: &venera_debug_protocol::Filter,
    capture: &Capture,
    tracked: bool,
) -> bool {
    use venera_debug_protocol::Target;
    let descendants = filter.targets.iter().any(|target| {
        matches!(
            target,
            Target::ProcessInstance {
                include_descendants: true,
                ..
            }
        )
    });
    let path = filter
        .targets
        .iter()
        .any(|target| matches!(target, Target::ExePath { .. }));
    capture.pid.is_some()
        && ((descendants && !tracked)
            || (path
                && capture.process.as_ref().is_none_or(|identity| {
                    identity.start_time_filetime.is_none() || identity.exe_path.is_none()
                })))
}

#[derive(Debug, PartialEq)]
pub struct Classification {
    pub component: String,
    pub level: String,
    pub raw_level: String,
    pub tid: Option<u32>,
}

fn level(raw: &str) -> &str {
    match raw {
        "T" | "trace" => "trace",
        "D" | "debug" => "debug",
        "I" | "info" => "info",
        "W" | "warn" | "warning" => "warning",
        "E" | "err" | "error" => "error",
        "fatal" => "fatal",
        _ => "unknown",
    }
}

/// Recognizes producer prefixes while retaining the complete original message.
pub fn classify(text: &str, kernel: bool) -> Option<Classification> {
    let mut s = text.trim_start();
    if s.as_bytes().get(2) == Some(&b':')
        && s.as_bytes().get(5) == Some(&b':')
        && s.as_bytes().get(8) == Some(&b'.')
        && s.as_bytes().get(12) == Some(&b' ')
    {
        s = s.get(13..)?;
    }
    let mut tid = None;
    let (component, rest) = if let Some(rest) = s.strip_prefix("MESA-VIRTIO: ") {
        ("venus", rest)
    } else if let Some(rest) = s.strip_prefix("MESA: ") {
        ("mesa", rest)
    } else if s.starts_with("venera_d3d11[") || s.starts_with("venera_d3d12[") {
        let open = s.find('[')?;
        let close = s.find("] ")?;
        let (_, thread) = s.get(open + 1..close)?.split_once(':')?;
        tid = thread.parse().ok();
        (&s[..open], &s[close + 2..])
    } else if kernel
        && s.trim_start_matches(['<', '>', '-', ' '])
            .starts_with("VioGpu")
    {
        ("kernel", "unknown")
    } else {
        return None;
    };
    let raw = rest.split([' ', ':', '\t']).next().unwrap_or("");
    let raw = if level(raw) == "unknown" && raw != "fixme" {
        "unknown"
    } else {
        raw
    };
    Some(Classification {
        component: component.into(),
        level: level(raw).into(),
        raw_level: raw.into(),
        tid,
    })
}

type FragmentKey = (Option<u32>, Option<u64>, String, u32, u32);
type ParsedFragment = (String, String, String, u32, u32, usize, usize, Vec<u8>);
const FRAGMENT_IDLE: Duration = Duration::from_secs(5);
const LATE_FRAGMENT_WINDOW: Duration = Duration::from_secs(30);
const MAX_PENDING: usize = 128;
const MAX_TRACKED: usize = 1024;
const MAX_FRAGMENT_BYTES: usize = 1024 * 1024;
struct Partial {
    last_progress: Instant,
    capture: Capture,
    level: String,
    channel: String,
    parts: Vec<Option<Vec<u8>>>,
    bytes: usize,
    remaining: usize,
}
#[derive(Default)]
pub struct Assembler {
    messages: HashMap<FragmentKey, Partial>,
    bytes: usize,
    retired: BTreeSet<FragmentKey>,
    retired_order: VecDeque<(Instant, FragmentKey)>,
    saturated: bool,
    untracked_loss: bool,
}
impl Assembler {
    /// Settles idle groups, including when the source delivers no new input.
    pub fn expire(&mut self, now: Instant) -> u64 {
        while self
            .retired_order
            .front()
            .is_some_and(|(until, _)| now >= *until)
        {
            let (_, key) = self.retired_order.pop_front().unwrap();
            self.retired.remove(&key);
        }
        let mut expired = Vec::new();
        self.messages.retain(|key, message| {
            if now.duration_since(message.last_progress) >= FRAGMENT_IDLE {
                self.bytes -= message.bytes;
                expired.push(key.clone());
                false
            } else {
                true
            }
        });
        let lost = expired.len() as u64;
        for key in expired {
            self.retire(key, now);
        }
        if self.tracked() < MAX_TRACKED {
            self.saturated = false;
        }
        lost
    }

    /// Settles remaining groups after every producer has stopped.
    pub fn finish(&mut self) -> u64 {
        let lost = self.messages.len() as u64;
        self.messages.clear();
        self.bytes = 0;
        self.retired.clear();
        self.retired_order.clear();
        self.saturated = false;
        lost
    }

    /// Reports the start of a capacity episode whose exact loss count is unknown.
    pub fn take_untracked_loss(&mut self) -> bool {
        std::mem::take(&mut self.untracked_loss)
    }

    fn tracked(&self) -> usize {
        self.messages.len() + self.retired.len()
    }

    fn retire(&mut self, key: FragmentKey, now: Instant) {
        self.retired.insert(key.clone());
        self.retired_order
            .push_back((now + LATE_FRAGMENT_WINDOW, key));
    }

    fn discard(&mut self, key: FragmentKey, now: Instant) {
        if let Some(message) = self.messages.remove(&key) {
            self.bytes -= message.bytes;
        }
        self.retire(key, now);
    }

    /// Returns complete captures and the number of observed assemblies lost.
    pub fn ingest(&mut self, capture: Capture) -> (Option<Capture>, u64) {
        self.ingest_at(capture, Instant::now())
    }

    pub fn ingest_at(&mut self, mut capture: Capture, now: Instant) -> (Option<Capture>, u64) {
        let lost = self.expire(now);
        if capture.structured.is_some() || !capture.text.starts_with("VDBG1|") {
            return (Some(capture), lost);
        }
        let fields: Vec<_> = capture.text.splitn(9, '|').collect();
        let parse = || -> Option<ParsedFragment> {
            if fields.len() != 9 || !["dxvk", "vkd3d-proton", "probe"].contains(&fields[1]) {
                return None;
            }
            let valid = match fields[1] {
                "dxvk" => {
                    fields[3] == "default"
                        && ["trace", "debug", "info", "warn", "err"].contains(&fields[2])
                }
                "vkd3d-proton" => {
                    ["api", "shader"].contains(&fields[3])
                        && ["none", "err", "info", "fixme", "warn", "trace"].contains(&fields[2])
                }
                "probe" => fields[3] == "default" && fields[2] == "info",
                _ => false,
            };
            if !valid
                || fields[4..8]
                    .iter()
                    .any(|field| field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()))
            {
                return None;
            }
            let tid = fields[4].parse().ok()?;
            let sequence = fields[5].parse().ok()?;
            let part: usize = fields[6].parse().ok()?;
            let parts: usize = fields[7].parse().ok()?;
            if part == 0
                || part > parts
                || parts > 1024
                || fields[8].len() > 2800
                || fields[8].len() % 2 != 0
            {
                return None;
            }
            let hex = fields[8].as_bytes();
            let mut bytes = Vec::with_capacity(hex.len() / 2);
            for pair in hex.as_chunks::<2>().0 {
                let digit = |b| match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    _ => None,
                };
                bytes.push(digit(pair[0])? * 16 + digit(pair[1])?);
            }
            Some((
                fields[1].into(),
                fields[2].into(),
                fields[3].into(),
                tid,
                sequence,
                part,
                parts,
                bytes,
            ))
        };
        let Some((producer, raw, channel, tid, sequence, part, parts, bytes)) = parse() else {
            return (None, lost + 1);
        };
        let key = (
            capture.pid,
            capture
                .process
                .as_ref()
                .and_then(|p| p.start_time_filetime.as_deref()?.parse().ok()),
            producer.clone(),
            tid,
            sequence,
        );
        if self.retired.contains(&key) {
            return (None, lost);
        }
        let pending = self.messages.get(&key);
        if let Some(message) = pending {
            let consistent =
                message.parts.len() == parts && message.level == raw && message.channel == channel;
            if consistent && message.parts[part - 1].as_ref() == Some(&bytes) {
                return (None, lost);
            }
            if !consistent || message.parts[part - 1].is_some() {
                self.discard(key, now);
                return (None, lost + 1);
            }
        }
        let payload = if parts == 1 {
            bytes
        } else {
            // Pending groups reserve their slot when they transition to retired.
            if pending.is_none() && self.tracked() == MAX_TRACKED {
                self.untracked_loss |= !self.saturated;
                self.saturated = true;
                return (None, lost);
            }
            if (pending.is_none() && self.messages.len() == MAX_PENDING)
                || self.bytes + bytes.len() > MAX_FRAGMENT_BYTES
            {
                self.discard(key, now);
                return (None, lost + 1);
            }
            let message = self.messages.entry(key.clone()).or_insert_with(|| {
                // The decoded payload is stored in parts, and the capture keeps metadata only.
                capture.text = String::new();
                Partial {
                    last_progress: now,
                    capture: capture.clone(),
                    level: raw.clone(),
                    channel: channel.clone(),
                    parts: vec![None; parts],
                    bytes: 0,
                    remaining: parts,
                }
            });
            message.bytes += bytes.len();
            self.bytes += bytes.len();
            message.parts[part - 1] = Some(bytes);
            message.last_progress = now;
            message.remaining -= 1;
            if message.remaining != 0 {
                return (None, lost);
            }
            let message = self.messages.remove(&key).unwrap();
            self.bytes -= message.bytes;
            capture = message.capture;
            message.parts.into_iter().flatten().flatten().collect()
        };
        let text = match String::from_utf8(payload) {
            Ok(text) => text,
            Err(_) => {
                if parts > 1 {
                    self.retire(key, now);
                }
                return (None, lost + 1);
            }
        };
        capture.text = text;
        capture.tid = Some(tid);
        capture.raw_level = Some(raw);
        capture.component = Some(
            if producer == "probe" {
                "probe"
            } else if producer == "dxvk" {
                "dxvk"
            } else if channel == "shader" {
                "vkd3d-shader"
            } else {
                "vkd3d"
            }
            .into(),
        );
        (Some(capture), lost)
    }
}

pub fn classify_capture(capture: &Capture) -> Option<Classification> {
    if let Some(component) = &capture.component {
        let raw = capture
            .raw_level
            .clone()
            .unwrap_or_else(|| "unknown".into());
        return Some(Classification {
            component: component.clone(),
            level: level(&raw).into(),
            raw_level: raw,
            tid: capture.tid,
        });
    }
    classify(&capture.text, capture.source == "etw")
}

#[cfg(test)]
mod tests {
    use super::*;
    use venera_debug_protocol::{Filter, ProcessIdentity, Target};
    fn identity(pid: u32, start: Option<&str>, path: Option<&str>) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_time_filetime: start.map(str::to_owned),
            exe_path: path.map(str::to_owned),
            ..Default::default()
        }
    }
    #[test]
    fn identity_enrichment_preserves_observed_path_for_same_instance() {
        assert_eq!(
            merge_identity(
                Some(identity(42, Some("100"), None)),
                Some(identity(42, Some("100"), Some("C:\\应用.exe")))
            ),
            Some(identity(42, Some("100"), Some("C:\\应用.exe")))
        );
        assert_eq!(
            merge_identity(
                Some(identity(42, Some("100"), Some("C:\\应用.exe"))),
                Some(identity(42, Some("100"), None))
            ),
            Some(identity(42, Some("100"), Some("C:\\应用.exe")))
        );
        assert_eq!(
            merge_identity(
                Some(identity(42, Some("100"), Some("C:\\应用.exe"))),
                Some(identity(42, None, None))
            ),
            Some(identity(42, Some("100"), Some("C:\\应用.exe")))
        );
    }
    #[test]
    fn authoritative_instance_changes_keep_their_own_path_fields() {
        assert_eq!(
            merge_identity(
                Some(identity(42, Some("100"), Some("C:\\old.exe"))),
                Some(identity(42, Some("200"), None))
            ),
            Some(identity(42, Some("200"), None))
        );
        assert_eq!(
            merge_identity(
                Some(identity(41, Some("100"), Some("C:\\old.exe"))),
                Some(identity(42, Some("100"), None))
            ),
            Some(identity(42, Some("100"), None))
        );
        assert_eq!(
            merge_identity(
                Some(identity(42, None, Some("C:\\old.exe"))),
                Some(identity(42, Some("200"), None))
            ),
            Some(identity(42, Some("200"), None))
        );
        assert_eq!(
            merge_identity(
                Some(identity(42, None, Some("C:\\old.exe"))),
                Some(identity(42, None, None))
            ),
            Some(identity(42, None, None))
        );
    }
    #[test]
    fn executable_target_waits_for_identity_enrichment_before_filtering() {
        let filter = Filter {
            targets: vec![Target::ExePath {
                path: "C:\\应用.exe".into(),
            }],
            ..Filter::default()
        };
        let mut capture = event("MESA: info: hello");
        capture.process = Some(identity(42, Some("100"), None));
        assert!(needs_identity(&filter, &capture, true));
        capture.process = merge_identity(
            capture.process.take(),
            Some(identity(42, Some("100"), Some("C:\\应用.exe"))),
        );
        assert!(!needs_identity(&filter, &capture, true));
        let record = venera_debug_protocol::LogRecord {
            structured: None,
            ancestors: Vec::new(),
            stream_id: "run-1".into(),
            seq: 1,
            observed_at: capture.observed_at,
            source_time: None,
            source: "dbwin".into(),
            component: "mesa".into(),
            level: "info".into(),
            raw_level: "info".into(),
            session_id: Some(1),
            process: capture.process,
            tid: None,
            text: "MESA: info: hello".into(),
        };
        assert!(venera_debug_protocol::matches_filter(&filter, &record, &[]));
        assert_eq!(
            record.process,
            Some(identity(42, Some("100"), Some("C:\\应用.exe")))
        );
    }
    fn event(text: &str) -> Capture {
        Capture {
            structured: None,
            pid: Some(42),
            tid: None,
            session_id: Some(1),
            observed_at: "2026-11-10T00:00:00Z".into(),
            observed_filetime: 0,
            source_time: None,
            source: "dbwin".into(),
            text: text.into(),
            raw_level: None,
            process: None,
            component: None,
        }
    }
    #[test]
    fn native_message_completes_across_multiple_inactivity_windows() {
        let mut assembler = Assembler::default();
        let now = Instant::now();
        let first = event("VDBG1|dxvk|info|default|42|71|2|3|ad");
        let second = event("VDBG1|dxvk|info|default|42|71|1|3|e4b8");
        let third = event("VDBG1|dxvk|info|default|42|71|3|3|0a");
        assert!(assembler.ingest_at(first, now).0.is_none());
        assert!(assembler
            .ingest_at(second, now + Duration::from_secs(4))
            .0
            .is_none());
        assert_eq!(assembler.expire(now + Duration::from_secs(7)), 0);
        let (capture, lost) = assembler.ingest_at(third, now + Duration::from_secs(8));
        assert_eq!(lost, 0);
        assert_eq!(capture.unwrap().text, "中\n");
        assert_eq!(assembler.finish(), 0);
    }
    #[test]
    fn native_idle_settlement_counts_message_once_with_duplicate_and_late_parts() {
        let mut assembler = Assembler::default();
        let now = Instant::now();
        let first = "VDBG1|dxvk|info|default|42|71|2|3|62";
        assert_eq!(assembler.ingest_at(event(first), now).1, 0);
        assert_eq!(
            assembler
                .ingest_at(event(first), now + Duration::from_secs(4))
                .1,
            0
        );
        assert_eq!(assembler.expire(now + Duration::from_secs(5)), 1);
        for part in [
            "VDBG1|dxvk|info|default|42|71|1|3|61",
            first,
            "VDBG1|dxvk|info|default|42|71|3|3|63",
        ] {
            let (capture, lost) = assembler.ingest_at(event(part), now + Duration::from_secs(6));
            assert_eq!((capture.map(|c| c.text), lost), (None, 0));
        }
        assert_eq!(assembler.expire(now + Duration::from_secs(12)), 0);
        assert_eq!(assembler.finish(), 0);
    }
    #[test]
    fn native_finish_settles_each_producer_message_identity_once() {
        let mut assembler = Assembler::default();
        let now = Instant::now();
        let first = "VDBG1|dxvk|info|default|42|71|1|3|61";
        assert_eq!(assembler.ingest_at(event(first), now).1, 0);
        assert_eq!(
            assembler
                .ingest_at(event("VDBG1|dxvk|info|default|42|71|2|3|62"), now)
                .1,
            0
        );
        let mut other_pid = event(first);
        other_pid.pid = Some(43);
        assert_eq!(assembler.ingest_at(other_pid, now).1, 0);
        assert_eq!(
            assembler
                .ingest_at(event("VDBG1|vkd3d-proton|info|api|42|71|1|3|61"), now)
                .1,
            0
        );
        assert_eq!(assembler.finish(), 3);
        assert_eq!(assembler.finish(), 0);
    }
    #[test]
    fn native_overload_counts_rejected_messages_and_serves_pending_groups() {
        let mut assembler = Assembler::default();
        let now = Instant::now();
        for sequence in 0..128 {
            let fragment = format!("VDBG1|dxvk|info|default|42|{sequence}|1|2|61");
            assert_eq!(assembler.ingest_at(event(&fragment), now).1, 0);
        }
        assert_eq!(
            assembler
                .ingest_at(event("VDBG1|dxvk|info|default|42|128|1|3|61"), now)
                .1,
            1
        );
        let (capture, lost) =
            assembler.ingest_at(event("VDBG1|dxvk|info|default|42|0|2|2|62"), now);
        assert_eq!((capture.unwrap().text.as_str(), lost), ("ab", 0));
        for part in [2, 3] {
            let fragment = format!("VDBG1|dxvk|info|default|42|128|{part}|3|62");
            assert_eq!(assembler.ingest_at(event(&fragment), now).1, 0);
        }
        assert_eq!(assembler.finish(), 127);
    }
    #[test]
    fn native_identity_saturation_reports_a_single_unknown_loss_episode() {
        let mut assembler = Assembler::default();
        let now = Instant::now();
        let mut lost = 0;
        for sequence in 0..1025 {
            let fragment = format!("VDBG1|dxvk|info|default|42|{sequence}|1|2|61");
            lost += assembler.ingest_at(event(&fragment), now).1;
        }
        assert_eq!(lost, 896);
        assert!(assembler.take_untracked_loss());
        assert_eq!(
            assembler
                .ingest_at(event("VDBG1|dxvk|info|default|42|1024|2|2|62"), now)
                .1,
            0
        );
        assert!(!assembler.take_untracked_loss());
        assert_eq!(assembler.expire(now + Duration::from_secs(5)), 128);
        assert_eq!(assembler.finish(), 0);
    }
    #[test]
    fn native_byte_overload_releases_a_rejected_messages_payload() {
        let mut assembler = Assembler::default();
        let now = Instant::now();
        let hex = "61".repeat(1400);
        let mut lost = 0;
        for part in 1..=750 {
            let fragment = format!("VDBG1|dxvk|info|default|42|71|{part}|751|{hex}");
            lost += assembler.ingest_at(event(&fragment), now).1;
        }
        assert_eq!(lost, 1);
        let first = event("VDBG1|dxvk|info|default|42|72|1|2|61");
        let second = event("VDBG1|dxvk|info|default|42|72|2|2|62");
        assert!(assembler.ingest_at(first, now).0.is_none());
        let (capture, lost) = assembler.ingest_at(second, now);
        assert_eq!((capture.unwrap().text.as_str(), lost), ("ab", 0));
        assert_eq!(assembler.finish(), 0);
    }
    #[test]
    fn native_patch_literal_messages_retain_payloads_and_channels() {
        let mut assembler = Assembler::default();
        for (input, text, component, raw, normalized, tid) in [
            (
                "VDBG1|dxvk|info|default|42|7|1|1|68656c6c6f0a",
                "hello\n",
                "dxvk",
                "info",
                "info",
                42,
            ),
            (
                "VDBG1|vkd3d-proton|fixme|shader|99|4294967295|1|1|666f6f3a20e4b8ad0a",
                "foo: 中\n",
                "vkd3d-shader",
                "fixme",
                "unknown",
                99,
            ),
            (
                "VDBG1|vkd3d-proton|err|api|7|0|1|1|666f6f3a206261640a",
                "foo: bad\n",
                "vkd3d",
                "err",
                "error",
                7,
            ),
            (
                "VDBG1|probe|info|default|0|1|1|1|",
                "",
                "probe",
                "info",
                "info",
                0,
            ),
        ] {
            let (capture, lost) = assembler.ingest(event(input));
            let capture = capture.unwrap();
            assert_eq!(
                (capture.text.as_str(), lost, capture.tid),
                (text, 0, Some(tid))
            );
            let class = classify_capture(&capture).unwrap();
            assert_eq!(
                (
                    class.component.as_str(),
                    class.raw_level.as_str(),
                    class.level.as_str()
                ),
                (component, raw, normalized)
            );
        }
        assert_eq!(
            assembler
                .ingest(event("VDBG1|dxvk|info|shader|1|2|1|1|61"))
                .1,
            1
        );
    }
    #[test]
    fn native_1400_byte_parts_and_invalid_assemblies_are_observed() {
        let mut assembler = Assembler::default();
        let first = format!("VDBG1|dxvk|debug|default|42|9|1|2|{}", "61".repeat(1400));
        assert!(assembler.ingest(event(&first)).0.is_none());
        let (complete, lost) = assembler.ingest(event("VDBG1|dxvk|debug|default|42|9|2|2|e4b8ad"));
        assert_eq!(complete.unwrap().text, format!("{}中", "a".repeat(1400)));
        assert_eq!(lost, 0);
        let oversized = format!("VDBG1|dxvk|debug|default|42|9|1|1|{}", "61".repeat(1401));
        assert_eq!(assembler.ingest(event(&oversized)).1, 1);
        assert_eq!(
            assembler
                .ingest(event("VDBG1|dxvk|info|default|42|4294967296|1|1|61"))
                .1,
            1
        );
        assert_eq!(
            assembler
                .ingest(event("VDBG1|vkd3d-proton|info|api|42|1|1|1|zz"))
                .1,
            1
        );
    }
    #[test]
    fn producer_identity_and_original_levels() {
        let venus = classify("MESA-VIRTIO: debug: 中文", false).unwrap();
        assert_eq!(
            (
                venus.component.as_str(),
                venus.level.as_str(),
                venus.raw_level.as_str()
            ),
            ("venus", "debug", "debug")
        );
        let wrapper = classify("12:34:56.123 venera_d3d12[41:99] W hello", false).unwrap();
        assert_eq!(
            (
                wrapper.component.as_str(),
                wrapper.tid,
                wrapper.level.as_str()
            ),
            ("venera_d3d12", Some(99), "warning")
        );
        assert_eq!(classify("unrelated app: error: message", false), None);
        assert_eq!(
            classify("MESA: renderer message", false).unwrap().level,
            "unknown"
        );
    }
    #[test]
    fn engine_fragments_reassemble_utf8_and_preserve_thread_and_level() {
        let mut assembler = Assembler::default();
        let event = |text: &str| Capture {
            structured: None,
            pid: Some(42),
            tid: None,
            session_id: Some(1),
            observed_at: "2026-11-10T00:00:00Z".into(),
            observed_filetime: 0,
            source_time: None,
            source: "dbwin".into(),
            text: text.into(),
            raw_level: None,
            process: None,
            component: None,
        };
        assert!(assembler
            .ingest(event("VDBG1|dxvk|warn|default|99|7|1|2|e4b8"))
            .0
            .is_none());
        let (capture, lost) = assembler.ingest(event("VDBG1|dxvk|warn|default|99|7|2|2|ad0a"));
        let capture = capture.unwrap();
        assert_eq!(
            (capture.text.as_str(), capture.tid, lost),
            ("中\n", Some(99), 0)
        );
        assert_eq!(
            classify_capture(&capture).unwrap(),
            Classification {
                component: "dxvk".into(),
                level: "warning".into(),
                raw_level: "warn".into(),
                tid: Some(99)
            }
        );
    }
}
pub fn etw_loss(
    stream_id: &str,
    previous: &mut u32,
    total: u32,
) -> Option<venera_debug_protocol::v1::HistoryEvent> {
    let count = total.wrapping_sub(*previous);
    *previous = total;
    (count != 0).then(|| venera_debug_protocol::v1::HistoryEvent::Gap {
        gap: venera_debug_protocol::v1::Gap {
            stream_id: stream_id.into(),
            reason: "etw_events_lost".into(),
            count: Some(venera_debug_protocol::v1::DecimalU64(u64::from(count))),
            first_seq: None,
            last_seq: None,
        },
    })
}
