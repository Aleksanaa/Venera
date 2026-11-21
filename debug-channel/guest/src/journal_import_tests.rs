const BOOT: u64 = 134_393_633_875_000_000;
const NOW: u64 = 1_794_889_440_000;

#[cfg(windows)]
#[test]
#[ignore = "Measures ReplaceFileW publication and receipt recovery with held metadata readers"]
fn replace_file_candidate_with_metadata_readers() {
    use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        ReplaceFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    for sharing in [
        None,
        Some(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE),
        Some(FILE_SHARE_READ | FILE_SHARE_WRITE),
    ] {
        let dir = Directory::new();
        let mut journal = dir.open(RetentionPolicy::default());
        let source = source("replacement-candidate");
        journal.discover_imports(vec![source.clone()]).unwrap();
        journal
            .import_batch(ImportBatch {
                source: source.clone(),
                through: 2,
                complete: false,
                entries: Vec::new(),
                now_ms: NOW,
            })
            .unwrap();
        let target = dir.0.join("metadata.json");
        let pending = dir.0.join("metadata.pending");
        let old = fs::read(&target).unwrap();
        let mut metadata = journal.metadata.clone();
        metadata.imports[0].complete = true;
        let new = serde_json::to_vec(&metadata).unwrap();
        let mut file = File::create(&pending).unwrap();
        file.write_all(&new).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let mut reader = sharing.map(|mode| {
            OpenOptions::new()
                .read(true)
                .share_mode(mode)
                .open(&target)
                .unwrap()
        });
        let target_w: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        let pending_w: Vec<u16> = pending.as_os_str().encode_wide().chain(Some(0)).collect();
        let result = unsafe {
            ReplaceFileW(
                target_w.as_ptr(),
                pending_w.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        let error = (result == 0).then(io::Error::last_os_error);
        println!(
            "ReplaceFileW sharing={sharing:?} error={error:?} target_exists={} pending_exists={}",
            target.exists(),
            pending.exists()
        );
        if sharing == Some(FILE_SHARE_READ | FILE_SHARE_WRITE) {
            assert!(matches!(error.unwrap().raw_os_error(), Some(5 | 32 | 1175)));
            assert_eq!(fs::read(&target).unwrap(), old);
            assert_eq!(fs::read(&pending).unwrap(), new);
        } else {
            assert!(error.is_none(), "candidate publication: {error:?}");
            assert_eq!(fs::read(&target).unwrap(), new);
            assert!(!pending.exists());
            let published = OpenOptions::new().write(true).open(&target).unwrap();
            published.sync_all().unwrap();
            println!("ReplaceFileW sharing={sharing:?} published file sync succeeded");
        }
        if let Some(reader) = reader.as_mut() {
            let mut observed = Vec::new();
            reader.read_to_end(&mut observed).unwrap();
            assert_eq!(observed, old);
            println!("ReplaceFileW sharing={sharing:?} held reader retains prior receipt bytes");
        }
        drop(reader);
        drop(journal);
        let mut reopened = dir.open(RetentionPolicy::default());
        let receipt = reopened.import_receipt(&source).unwrap();
        assert_eq!(
            receipt.complete,
            sharing != Some(FILE_SHARE_READ | FILE_SHARE_WRITE)
        );
        assert_eq!(receipt.through, 2);
        assert_eq!(reopened.range().unwrap().high_water.0, 0);
        let completed = reopened.complete_import(&source, None, BOOT, NOW).unwrap();
        assert!(completed.complete);
        assert_eq!(completed.through, 2);
        reopened.close().unwrap();
        let reopened = dir.open(RetentionPolicy::default());
        assert!(reopened.import_receipt(&source).unwrap().complete);
        assert_eq!(reopened.range().unwrap().high_water.0, 0);
        println!(
            "ReplaceFileW sharing={sharing:?} reopen receipt complete=true through=2 high_water=0"
        );
    }
}

#[cfg(windows)]
#[test]
#[ignore = "Compares import completion with metadata reader sharing modes"]
fn completion_commit_with_metadata_readers() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    for sharing in [
        None,
        Some(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE),
        Some(FILE_SHARE_READ | FILE_SHARE_WRITE),
    ] {
        let dir = Directory::new();
        let mut journal = dir.open(RetentionPolicy::default());
        let source = source("completion-probe");
        journal.discover_imports(vec![source.clone()]).unwrap();
        journal
            .import_batch(ImportBatch {
                source: source.clone(),
                through: 2,
                complete: false,
                entries: Vec::new(),
                now_ms: NOW,
            })
            .unwrap();
        let reader = sharing.map(|mode| {
            fs::OpenOptions::new()
                .read(true)
                .share_mode(mode)
                .open(dir.0.join("metadata.json"))
                .unwrap()
        });
        let result = journal.complete_import(&source, None, BOOT, NOW);
        println!("sharing={sharing:?} completion={result:?}");
        let committed: Value =
            serde_json::from_slice(&fs::read(dir.0.join("metadata.json")).unwrap()).unwrap();
        let pending = fs::read(dir.0.join("metadata.pending"))
            .ok()
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).unwrap());
        println!(
            "committed_receipts={} pending_receipts={}",
            committed["imports"],
            pending
                .as_ref()
                .map(|value| value["imports"].to_string())
                .unwrap_or_else(|| "absent".into())
        );
        if let Err(error) = result {
            assert!(sharing.is_some(), "unobserved completion: {error}");
            assert!(matches!(error.raw_os_error(), Some(5 | 32)));
            assert_eq!(committed["imports"][0]["complete"], false);
            assert_eq!(pending.unwrap()["imports"][0]["complete"], true);
            let replacement = replace_metadata(
                &dir.0.join("metadata.pending"),
                &dir.0.join("metadata.json"),
            );
            println!("sharing={sharing:?} direct MoveFileExW replacement={replacement:?}");
            assert_eq!(
                replacement.unwrap_err().raw_os_error(),
                error.raw_os_error()
            );
            assert!(journal
                .range()
                .unwrap_err()
                .to_string()
                .contains("reopen journal"));
        } else {
            assert_ne!(sharing, Some(FILE_SHARE_READ | FILE_SHARE_WRITE));
            let receipt = result.unwrap();
            assert!(receipt.complete);
            assert_eq!(receipt.through, 2);
            assert_eq!(committed["imports"][0]["complete"], true);
        }
        drop(reader);
        drop(journal);
        let mut reopened = dir.open(RetentionPolicy::default());
        let receipt = reopened.complete_import(&source, None, BOOT, NOW).unwrap();
        assert!(receipt.complete);
        assert_eq!(receipt.through, 2);
        assert_eq!(reopened.range().unwrap().high_water.0, 0);
    }
}

