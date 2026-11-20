// SPDX-License-Identifier: MIT
use super::*;
use serde_json::{json, Value};
use venera_debug_protocol::v1::{
    HistoryCursor, HistoryEvent, HistoryPage, HistoryRange, HistoryRead, HistoryRecord, Method,
};

pub(crate) fn range() -> io::Result<HistoryRange> {
    let (mut rpc, mut file) = rpc_connect()?;
    Ok(serde_json::from_value(rpc_call(&mut rpc, &mut file, Method::HistoryRange, json!({}))?)?)
}

pub(crate) fn observe(
    before: HistoryRange,
    matrix_pids: [u32; 2],
    pid: u32,
    marker: &str,
    fence: &str,
    ods_returned_filetime: u64,
) -> (bool, Value, Vec<HistoryRecord>) {
    let mut after = HistoryCursor { journal_id: before.journal_id.clone(), position: before.high_water };
    let mut originals = Vec::new();
    let mut fences = Vec::new();
    let mut records = Vec::new();
    let mut missing = Vec::new();
    let mut gaps = Vec::new();
    let mut settle_until = None;
    let mut settled = false;
    let deadline = Instant::now() + Duration::from_secs(90);
    let result = (|| -> io::Result<()> {
        let (mut rpc, mut file) = rpc_connect()?;
        while Instant::now() < deadline {
            let final_scan = settle_until.is_some_and(|until| Instant::now() >= until);
            let high: HistoryRange = serde_json::from_value(rpc_call(&mut rpc, &mut file, Method::HistoryRange, json!({}))?)?;
            if high.journal_id != after.journal_id || high.high_water.0 < after.position.0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("history changed: {high:?}")));
            }
            while after.position.0 < high.high_water.0 {
                if Instant::now() >= deadline { return Ok(()); }
                let request = HistoryRead { after: Some(after.clone()), through: Some(high.high_water), limit: 128 };
                let page: HistoryPage = serde_json::from_value(rpc_call(&mut rpc, &mut file, Method::HistoryRead, serde_json::to_value(request)?)?)?;
                if page.next.journal_id != after.journal_id || page.next.position.0 <= after.position.0 || page.next.position.0 > high.high_water.0 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("history cursor did not cover requested positions: {:?}", page.next)));
                }
                missing.extend(page.missing);
                for entry in page.records {
                    match &entry.event {
                        HistoryEvent::Log { record } => {
                            let record_pid = record.process.as_ref().map(|p| p.pid);
                            // A run-specific token also identifies an ODS record whose PID was unresolved.
                            if record.text.contains(marker) {
                                originals.push(entry);
                            } else if record.text.contains(fence) {
                                if record.text == fence && record_pid == Some(pid) && is_d11_etw(record) {
                                    settle_until.get_or_insert_with(|| Instant::now() + Duration::from_secs(3));
                                }
                                fences.push(entry);
                            } else if record_pid.is_some_and(|p| matrix_pids.contains(&p)) {
                                records.push(entry);
                            }
                        }
                        HistoryEvent::Gap { gap } if !gap.is_history_attribution_marker() => gaps.push(entry),
                        HistoryEvent::Gap { .. } => (),
                    }
                }
                after = page.next;
            }
            // Read a fresh high-water snapshot after the settling interval.
            if final_scan {
                settled = true;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        Ok(())
    })();
    let find_etw = |entries: &[HistoryRecord], text| {
        entries.iter().filter_map(|entry| match &entry.event {
            HistoryEvent::Log { record } if record.text == text && is_d11_etw(record)
                && record.process.as_ref().is_some_and(|p| p.pid == pid) =>
                Some((entry.cursor.position.0, record.structured.as_ref().unwrap().source_time_filetime.0)),
            _ => None,
        }).collect::<Vec<_>>()
    };
    let etw = find_etw(&originals, marker);
    let fence_etw = find_etw(&fences, fence);
    let dbwin = originals.iter().filter(|entry| matches!(&entry.event,
        HistoryEvent::Log { record } if record.source == "dbwin")).count();
    let ordered = matches!((etw.as_slice(), fence_etw.as_slice()), ([(pos, time)], [(fence_pos, fence_time)])
        if pos < fence_pos && *time <= ods_returned_filetime && ods_returned_filetime < *fence_time);
    let pass = result.is_ok() && settled && missing.is_empty() && gaps.is_empty()
        && originals.len() == 1 && fences.len() == 1 && dbwin == 0 && ordered;
    let report = json!({"status":if pass {"observed"} else {"incomplete"},
        "error":result.err().map(|error| error.to_string()), "pid":pid, "marker":marker, "fence":fence,
        "ods_returned_filetime":ods_returned_filetime.to_string(),
        "before":before, "scanned_through":after, "settled":settled, "ordered":ordered,
        "etw_matches":etw.len(), "dbwin_matches":dbwin, "fence_matches":fence_etw.len(),
        "candidates":originals, "fences":fences, "missing":missing,"gaps":gaps});
    (pass, report, records)
}

fn is_d11_etw(record: &venera_debug_protocol::v1::LogRecord) -> bool {
    record.source == "etw" && record.component == "venera_d3d11"
        && record.structured.as_ref().is_some_and(|event| event.event_id == 5)
}
