use super::*;
use venera_debug_protocol::v1::{
    AttributionHistoryEvent, AttributionHistoryPage, AttributionHistoryRecord, Gap,
    HistoryAttribution, HistoryEncoding, HISTORY_ATTRIBUTION_MARKER,
};

// The marker remains a valid v1 history event on disk. Its adjacent payload and
// its position are published by the same manifest as the segment extent.
#[derive(Serialize, Deserialize)]
pub(super) struct Revision {
    pub target: HistoryCursor,
    attribution: HistoryAttribution,
}

impl Revision {
    pub(super) fn validate(&self, record: &HistoryRecord) -> io::Result<()> {
        if !matches!(&record.event, HistoryEvent::Gap { gap } if gap.is_history_attribution_marker())
            || self.target.journal_id != record.cursor.journal_id
            || self.target.position.0 == 0
            || self.target.position >= record.cursor.position
            || !self.attribution.is_resolved()
        {
            return Err(invalid("invalid journal attribution revision"));
        }
        Ok(())
    }

    fn event(self) -> AttributionHistoryEvent {
        AttributionHistoryEvent::AttributionRevision {
            target: self.target,
            attribution: self.attribution,
        }
    }
}

pub(super) fn source_time(record: &HistoryRecord, provenance: Option<&Provenance>) -> Option<u64> {
    let HistoryEvent::Log { record: log } = &record.event else {
        return None;
    };
    if log.process.is_none() || log.component == "kernel" || log.source == "wpp_recorder" {
        return None;
    }
    let boot: u64 = record.boot_id.parse().ok()?;
    let time = log
        .structured
        .as_ref()
        .map(|e| e.source_time_filetime.0)
        .or_else(|| provenance.map(|p| p.source_filetime))
        .or_else(|| parse_filetime(log.source_time.as_deref()?))?;
    (boot != 0 && time >= boot).then_some(time)
}