#[test]
#[ignore = "Replays a preserved pending completion in a private journal copy"]
fn preserved_receipt_completion_replay() {
    let original = std::path::PathBuf::from(std::env::var_os("VENERA_JOURNAL_DIAGNOSTIC").unwrap());
    let dir = Directory::new();
    for file in fs::read_dir(&original).unwrap() {
        let file = file.unwrap();
        assert!(file.file_type().unwrap().is_file());
        fs::copy(file.path(), dir.0.join(file.file_name())).unwrap();
    }
    let before: Value =
        serde_json::from_slice(&fs::read(dir.0.join("metadata.json")).unwrap()).unwrap();
    let pending: Value =
        serde_json::from_slice(&fs::read(dir.0.join("metadata.pending")).unwrap()).unwrap();
    assert_eq!(before["high_water"], 151567);
    assert_eq!(before["imports"][0]["complete"], false);
    assert_eq!(pending["imports"][0]["complete"], true);
    let mut journal = dir.open(RetentionPolicy {
        max_bytes: crate::capture_storage::JOURNAL_BYTES,
        ..Default::default()
    });
    let sources = journal.import_sources().unwrap();
    assert_eq!(sources.len(), 1);
    let source = &sources[0];
    assert_eq!(
        source.id,
        "3330617030:851968:824982:31291068:1324273529:134394002549120122:134394002545000000"
    );
    let receipt = journal.import_receipt(source).unwrap();
    assert_eq!(receipt.through, 2);
    assert!(!receipt.complete);
    let result = journal.complete_import(source, None, source.boot_filetime, 1_794_937_904_547);
    println!("preserved completion result={result:?}");
    let receipt = result.unwrap();
    assert!(receipt.complete);
    assert_eq!(receipt.through, 2);
    assert_eq!(journal.range().unwrap().high_water.0, 151567);
    journal.close().unwrap();
    let reopened = dir.open(RetentionPolicy {
        max_bytes: crate::capture_storage::JOURNAL_BYTES,
        ..Default::default()
    });
    assert!(reopened.import_receipt(source).unwrap().complete);
    assert_eq!(reopened.range().unwrap().high_water.0, 151567);
}

