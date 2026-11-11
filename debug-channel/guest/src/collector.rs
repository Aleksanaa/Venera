use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
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
    match (
        &captured.start_time_filetime,
        &candidate.start_time_filetime,
    ) {
        (Some(left), Some(right)) if left == right => {
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

type FragmentKey = (u32, Option<String>, String, u32, u32);
type ParsedFragment = (String, String, String, u32, u32, usize, usize, Vec<u8>);
struct Partial {
    created: Instant,
    capture: Capture,
    level: String,
    channel: String,
    parts: Vec<Option<Vec<u8>>>,
    bytes: usize,
}
#[derive(Default)]
pub struct Assembler {
    messages: HashMap<FragmentKey, Partial>,
    bytes: usize,
}
impl Assembler {
    /// Returns complete captures and the number of observed assemblies lost.
    pub fn ingest(&mut self, mut capture: Capture) -> (Option<Capture>, u64) {
        let mut lost = 0;
        self.messages.retain(|_, message| {
            if message.created.elapsed() >= Duration::from_secs(5) {
                self.bytes -= message.bytes;
                lost += 1;
                false
            } else {
                true
            }
        });
        if !capture.text.starts_with("VDBG1|") {
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
            capture.pid.unwrap_or(0),
            capture
                .process
                .as_ref()
                .and_then(|p| p.start_time_filetime.clone()),
            producer.clone(),
            tid,
            sequence,
        );
        if (self.messages.len() >= 128 && !self.messages.contains_key(&key))
            || self.bytes + bytes.len() > 1024 * 1024
        {
            return (None, lost + 1);
        }
        let message = self.messages.entry(key.clone()).or_insert_with(|| Partial {
            created: Instant::now(),
            capture: capture.clone(),
            level: raw.clone(),
            channel: channel.clone(),
            parts: vec![None; parts],
            bytes: 0,
        });
        if message.parts.len() != parts
            || message.level != raw
            || message.channel != channel
            || message.parts[part - 1].is_some()
        {
            let previous = self.messages.remove(&key).unwrap();
            self.bytes -= previous.bytes;
            return (None, lost + 1);
        }
        message.bytes += bytes.len();
        self.bytes += bytes.len();
        message.parts[part - 1] = Some(bytes);
        if message.parts.iter().any(Option::is_none) {
            return (None, lost);
        }
        let message = self.messages.remove(&key).unwrap();
        self.bytes -= message.bytes;
        let payload: Vec<u8> = message.parts.into_iter().flatten().flatten().collect();
        let text = match String::from_utf8(payload) {
            Ok(text) => text,
            Err(_) => return (None, lost + 1),
        };
        capture = message.capture;
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