// Inverse of the Windows collector's UTC FILETIME representation (100 ns).
fn parse_filetime(value: &str) -> Option<u64> {
    let (date, clock) = value.strip_suffix('Z')?.split_once('T')?;
    let (year, date) = date.split_once('-')?;
    let (month, day) = date.split_once('-')?;
    let (hour, clock) = clock.split_once(':')?;
    let (minute, clock) = clock.split_once(':')?;
    let (second, fraction) = clock.split_once('.')?;
    let number = |s: &str, width| {
        (s.len() == width && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    let year = number(year, 4)?;
    let month = number(month, 2)?;
    let day = number(day, 2)?;
    let hour = number(hour, 2)?;
    let minute = number(minute, 2)?;
    let second = number(second, 2)?;
    let fraction = number(fraction, 7)?;
    let leap = |y: u64| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let mut months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    months[1] += u64::from(leap(year));
    if year < 1601 || !(1..=12).contains(&month) || hour >= 24 || minute >= 60 || second >= 60 {
        return None;
    }
    let month = month as usize - 1;
    if day == 0 || day > months[month] {
        return None;
    }
    let leap_days = |y: u64| y / 4 - y / 100 + y / 400;
    let days = (year - 1601) * 365 + leap_days(year - 1) - leap_days(1600)
        + months[..month].iter().sum::<u64>()
        + day
        - 1;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 10_000_000 + fraction)
}

impl Journal {
    fn locate_entry(&self, position: u64) -> Option<(u64, &Entry)> {
        let (&key, segment) = self.segments.range(..=position).next_back()?;
        let index = segment
            .entries
            .binary_search_by_key(&position, |entry| entry.position)
            .ok()?;
        Some((key, &segment.entries[index]))
    }

    pub(super) fn snapshot_targets<'a>(
        &'a self,
        segment: &'a Segment,
    ) -> impl Iterator<Item = u64> + 'a {
        segment.entries.iter().filter_map(|revision| {
            let target = revision.revision_target?;
            if *self.revisions.get(&target)?.last()? != revision.position {
                return None;
            }
            self.locate_entry(target).map(|(key, _)| key)
        })
    }

    fn current_attribution(&self, target: u64) -> io::Result<Option<HistoryAttribution>> {
        let Some(position) = self
            .revisions
            .get(&target)
            .and_then(|positions| positions.last())
        else {
            return Ok(None);
        };
        let (key, entry) = self
            .locate_entry(*position)
            .ok_or_else(|| invalid("revision entry missing"))?;
        let stored: StoredRecord = serde_json::from_slice(&self.entry_bytes(key, entry)?)?;
        let revision = stored
            .revision
            .ok_or_else(|| invalid("revision payload missing"))?;
        Ok(Some(revision.attribution))
    }

    // Reconcile from the published evidence after a restart between the evidence
    // commit and the revision commit. Published revisions supply the progress.
    pub(super) fn resume_attribution(&mut self) -> io::Result<()> {
        let boots: BTreeSet<u64> = self
            .segments
            .values()
            .filter_map(|s| s.boot.parse().ok())
            .collect();
        for boot in boots {
            self.revise_attribution(boot, 0)?;
        }
        Ok(())
    }

    pub(super) fn revise_attribution(&mut self, boot: u64, after: u64) -> io::Result<u64> {
        let process_through = self.process_through(boot)?;
        if process_through == 0 {
            return Ok(0);
        }
        let boot_id = boot.to_string();
        let through = self.metadata.high_water;
        let mut cursor = after;
        let mut published = 0;
        while cursor < through {
            let key = self
                .segments
                .range(..=cursor)
                .next_back()
                .map_or(cursor, |(&key, _)| key);
            let Some((key, entry, time)) = self
                .segments
                .range(key..=through)
                .filter(|(_, segment)| segment.boot == boot_id)
                .find_map(|(&key, segment)| {
                    segment.entries.iter().find_map(|entry| {
                        (entry.position > cursor && entry.position <= through)
                            .then_some(entry.attribution_time)
                            .flatten()
                            .filter(|&time| time <= process_through)
                            .map(|time| (key, entry, time))
                    })
                })
            else {
                break;
            };
            cursor = entry.position;
            let record = self.decode_entry(cursor, &self.entry_bytes(key, entry)?)?;
            let HistoryEvent::Log { record: log } = record.event else {
                continue;
            };
            let mut resolved = log.clone();
            if self.attribute_process_record(&mut resolved, boot, time, true)?
                != crate::ancestry::Attribution::Ready
            {
                continue;
            }
            let Some(process) = resolved.process else {
                continue;
            };
            let attribution = HistoryAttribution {
                process,
                ancestors: resolved.ancestors,
            };
            let previous = self.current_attribution(cursor)?;
            let previous_process = previous
                .as_ref()
                .map(|p| &p.process)
                .or(log.process.as_ref());
            let previous_ancestors = previous.as_ref().map_or(&log.ancestors, |p| &p.ancestors);
            if !attribution.is_resolved()
                || attribution.ancestors.len() < previous_ancestors.len()
                || (previous_process == Some(&attribution.process)
                    && previous_ancestors == &attribution.ancestors)
            {
                continue;
            }
            let revision = Revision {
                target: record.cursor,
                attribution,
            };
            let marker = JournalInput {
                boot_id: record.boot_id,
                recorded_at_ms: record.recorded_at_ms.0,
                startup: record.startup,
                event: HistoryEvent::Gap {
                    gap: Gap {
                        stream_id: self.metadata.journal_id.clone(),
                        reason: HISTORY_ATTRIBUTION_MARKER.into(),
                        count: Some(DecimalU64(0)),
                        first_seq: None,
                        last_seq: None,
                    },
                },
            };
            // Check the capable frame separately from the compact on-disk envelope.
            let position = self.metadata.high_water + 1;
            let mut range = self.range()?;
            range.high_water = DecimalU64(position);
            let next = HistoryCursor {
                journal_id: range.journal_id.clone(),
                position: DecimalU64(position),
            };
            let probe = AttributionHistoryPage {
                range,
                next: next.clone(),
                missing: vec![],
                records: vec![AttributionHistoryRecord {
                    cursor: next,
                    boot_id: marker.boot_id.clone(),
                    recorded_at_ms: record.recorded_at_ms,
                    startup: marker.startup,
                    event: AttributionHistoryEvent::AttributionRevision {
                        target: revision.target.clone(),
                        attribution: revision.attribution.clone(),
                    },
                }],
            };
            if serde_json::to_vec(&probe)?.len() <= MAX_HISTORY_PAGE_BYTES - 768 {
                self.append_stored_inner(vec![(marker, None)], None, None, Some(&revision))?;
                published += 1;
            }
        }
        Ok(published)
    }

    pub(super) fn attribution_entry(
        &self,
        key: u64,
        entry: &Entry,
    ) -> io::Result<AttributionHistoryRecord> {
        let bytes = self.entry_bytes(key, entry)?;
        let record = self.decode_entry(entry.position, &bytes)?;
        let event = match record.event {
            HistoryEvent::Log { record } => AttributionHistoryEvent::Log { record },
            HistoryEvent::Gap { .. } if entry.revision_target.is_some() => {
                let stored: StoredRecord = serde_json::from_slice(&bytes)?;
                stored
                    .revision
                    .ok_or_else(|| invalid("revision payload missing"))?
                    .event()
            }
            HistoryEvent::Gap { gap } => AttributionHistoryEvent::Gap { gap },
        };
        Ok(AttributionHistoryRecord {
            cursor: record.cursor,
            boot_id: record.boot_id,
            recorded_at_ms: record.recorded_at_ms,
            startup: record.startup,
            event,
        })
    }

    pub(super) fn encode_history_page(
        &self,
        page: AttributionHistoryPage,
        encoding: HistoryEncoding,
    ) -> io::Result<serde_json::Value> {
        let mut snapshots = BTreeMap::new();
        if encoding == HistoryEncoding::Compatibility {
            for entry in &page.records {
                let AttributionHistoryEvent::Log { record } = &entry.event else {
                    continue;
                };
                let Some(attribution) = self.current_attribution(entry.cursor.position.0)? else {
                    continue;
                };
                let mut projected = record.clone();
                attribution.apply_to(&mut projected);
                if single_record_page_fits(
                    &HistoryRecord {
                        cursor: entry.cursor.clone(),
                        boot_id: entry.boot_id.clone(),
                        recorded_at_ms: entry.recorded_at_ms,
                        startup: entry.startup,
                        event: HistoryEvent::Log { record: projected },
                    },
                    venera_debug_protocol::v1::MAX_FRAME_BYTES - 1024,
                )? {
                    snapshots.insert(entry.cursor.position.0, attribution);
                }
            }
        }
        encoding
            .encode_page(page, |cursor| Ok(snapshots.remove(&cursor.position.0)))
            .map_err(|error| invalid(&error.message))
    }
}
