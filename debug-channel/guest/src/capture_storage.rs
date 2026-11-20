pub const TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
pub const TRACE_MEGABYTES: u32 = 32;
pub const LIVE_FILES_PER_SOURCE: usize = 2;
// Two sources: three boot files and two handoff files each; two Recorder exports.
// One MiB covers final buffer alignment and the paired formatting data.
pub const CAPTURE_RESERVATION: u64 =
    (2 * (3 + LIVE_FILES_PER_SOURCE as u64) * TRACE_MEGABYTES as u64 + 2 * 4 + 1) * 1024 * 1024;
pub const JOURNAL_BYTES: u64 = TOTAL_BYTES - CAPTURE_RESERVATION;

use crate::{history::History, journal::ImportSource, state::State};
use std::{io, sync::Mutex};

pub fn capture_loss(
    events: u32,
    buffers: u32,
    invalid: u64,
    capacity: bool,
    recovered: bool,
) -> Option<(String, Option<u64>)> {
    if events == 0 && buffers == 0 && invalid == 0 {
        return None;
    }
    // The number of events in lost buffers is unknown.
    let count = if buffers == 0 {
        u64::from(events).checked_add(invalid)
    } else {
        None
    };
    Some((format!("capture_loss events={events} buffers={buffers} invalid={invalid} capacity={capacity} recovered_export={recovered}"), count))
}

pub struct ImportDiagnostics {
    pub loss: Option<(String, Option<u64>)>,
    capacity_reached: bool,
    recovered_export: bool,
}

impl ImportDiagnostics {
    pub fn sealed(
        file: &str,
        physical_bytes: u64,
        recovered_export: bool,
        events: u32,
        buffers: u32,
        invalid: u64,
    ) -> Self {
        let limit = if file.starts_with("recorder-") {
            4
        } else {
            u64::from(TRACE_MEGABYTES)
        };
        let capacity_reached = physical_bytes >= limit * 1024 * 1024;
        Self {
            loss: capture_loss(events, buffers, invalid, capacity_reached, recovered_export),
            capacity_reached,
            recovered_export,
        }
    }

    pub fn source_failure(reason: String) -> Self {
        Self {
            loss: Some((reason, None)),
            capacity_reached: false,
            recovered_export: false,
        }
    }

    pub fn finish(
        self,
        state: &Mutex<State>,
        history: &History,
        source: ImportSource,
        file: &str,
        boot: u64,
        now_ms: u64,
    ) -> io::Result<()> {
        self.finish_with_graphics(state, history, source, file, boot, now_ms, None)
    }

    pub fn finish_with_graphics(
        self,
        state: &Mutex<State>,
        history: &History,
        source: ImportSource,
        file: &str,
        boot: u64,
        now_ms: u64,
        graphics: Option<crate::engine_events::GraphicsUpdate>,
    ) -> io::Result<()> {
        let recorder = file.starts_with("recorder-").then(|| match &self.loss {
            Some((reason, _)) => serde_json::json!({
                "error": format!("{reason}; source={}", source.id), "file": file
            }),
            None => serde_json::json!("exported"),
        });
        let session = (self.capacity_reached || self.recovered_export).then(|| {
            serde_json::json!({
                "file": file, "source": source.id, "capacity_reached": self.capacity_reached,
                "recovered_export": self.recovered_export,
            })
        });
        history.complete_graphics_import(source, self.loss, boot, now_ms, graphics)?;
        let mut state = state.lock().unwrap();
        if let Some(result) = recorder {
            state.status.health["recorder"] = result;
        }
        if let Some(result) = session {
            state.status.health["capture_session"] = result;
        }
        Ok(())
    }
}

