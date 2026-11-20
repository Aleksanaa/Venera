pub const TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
pub const TRACE_MEGABYTES: u32 = 32;
pub const LIVE_FILES_PER_SOURCE: usize = 2;
// Two sources: three boot files and two handoff files each; two Recorder exports.
// One MiB covers final buffer alignment and the paired formatting data.
pub const CAPTURE_RESERVATION: u64 =
    (2 * (3 + LIVE_FILES_PER_SOURCE as u64) * TRACE_MEGABYTES as u64 + 2 * 4 + 1) * 1024 * 1024;
pub const JOURNAL_BYTES: u64 = TOTAL_BYTES - CAPTURE_RESERVATION;

pub fn capture_loss(
    events: u32,
    buffers: u32,
    invalid: u64,
    capacity: bool,
    recovered: bool,
) -> Option<(String, Option<u64>)> {
    if events == 0 && buffers == 0 && invalid == 0 && !capacity && !recovered {
        return None;
    }
    let count = (buffers == 0 && !capacity && !recovered).then_some(u64::from(events) + invalid);
    Some((format!("capture_loss events={events} buffers={buffers} invalid={invalid} capacity={capacity} recovered_export={recovered}"), count))
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
    #[test]
    fn sealed_source_loss_counts_follow_trace_evidence() {
        assert_eq!(capture_loss(0, 0, 0, false, false), None);
        assert_eq!(
            capture_loss(28, 0, 2, false, false),
            Some((
                "capture_loss events=28 buffers=0 invalid=2 capacity=false recovered_export=false"
                    .into(),
                Some(30)
            ))
        );
        for (buffers, capacity, recovered) in
            [(1, false, false), (0, true, false), (0, false, true)]
        {
            assert_eq!(
                capture_loss(0, buffers, 0, capacity, recovered).unwrap().1,
                None
            );
        }
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