#[test]
fn missing_sources_release_receipts_and_preserve_gap_history() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    for index in 0..140 {
        journal
            .discover_imports(vec![source(&format!("missing-{index}"))])
            .unwrap();
        journal
            .reconcile_imports(&[], BOOT, NOW + 1_000_000)
            .unwrap();
    }
    assert_eq!(journal.range().unwrap().high_water.0, 140);
    assert_eq!(logs(&journal).len(), 140);
    let HistoryEvent::Gap { gap } = &logs(&journal)[139].event else {
        panic!("missing-source diagnostic")
    };
    assert_eq!(gap.reason, "capture_source_missing; source=missing-139");
    assert_eq!(gap.count, None);
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .discover_imports(vec![source("next-available")])
        .unwrap();
    assert_eq!(
        journal
            .import_receipt(&source("next-available"))
            .unwrap()
            .through,
        0
    );
    assert_eq!(journal.range().unwrap().high_water.0, 140);
}

#[test]
fn active_and_header_pending_files_keep_their_receipts_until_absence_is_confirmed() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    let file = source("1:2:3:4:5:100:134393633875000000");
    journal.discover_imports(vec![file.clone()]).unwrap();
    journal
        .reconcile_imports(&["1:2:3:4:5".into()], BOOT, NOW + 1_000_000)
        .unwrap();
    assert_eq!(journal.import_receipt(&file).unwrap().complete, false);
    assert_eq!(journal.range().unwrap().high_water.0, 0);
    journal
        .reconcile_imports(&[], BOOT, NOW + 1_000_000)
        .unwrap();
    assert_eq!(logs(&journal).len(), 1);
    assert!(journal.import_receipt(&file).is_err());
}

#[test]
fn trace_loss_completes_after_valid_records_and_persists_its_count() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    let input = source("primary-loss");
    journal.discover_imports(vec![input.clone()]).unwrap();
    journal
        .import_batch(batch(
            "primary-loss",
            1,
            false,
            vec![ImportEntry {
                ordinal: 1,
                source_filetime: BOOT + 1,
                event: structured(1, "etw"),
            }],
        ))
        .unwrap();
    journal
        .complete_import(
            &input,
            Some(("capture_loss events=5".into(), Some(5))),
            BOOT,
            NOW + 1_000_000,
        )
        .unwrap();
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .complete_import(
            &input,
            Some(("capture_loss events=5".into(), Some(5))),
            BOOT,
            NOW + 1_000_000,
        )
        .unwrap();
    let records = logs(&journal);
    assert_eq!(records.len(), 2);
    let HistoryEvent::Log { record } = &records[0].event else {
        panic!("valid source record")
    };
    assert_eq!(record.text, "equal text");
    let HistoryEvent::Gap { gap } = &records[1].event else {
        panic!("loss diagnostic")
    };
    assert_eq!(gap.count, Some(DecimalU64(5)));
    assert_eq!(gap.reason, "capture_loss events=5; source=primary-loss");
    assert_eq!(journal.import_receipt(&input).unwrap().complete, true);
}