pub fn validate(files: &[(String, u64)]) -> std::io::Result<()> {
    let mut counts = [0usize; 5];
    let mut bytes = 0u64;
    for (name, size) in files {
        let group = if name.starts_with("kernel-boot.etl") {
            0
        } else if name.starts_with("legacy-boot.etl") {
            1
        } else if name.starts_with("kernel-live-") {
            2
        } else if name.starts_with("legacy-live-") {
            3
        } else if name.starts_with("recorder-v1-") {
            4
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Unrecognized capture file",
            ));
        };
        counts[group] += 1;
        let limit = if group == 4 {
            4
        } else {
            TRACE_MEGABYTES as u64
        } * 1024
            * 1024
            + 64 * 1024;
        bytes = bytes
            .checked_add(*size)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        if *size > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "Trace file exceeds its capture reservation",
            ));
        }
    }
    if counts[0] > 3
        || counts[1] > 3
        || counts[2] > LIVE_FILES_PER_SOURCE
        || counts[3] > LIVE_FILES_PER_SOURCE
        || counts[4] > 2
        || bytes > CAPTURE_RESERVATION
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "Capture storage is full",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use venera_debug_protocol::v1::{DecimalU64, HistoryEvent, HistoryRead};

    #[test]
    fn sealed_source_session_status_and_history() {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let now = elapsed.as_millis() as u64;
        let boot = 116_444_736_000_000_000 + (now - 1000) * 10_000;
        let dir = std::env::temp_dir().join(format!(
            "venera-etw-status-{}-{}",
            std::process::id(),
            elapsed.as_nanos()
        ));
        for (id, file, bytes, recovered, counters, session, expected_gap) in [
            ("full", "kernel-live-full.etl", 32 * 1024 * 1024, false, (0, 0, 0),
                json!({"file":"kernel-live-full.etl", "source":"full", "capacity_reached":true, "recovered_export":false}), None),
            ("recovered", "recorder-recovered.etl", 4 * 1024 * 1024 - 1, true, (0, 0, 0),
                json!({"file":"recorder-recovered.etl", "source":"recovered", "capacity_reached":false, "recovered_export":true}), None),
            ("recovered-with-loss", "recorder-loss.etl", 4 * 1024 * 1024, true, (28, 0, 2),
                json!({"file":"recorder-loss.etl", "source":"recovered-with-loss", "capacity_reached":true, "recovered_export":true}),
                Some(("capture_loss events=28 buffers=0 invalid=2 capacity=true recovered_export=true; source=recovered-with-loss", Some(DecimalU64(30))))),
            ("lost-buffers", "recorder-buffers.etl", 65536, false, (5, 3, 0), json!(null),
                Some(("capture_loss events=5 buffers=3 invalid=0 capacity=false recovered_export=false; source=lost-buffers", None))),
        ] {
            let open = || {
                let journal = Journal::open(dir.join(id), "etw-status", Default::default()).unwrap();
                let stop = Arc::new(AtomicBool::new(false));
                let (history, owner) = History::start(journal, stop.clone()).unwrap();
                (history, owner, stop)
            };
            let read = |history: &History| history.read(HistoryRead {
                after: None, through: None, limit: 100,
            }).unwrap();
            let legacy = ImportSource { id: "old-archive".into(), boot_filetime: boot };
            let source = ImportSource { id: id.into(), boot_filetime: boot };
            let (history, owner, stop) = open();
            history.discover_imports(vec![legacy.clone(), source.clone()]).unwrap();
            history.complete_import(legacy, Some(("capture_loss events=0 buffers=0 invalid=0 capacity=true recovered_export=false".into(), None)), boot, now).unwrap();
            let old_page = read(&history);
            assert_eq!(old_page.records.len(), 1);

            let state = Mutex::new(State::new("etw-status".into(), true));
            ImportDiagnostics::sealed(file, bytes, recovered, counters.0, counters.1, counters.2)
                .finish(&state, &history, source.clone(), file, boot, now).unwrap();
            let health = state.lock().unwrap().status.health.clone();
            assert_eq!(health["capture_session"], session, "{id}");
            let recorder = match (id, expected_gap) {
                ("full", _) => json!(null),
                (_, Some((reason, _))) => json!({"error": reason, "file": file}),
                _ => json!("exported"),
            };
            assert_eq!(health["recorder"], recorder, "{id}");
            assert!(history.import_receipt(source).unwrap().complete, "{id}");

            let page = read(&history);
            assert_eq!(page.records[..1], old_page.records, "{id}");
            let gaps: Vec<_> = page.records[1..].iter().map(|entry| {
                let HistoryEvent::Gap { gap } = &entry.event else { panic!("{entry:?}") };
                assert_eq!((gap.first_seq, gap.last_seq), (None, None));
                (gap.reason.as_str(), gap.count)
            }).collect();
            assert_eq!(gaps, expected_gap.into_iter().collect::<Vec<_>>(), "{id}");
            assert_eq!(page.range.high_water, DecimalU64(1 + u64::from(expected_gap.is_some())), "{id}");
            stop.store(true, Ordering::Release);
            owner.join().unwrap().unwrap();

            let (history, owner, stop) = open();
            assert_eq!(read(&history), page, "{id}");
            stop.store(true, Ordering::Release);
            owner.join().unwrap().unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn physical_capture_slots_fit_beside_journal_replacements() {
        let mut files = Vec::new();
        for name in ["kernel", "legacy"] {
            for index in 0..3 {
                files.push((format!("{name}-boot.etl.{index}"), 32 * 1024 * 1024 + 65536));
            }
            for index in 0..2 {
                files.push((format!("{name}-live-{index}.etl"), 32 * 1024 * 1024 + 65536));
            }
        }
        for index in 0..2 {
            files.push((format!("recorder-v1-{index}.etl"), 4 * 1024 * 1024 + 65536));
        }
        validate(&files).unwrap();
        assert_eq!(
            JOURNAL_BYTES + files.iter().map(|(_, size)| size).sum::<u64>(),
            1_073_479_680
        );
        files.push(("kernel-live-extra.etl".into(), 1));
        assert_eq!(
            validate(&files).unwrap_err().kind(),
            std::io::ErrorKind::StorageFull
        );
        files.pop();
        files[0].1 += 1;
        assert_eq!(
            validate(&files).unwrap_err().kind(),
            std::io::ErrorKind::StorageFull
        );
    }
}
