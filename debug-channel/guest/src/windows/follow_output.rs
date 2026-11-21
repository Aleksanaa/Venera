use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::Arc,
};
use venera_debug_protocol::{
    client::{HistoryConsumer, Start},
    matches_filter,
    v1::*,
    Filter,
};

pub(super) enum FollowPage {
    Compatibility(HistoryPage),
    Attribution(AttributionHistoryPage),
}

impl FollowPage {
    pub(super) fn decode(
        consumer: &mut HistoryConsumer,
        encoding: HistoryEncoding,
        value: Value,
    ) -> io::Result<Self> {
        match encoding {
            HistoryEncoding::Attribution => {
                let page = serde_json::from_value(value)?;
                consumer.attribution_page(&page).map_err(invalid)?;
                Ok(Self::Attribution(page))
            }
            HistoryEncoding::Compatibility => {
                let page = serde_json::from_value(value)?;
                consumer.page(&page).map_err(invalid)?;
                Ok(Self::Compatibility(page))
            }
        }
    }
}

// The Panel and its feed keep the same bounded window of candidate rows.
const WINDOW: usize = 20_000;

#[derive(Default)]
pub(super) struct PanelOutput {
    journal: String,
    rows: BTreeMap<u64, Option<(LogRecord, Arc<Filter>, DecimalU64)>>,
}

pub(super) fn invalid(error: RpcError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.message)
}

impl PanelOutput {
    pub(super) fn page(
        &mut self,
        consumer: &mut HistoryConsumer,
        page: &AttributionHistoryPage,
        filter: Filter,
        output: &mut impl Write,
    ) -> io::Result<()> {
        consumer.attribution_page(page).map_err(invalid)?;
        if self.journal != page.range.journal_id {
            self.rows.clear();
            self.journal.clone_from(&page.range.journal_id);
        }
        let filter = Arc::new(filter);
        let mut entries = BTreeMap::new();
        for missing in &page.missing {
            entries.insert(
                missing.first.0,
                (
                    None,
                    json!({"type":"gap","reason":missing.reason,
                "first_position":missing.first,"last_position":missing.last}),
                ),
            );
        }
        for entry in &page.records {
            let cursor = &entry.cursor;
            let item = match &entry.event {
                AttributionHistoryEvent::Log { record } => {
                    if (!filter.components.is_empty()
                        && !filter.components.contains(&record.component))
                        || (!filter.levels.is_empty() && !filter.levels.contains(&record.level))
                    {
                        continue;
                    }
                    let mut value = serde_json::to_value(record)?;
                    value["type"] = json!("log");
                    value["cursor"] = json!(cursor);
                    value["visible"] = json!(matches_filter(&filter, record, &record.ancestors));
                    // Keep just what matching requires; the body lives in the Panel row.
                    let matching = LogRecord {
                        stream_id: String::new(),
                        seq: 0,
                        observed_at: String::new(),
                        source_time: None,
                        source: String::new(),
                        component: record.component.clone(),
                        level: record.level.clone(),
                        raw_level: String::new(),
                        session_id: None,
                        process: record.process.clone(),
                        ancestors: record.ancestors.clone(),
                        tid: None,
                        text: String::new(),
                        structured: None,
                    };
                    (Some((matching, filter.clone(), cursor.position)), value)
                }
                AttributionHistoryEvent::Gap { gap } => {
                    (None, json!({"type":"gap","reason":gap.reason}))
                }
                AttributionHistoryEvent::AttributionRevision { .. } => continue,
            };
            entries.insert(cursor.position.0, item);
        }
        // Logs precede every revision of their cursor, including revisions on the same page.
        for (position, (row, value)) in entries {
            self.rows.insert(position, row);
            if self.rows.len() > WINDOW {
                self.rows.pop_first();
            }
            write_line(output, &value)?;
        }
        let mut revisions: Vec<_> = page.records.iter().collect();
        revisions.sort_by_key(|entry| entry.cursor.position.0);
        for entry in revisions {
            if let AttributionHistoryEvent::AttributionRevision {
                target,
                attribution,
            } = &entry.event
            {
                self.revise(target, entry.cursor.position, attribution, output)?;
            }
        }
        output.flush()?;
        consumer.consumed().map_err(invalid)?;
        Ok(())
    }

    pub(super) fn recovery_bounds(&self, journal: &str) -> Option<(u64, u64)> {
        if self.journal != journal {
            return None;
        }
        let mut logs = self.rows.iter().filter(|(_, row)| row.is_some());
        let (&first, _) = logs.next()?;
        Some((first, logs.next_back().map_or(first, |(&last, _)| last)))
    }

    pub(super) fn recover(
        &mut self,
        mut range: HistoryRange,
        (first, last): (u64, u64),
        output: &mut impl Write,
        mut read: impl FnMut(HistoryRead) -> io::Result<HistoryPage>,
    ) -> io::Result<()> {
        if range.journal_id != self.journal || range.oldest.0 > last {
            return Ok(());
        }
        let after = HistoryCursor {
            journal_id: range.journal_id.clone(),
            position: DecimalU64(first.max(range.oldest.0) - 1),
        };
        range.high_water.0 = range.high_water.0.min(last);
        let mut reader = HistoryConsumer::new(Start::Beginning);
        reader.range(range, Some(after)).map_err(invalid)?;
        while !reader.caught_up() {
            let page = read(reader.read_request())?;
            reader.page(&page).map_err(invalid)?;
            self.snapshot(&page, output)?;
            reader.consumed().map_err(invalid)?;
        }
        Ok(())
    }

    fn snapshot(&mut self, page: &HistoryPage, output: &mut impl Write) -> io::Result<()> {
        for entry in &page.records {
            if let HistoryEvent::Log { record } = &entry.event {
                if let Some(process) = &record.process {
                    let snapshot = HistoryAttribution {
                        process: process.clone(),
                        ancestors: record.ancestors.clone(),
                    };
                    if snapshot.is_resolved() {
                        // The compatibility projection and high-water mark share one read view.
                        self.revise(&entry.cursor, page.range.high_water, &snapshot, output)?;
                    }
                }
            }
        }
        output.flush()
    }

    fn revise(
        &mut self,
        target: &HistoryCursor,
        through: DecimalU64,
        attribution: &HistoryAttribution,
        output: &mut impl Write,
    ) -> io::Result<()> {
        if target.journal_id == self.journal {
            if let Some(Some((record, filter, applied))) = self.rows.get_mut(&target.position.0) {
                if through <= *applied {
                    return Ok(());
                }
                *applied = through;
                if record.process.as_ref() != Some(&attribution.process)
                    || record.ancestors != attribution.ancestors
                {
                    attribution.apply_to(record);
                    write_line(
                        output,
                        &json!({"type":"attribution_revision","target":target,
                        "attribution":attribution,"visible":matches_filter(filter, record, &record.ancestors)}),
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn write_line(output: &mut impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")
}