#[test]
fn terminal_diagnostic_recovers_once_after_metadata_write_failure() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    let input = ImportSource {
        id: "partial-recorder".into(),
        boot_filetime: 0,
    };
    journal.discover_imports(vec![input.clone()]).unwrap();
    fs::create_dir(dir.0.join("metadata.pending")).unwrap();
    assert!(journal
        .complete_import(
            &input,
            Some(("capture_header_failed".into(), None)),
            BOOT,
            NOW + 1_000_000
        )
        .is_err());
    drop(journal);
    fs::remove_dir(dir.0.join("metadata.pending")).unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    assert!(!journal.import_receipt(&input).unwrap().complete);
    journal
        .complete_import(
            &input,
            Some(("capture_header_failed".into(), None)),
            BOOT,
            NOW + 1_000_000,
        )
        .unwrap();
    assert_eq!(logs(&journal).len(), 1);
    journal.forget_import(&input).unwrap();
    drop(journal);
    let journal = dir.open(RetentionPolicy::default());
    assert_eq!(logs(&journal).len(), 1);
    let metadata: Value =
        serde_json::from_slice(&fs::read(dir.0.join("metadata.json")).unwrap()).unwrap();
    assert_eq!(metadata["recent_boots"], json!(["134393633875000000"]));
}

#[test]
fn structured_tail_recovery_joins_live_and_recorder_identity() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source("overlap")]).unwrap();
    let metadata = fs::read(dir.0.join("metadata.json")).unwrap();
    journal
        .append_batch(vec![JournalInput {
            boot_id: BOOT.to_string(),
            recorded_at_ms: NOW,
            startup: false,
            event: structured(1, "etw"),
        }])
        .unwrap();
    drop(journal);
    fs::write(dir.0.join("metadata.json"), metadata).unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .import_batch(batch(
            "overlap",
            2,
            true,
            vec![
                ImportEntry {
                    ordinal: 1,
                    source_filetime: BOOT + 1,
                    event: structured(1, "wpp_recorder"),
                },
                ImportEntry {
                    ordinal: 2,
                    source_filetime: BOOT + 1,
                    event: structured(2, "wpp_recorder"),
                },
            ],
        ))
        .unwrap();
    assert_eq!(journal.range().unwrap().high_water.0, 2);
    assert_eq!(
        logs(&journal)
            .iter()
            .map(|r| match &r.event {
                HistoryEvent::Log { record } =>
                    record.structured.as_ref().unwrap().producer_sequence.0,
                _ => panic!("log"),
            })
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn committed_records_recover_progress_before_receipt_completion() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source("committed")]).unwrap();
    journal
        .import_batch(batch("committed", 4, false, vec![imported(1), imported(2)]))
        .unwrap();
    drop(journal);
    let path = dir.0.join("metadata.json");
    let mut metadata: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    metadata["imports"][0]["through"] = json!(0);
    fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    assert_eq!(
        journal
            .import_receipt(&source("committed"))
            .unwrap()
            .through,
        2
    );
    journal
        .import_batch(batch("committed", 4, true, vec![imported(1), imported(2)]))
        .unwrap();
    assert_eq!(logs(&journal).len(), 2);
    journal
        .reconcile_imports(&[], BOOT, NOW + 1_000_000)
        .unwrap();
    assert!(journal.import_receipt(&source("committed")).is_err());
    assert_eq!(logs(&journal).len(), 2);
}

#[test]
fn original_time_controls_startup_and_runtime_retention() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source("times")]).unwrap();
    let now_ms = 1794889787500 + 120_000 + 5 * 60 * 60 * 1000 + 1;
    journal
        .import_batch(ImportBatch {
            source: source("times"),
            through: 3,
            complete: true,
            now_ms,
            entries: vec![
                ImportEntry {
                    ordinal: 1,
                    source_filetime: BOOT + 120 * 10_000_000 - 1,
                    event: event("boundary"),
                },
                ImportEntry {
                    ordinal: 2,
                    source_filetime: BOOT + 120 * 10_000_000,
                    event: event("expired"),
                },
                ImportEntry {
                    ordinal: 3,
                    source_filetime: BOOT + 120 * 10_000_000 - 1,
                    event: event("boundary"),
                },
            ],
        })
        .unwrap();
    let records = logs(&journal);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].recorded_at_ms.0, 1794889907499);
    assert_eq!(records[1].recorded_at_ms.0, 1794889907499);
    assert_eq!(records[0].startup, true);
    assert_eq!(records[1].cursor.position.0, 2);
}

