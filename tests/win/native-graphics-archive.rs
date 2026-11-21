// SPDX-License-Identifier: MIT
#![allow(dead_code)]
#[path = "../host/src/store.rs"]
mod store;

use std::{fs, path::Path};
use venera_debug_protocol::v1::{HistoryEvent, HistoryPage, HistoryRecord};

fn main() -> store::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("Usage: native-graphics-archive HISTORY-PAGE.JSON NEW-ARCHIVE-DIR".into());
    }
    let page: HistoryPage = serde_json::from_slice(&fs::read(&args[0])?)?;
    let shader = "mov r0, r1\n".repeat(8000);
    let vkd = format!("compile: {shader}");
    for (id, body) in [(7, shader), (8, vkd)] {
        assert_eq!(page.records.iter().filter(|entry| matches!(&entry.event,
            HistoryEvent::Log { record } if record.source == "etw" && record.text == body
                && record.structured.as_ref().is_some_and(|event| event.event_id == id))).count(), 2);
    }
    let dir = Path::new(&args[1]);
    fs::create_dir(dir)?;
    let path = dir.join("archive.sqlite");
    let archive = store::Store::open(&path, 8 * 1024 * 1024)?;
    assert_eq!(archive.append_history_page(&page)?, page.next);
    assert_eq!(archive.append_history_page(&page)?, page.next);
    drop(archive);
    let archive = store::Store::open(&path, 8 * 1024 * 1024)?;
    assert_eq!(archive.history_cursor(&page.range.journal_id)?, Some(page.next));
    let mut export = Vec::new();
    store::export(&path, &mut export, &store::ExportRange::default())?;
    let got: Vec<HistoryRecord> = std::str::from_utf8(&export)?.lines().map(|line| {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        serde_json::from_value(row["message"].clone()).unwrap()
    }).collect();
    assert_eq!(got, page.records);
    fs::write(dir.join("archive.ndjson"), export)?;
    println!("Archived and exported {} Windows history records", got.len());
    Ok(())
}
