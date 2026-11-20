// SPDX-License-Identifier: MIT
use super::*;
use serde_json::{json, Value};
use venera_debug_protocol::v1::{HistoryCursor, HistoryEvent, HistoryPage, HistoryRange, HistoryRead, Method};

pub(crate) fn range() -> io::Result<HistoryRange> {
    let (mut rpc, mut file) = rpc_connect()?;
    Ok(serde_json::from_value(rpc_call(&mut rpc, &mut file, Method::HistoryRange, json!({}))?)?)
}

pub(crate) fn observe(before: io::Result<HistoryRange>, pid: u32, marker: &str) -> (bool, Value) {
    let before = match before {
        Ok(range) => range,
        Err(error) => return (false, json!({"status":"baseline-rpc-error", "error":error.to_string(), "pid":pid, "marker":marker})),
    };
    let mut after = HistoryCursor { journal_id: before.journal_id.clone(), position: before.high_water };
    let mut etw = 0;
    let mut dbwin = 0;
    let mut candidates = Vec::new();
    let mut missing = Vec::new();
    let mut gaps = Vec::new();
    let mut problem = None;
    let deadline = Instant::now() + Duration::from_secs(90);
    let result = (|| -> io::Result<()> {
        let (mut rpc, mut file) = rpc_connect()?;
        while Instant::now() < deadline {
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
                    if entry.cursor.position.0 <= after.position.0 { continue; }
                    match &entry.event {
                        HistoryEvent::Log { record } if record.process.as_ref().is_some_and(|process| process.pid == pid) && record.text.contains(marker) => {
                            if record.text == marker {
                                etw += u32::from(record.source == "etw" && record.component == "venera_d3d11"
                                    && record.structured.as_ref().is_some_and(|event| event.event_id == 5));
                                dbwin += u32::from(record.source == "dbwin" && record.component.is_empty() && record.structured.is_none());
                            }
                            candidates.push(entry);
                        }
                        HistoryEvent::Gap { .. } => gaps.push(entry),
                        _ => (),
                    }
                }
                after = page.next;
            }
            if etw > 0 && dbwin > 0 { return Ok(()); }
            std::thread::sleep(Duration::from_millis(400));
        }
        Ok(())
    })();
    if let Err(error) = result { problem = Some(error.to_string()); }
    let pass = problem.is_none() && candidates.len() == 2 && etw == 1 && dbwin == 1;
    (pass, json!({"status":if pass {"pair-observed"} else {"pair-incomplete"}, "error":problem,
        "pid":pid, "marker":marker, "before":before, "scanned_through":after,
        "etw_matches":etw, "dbwin_matches":dbwin, "candidates":candidates, "missing":missing,"gaps":gaps}))
}