#[test]
fn invalid_event_context_leaves_the_import_retryable() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source("invalid")]).unwrap();
    let invalid = ImportEntry {
        ordinal: 2,
        source_filetime: BOOT + 2,
        event: structured(1, "etw"),
    };
    assert!(journal
        .import_batch(batch("invalid", 2, true, vec![imported(1), invalid]))
        .is_err());
    assert_eq!(
        journal.import_receipt(&source("invalid")).unwrap().through,
        0
    );
    assert_eq!(journal.range().unwrap().high_water.0, 0);
    journal
        .import_batch(batch("invalid", 1, true, vec![imported(1)]))
        .unwrap();
    assert_eq!(logs(&journal).len(), 1);
}

#[test]
fn owner_persistence_error_keeps_the_source_available_for_recovery() {
    use std::sync::{atomic::AtomicBool, Arc};
    let dir = Directory::new();
    let path = dir.0.join("original.etl");
    fs::write(&path, "original capture").unwrap();
    let journal = dir.open(RetentionPolicy::default());
    let stop = Arc::new(AtomicBool::new(false));
    let (history, worker) = crate::history::History::start(journal, stop.clone()).unwrap();
    history.discover_imports(vec![source("failed")]).unwrap();
    fs::create_dir(dir.0.join("metadata.pending")).unwrap();
    assert!(history
        .import_batch(batch("failed", 1, true, vec![imported(1)]))
        .is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "original capture");
    stop.store(true, Ordering::Release);
    assert!(worker.join().unwrap().is_err());
    fs::remove_dir(dir.0.join("metadata.pending")).unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .import_batch(batch("failed", 1, true, vec![imported(1)]))
        .unwrap();
    assert_eq!(logs(&journal).len(), 1);
}

fn source(id: &str) -> ImportSource {
    ImportSource {
        id: id.into(),
        boot_filetime: BOOT,
    }
}

fn imported(ordinal: u64) -> ImportEntry {
    ImportEntry {
        ordinal,
        source_filetime: BOOT + ordinal,
        event: event("equal text"),
    }
}

fn batch(id: &str, through: u64, complete: bool, entries: Vec<ImportEntry>) -> ImportBatch {
    ImportBatch {
        source: source(id),
        through,
        complete,
        entries,
        now_ms: NOW,
    }
}

fn logs(journal: &Journal) -> Vec<HistoryRecord> {
    journal
        .read(
            HistoryRead {
                after: None,
                through: None,
                limit: 256,
            },
            1024 * 1024,
        )
        .unwrap()
        .records
}

fn structured(sequence: u64, transport: &str) -> HistoryEvent {
    serde_json::from_value(json!({"type":"log", "record": {
        "stream_id":"stream", "seq":"1", "observed_at":"2026-11-17T07:04:00Z", "source":transport,
        "component":"kernel", "level":"info", "raw_level":"info", "text":"equal text",
        "structured": {"provider":"6c5a2378-22b9-4c10-a91f-69e34c280b81", "event_id":2, "version":1,
            "boot_time_filetime":BOOT.to_string(), "producer_instance":"41", "producer_sequence":sequence.to_string(),
            "source_time_filetime":(BOOT + 1).to_string(), "fields":{}}
    }})).unwrap()
}

#[test]
fn complete_tail_recovers_import_progress_before_retrying_legacy_records() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source("sealed-1")]).unwrap();
    let metadata = fs::read(dir.0.join("metadata.json")).unwrap();
    let mut legacy: Value = serde_json::from_slice(&metadata).unwrap();
    legacy.as_object_mut().unwrap().remove("segments");
    let metadata = serde_json::to_vec(&legacy).unwrap();
    assert_eq!(
        journal
            .import_batch(batch("sealed-1", 7, false, vec![imported(2), imported(5)]))
            .unwrap()
            .through,
        7
    );
    drop(journal);
    fs::write(dir.0.join("metadata.json"), metadata).unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    assert_eq!(
        journal.import_receipt(&source("sealed-1")).unwrap().through,
        5
    );
    let receipt = journal
        .import_batch(batch(
            "sealed-1",
            9,
            true,
            vec![imported(2), imported(5), imported(8)],
        ))
        .unwrap();
    assert_eq!((receipt.through, receipt.complete), (9, true));
    let records = logs(&journal);
    assert_eq!(
        records
            .iter()
            .map(|r| r.cursor.position.0)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(records[0].boot_id, "134393633875000000");
    assert_eq!(records[0].recorded_at_ms.0, 1794889787500);
    let path = journal.segment_files().unwrap()[0].1.clone();
    let first: Value =
        serde_json::from_str(fs::read_to_string(path).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(first["provenance"]["ordinal"], 2);
    assert_eq!(
        first["provenance"]["source_filetime"],
        134393633875000002u64
    );
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    assert_eq!(
        journal
            .import_batch(batch(
                "sealed-1",
                9,
                true,
                vec![imported(2), imported(5), imported(8)]
            ))
            .unwrap()
            .through,
        9
    );
    assert_eq!(logs(&journal).len(), 3);
}

#[test]
fn live_and_recorder_share_logical_identity_in_both_arrival_orders() {
    for import_first in [false, true] {
        let dir = Directory::new();
        let mut journal = dir.open(RetentionPolicy::default());
        journal.discover_imports(vec![source("recorder")]).unwrap();
        let live = || JournalInput {
            boot_id: "observed-boot".into(),
            recorded_at_ms: NOW,
            startup: false,
            event: structured(1, "etw"),
        };
        if !import_first {
            journal.append_batch(vec![live()]).unwrap();
        }
        journal
            .import_batch(batch(
                "recorder",
                2,
                true,
                vec![
                    ImportEntry {
                        ordinal: 1,
                        source_filetime: BOOT + 1,
                        event: structured(1, "wpp_recorder"),
                    },
                    ImportEntry {
                        ordinal: 2,
                        source_filetime: BOOT + 1,
                        event: structured(2, "wpp_recorder"),
                    },
                ],
            ))
            .unwrap();
        drop(journal);
        let mut journal = dir.open(RetentionPolicy::default());
        journal.append_batch(vec![live(), live()]).unwrap();
        let records = logs(&journal);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].boot_id, "134393633875000000");
        assert_eq!(records[0].startup, true);
        assert_eq!(records[0].recorded_at_ms.0, 1794889787500);
        assert_eq!(
            records
                .iter()
                .map(|r| match &r.event {
                    HistoryEvent::Log { record } =>
                        record.structured.as_ref().unwrap().producer_sequence.0,
                    _ => panic!("log"),
                })
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }
}

#[test]
fn header_inventory_includes_empty_boots_before_retention() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .append(&BOOT.to_string(), NOW, true, event("first boot"))
        .unwrap();
    let sources: Vec<_> = (0..4)
        .rev()
        .map(|i| ImportSource {
            id: format!("boot-{i}"),
            boot_filetime: BOOT + i * 10_000_000,
        })
        .collect();
    journal.discover_imports(sources.clone()).unwrap();
    for item in sources.iter().cloned() {
        journal
            .import_batch(ImportBatch {
                source: item,
                through: 0,
                complete: true,
                entries: vec![],
                now_ms: NOW,
            })
            .unwrap();
    }
    drop(journal);
    let mut journal = dir.open(RetentionPolicy::default());
    let mut inventory = sources;
    inventory.push(source("late-old-file"));
    journal.discover_imports(inventory).unwrap();
    journal
        .import_batch(batch("late-old-file", 1, true, vec![imported(1)]))
        .unwrap();
    assert_eq!(journal.range().unwrap().high_water.0, 1);
    assert_eq!(logs(&journal).len(), 0);
    let metadata: Value =
        serde_json::from_slice(&fs::read(dir.0.join("metadata.json")).unwrap()).unwrap();
    assert_eq!(
        metadata["recent_boots"],
        json!([
            "134393633885000000",
            "134393633895000000",
            "134393633905000000"
        ])
    );
    assert_eq!(
        journal
            .import_receipt(&ImportSource {
                id: "boot-3".into(),
                boot_filetime: BOOT + 30_000_000
            })
            .unwrap()
            .complete,
        true
    );
}

#[test]
fn receipt_capacity_preserves_pending_sources_and_reuses_retired_slots() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal
        .discover_imports((0..128).map(|i| source(&format!("file-{i}"))).collect())
        .unwrap();
    assert!(journal.discover_imports(vec![source("next")]).is_err());
    assert!(journal.forget_import(&source("file-0")).is_err());
    journal
        .import_batch(batch("file-0", 4, true, vec![]))
        .unwrap();
    journal.forget_import(&source("file-0")).unwrap();
    journal.discover_imports(vec![source("next")]).unwrap();
    drop(journal);
    let journal = dir.open(RetentionPolicy::default());
    assert_eq!(
        journal.import_receipt(&source("file-127")).unwrap().through,
        0
    );
    assert_eq!(
        journal.import_receipt(&source("next")).unwrap().complete,
        false
    );
    assert!(journal.import_receipt(&source("file-0")).is_err());
}

#[test]
fn failed_import_reopens_previous_receipt_and_retries_in_full() {
    let dir = Directory::new();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source("sealed")]).unwrap();
    fs::create_dir(dir.0.join("metadata.pending")).unwrap();
    assert!(journal
        .import_batch(batch("sealed", 3, true, vec![imported(1), imported(2)]))
        .is_err());
    assert!(journal.import_receipt(&source("sealed")).is_err());
    drop(journal);
    fs::remove_dir(dir.0.join("metadata.pending")).unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    assert_eq!(
        journal.import_receipt(&source("sealed")).unwrap().through,
        0
    );
    assert_eq!(
        journal.import_receipt(&source("sealed")).unwrap().complete,
        false
    );
    journal
        .import_batch(batch("sealed", 3, true, vec![imported(1), imported(2)]))
        .unwrap();
    assert_eq!(logs(&journal).len(), 2);
}

#[test]
fn owner_confirmation_covers_records_and_metadata_before_source_removal() {
    use std::sync::{atomic::AtomicBool, Arc};
    let dir = Directory::new();
    let path = dir.0.join("sealed.etl");
    fs::write(&path, "capture").unwrap();
    let journal = dir.open(RetentionPolicy::default());
    let stop = Arc::new(AtomicBool::new(false));
    let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
    history.discover_imports(vec![source("sealed")]).unwrap();
    let receipt = history
        .import_batch(batch("sealed", 2, true, vec![imported(1)]))
        .unwrap();
    assert_eq!((receipt.through, receipt.complete), (2, true));
    let metadata: Value =
        serde_json::from_slice(&fs::read(dir.0.join("metadata.json")).unwrap()).unwrap();
    assert_eq!(metadata["imports"][0]["complete"], true);
    assert_eq!(metadata["imports"][0]["through"], 2);
    assert_eq!(history.range().unwrap().high_water.0, 1);
    fs::remove_file(&path).unwrap();
    history.forget_import(source("sealed")).unwrap();
    stop.store(true, Ordering::Release);
    owner.join().unwrap().unwrap();
    let journal = dir.open(RetentionPolicy::default());
    assert_eq!(logs(&journal).len(), 1);
    assert!(journal.import_receipt(&source("sealed")).is_err());
}
