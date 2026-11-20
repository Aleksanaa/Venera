use crate::{
    ancestry::{Attribution, Lifecycle},
    capture_storage,
    engine_events::Decoder,
};
use std::{cell::RefCell, io::BufWriter};
use venera_debug_protocol::{LogRecord, ProcessIdentity};

#[derive(Debug)]
struct Sample {
    boundary: String,
    eof: u64,
    allocation: u64,
    capture_count: usize,
    capture_allocation: u64,
    process_allocation: u64,
    graphics_allocation: u64,
}

thread_local! {
    static SAMPLES: RefCell<Option<Vec<Sample>>> = const { RefCell::new(None) };
}

#[cfg(unix)]
fn allocated(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    path.metadata().unwrap().blocks() * 512
}

#[cfg(windows)]
fn allocated(path: &Path) -> u64 {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandleEx, FILE_STANDARD_INFO,
    };
    let file = File::open(path).unwrap();
    let mut info = FILE_STANDARD_INFO::default();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&mut info as *mut FILE_STANDARD_INFO).cast(),
            size_of_val(&info) as u32,
        )
    };
    assert_ne!(ok, 0, "{}", io::Error::last_os_error());
    info.AllocationSize.try_into().unwrap()
}

pub(in crate::journal) fn observe(pending: &Path, original: &Path) {
    let name = pending.file_name().unwrap().to_str().unwrap();
    sample(pending.parent().unwrap(), name, Some((pending, original)));
}

fn sample(journal: &Path, boundary: &str, slots: Option<(&Path, &Path)>) {
    SAMPLES.with_borrow_mut(|samples| {
        let Some(samples) = samples else { return };
        let capture = journal.parent().unwrap().join("capture");
        let mut result = Sample {
            boundary: boundary.into(),
            eof: 0,
            allocation: 0,
            capture_count: 0,
            capture_allocation: 0,
            process_allocation: 0,
            graphics_allocation: 0,
        };
        let mut inventory = std::env::var_os("VENERA_SHARED_BUDGET_INVENTORY").map(|path| {
            BufWriter::new(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap(),
            )
        });
        for dir in [&capture, journal] {
            let mut files: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    assert!(entry.file_type().unwrap().is_file());
                    (
                        entry.file_name().into_string().unwrap(),
                        entry.metadata().unwrap().len(),
                        allocated(&entry.path()),
                    )
                })
                .collect();
            files.sort_unstable();
            let eof = files.iter().map(|file| file.1).sum::<u64>();
            let allocation = files.iter().map(|file| file.2).sum::<u64>();
            if dir == capture {
                capture_storage::validate(
                    &files
                        .iter()
                        .map(|(name, len, _)| (name.clone(), *len))
                        .collect::<Vec<_>>(),
                )
                .unwrap();
                result.capture_count = files.len();
                result.capture_allocation = allocation;
            } else {
                result.process_allocation = files
                    .iter()
                    .filter(|file| file.0.ends_with(".proc") && file.1 != 0)
                    .map(|file| file.2)
                    .sum();
                result.graphics_allocation = files
                    .iter()
                    .filter(|file| file.0.ends_with(".frag") && file.1 != 0)
                    .map(|file| file.2)
                    .sum();
            }
            let directory = dir.file_name().unwrap().to_str().unwrap();
            if let Some(writer) = &mut inventory {
                for (name, len, bytes) in &files {
                    writeln!(
                        writer,
                        "{}\t{boundary}\t{directory}\t{name}\t{len}\t{bytes}",
                        samples.len()
                    )
                    .unwrap();
                }
            }
            println!(
                "sample {} {directory} files={} EOF={eof} allocated={allocation}",
                samples.len(),
                files.len()
            );
            result.eof += eof;
            result.allocation += allocation;
        }
        if let Some(writer) = &mut inventory {
            writer.flush().unwrap();
        }
        if let Some((pending, original)) = slots {
            let pending_len = pending.metadata().unwrap().len();
            let original_len = original.metadata().unwrap().len();
            println!(
                "slots {boundary}={} original {}={}",
                pending_len,
                original.file_name().unwrap().to_string_lossy(),
                original_len
            );
            assert!(pending_len > 0 && original_len > 0);
        }
        println!("sample {} {result:?}", samples.len());
        samples.push(result);
    });
}

#[test]
#[ignore = "requires the preserved DebugHistory directory and capacity.etl"]
fn early_history_migration_extent_peak() {
    const ID: &str = "2026-11-17T05:18:23.677Z-11680";
    const TARGET: u64 = 111_627;
    const HIGH_WATER: u64 = 124_175;
    let env_path = |key| PathBuf::from(std::env::var_os(key).expect(key));
    let fixture = env_path("VENERA_MIGRATION_FIXTURE")
        .canonicalize()
        .expect("locate DebugHistory fixture");
    let inventory = env_path("VENERA_SHARED_BUDGET_INVENTORY");
    let evidence = inventory
        .parent()
        .unwrap()
        .canonicalize()
        .expect("locate evidence directory");
    let dir = Directory::new();
    let scratch = dir.0.canonicalize().unwrap();
    assert!(!evidence.starts_with(&scratch) && !evidence.starts_with(&fixture));
    File::create_new(&inventory).expect("create a fresh inventory outside scratch and fixture");
    println!(
        "measurement={} evidence={}",
        scratch.display(),
        evidence.display()
    );

    let root = dir.0.join("DebugHistory");
    fs::create_dir(&root).unwrap();
    for entry in fs::read_dir(&fixture).expect("enumerate DebugHistory fixture") {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file(), "{:?}", entry.path());
        fs::copy(entry.path(), root.join(entry.file_name())).unwrap_or_else(|error| {
            panic!("copy {}: {error}", entry.path().display());
        });
    }
    let capture = dir.0.join("capture");
    fs::create_dir(&capture).unwrap();
    let etl = capture.join("kernel-boot.etl.1");
    assert_eq!(
        fs::copy(env_path("VENERA_CAPACITY_ETL"), &etl).expect("copy capacity.etl"),
        32 << 20
    );
    OpenOptions::new()
        .write(true)
        .open(&etl)
        .unwrap()
        .sync_all()
        .expect("sync capacity.etl copy");

    let manifest = || {
        serde_json::from_reader::<_, Value>(File::open(root.join("metadata.json")).unwrap())
            .unwrap()
    };
    let legacy = manifest();
    assert_eq!(legacy["journal_id"], ID);
    assert_eq!(legacy["high_water"], HIGH_WATER);
    assert!(legacy.get("process_segments").is_none());
    assert!(legacy["processes"]
        .as_object()
        .is_some_and(|boots| !boots.is_empty()));
    SAMPLES.set(Some(Vec::new()));
    sample(&root, "before-migration", None);
    let open = || {
        Journal::open(
            &root,
            "migration-qa",
            RetentionPolicy {
                max_bytes: capture_storage::JOURNAL_BYTES,
                ..Default::default()
            },
        )
        .expect("open copied DebugHistory through production migration")
    };
    let check = |j: &Journal| {
        let range = j.range().unwrap();
        let page = j
            .read(
                HistoryRead {
                    after: Some(HistoryCursor {
                        journal_id: ID.into(),
                        position: DecimalU64(TARGET - 1),
                    }),
                    through: Some(DecimalU64(TARGET)),
                    limit: 1,
                },
                256 << 10,
            )
            .unwrap();
        assert_eq!(range.journal_id, ID);
        assert_eq!(range.high_water.0, HIGH_WATER);
        assert_eq!(page.next.position.0, TARGET);
        assert!(page.missing.is_empty());
        let [row] = page.records.as_slice() else {
            panic!("expected exactly one retained row at {TARGET}")
        };
        assert_eq!(row.cursor, page.next);
        assert_eq!(row.boot_id, "134394002545000000");
        let HistoryEvent::Log { record } = &row.event else {
            panic!("expected kernel initialization log")
        };
        assert_eq!(record.text, "Venera kernel initialization");
        println!(
            "read position={} high_water={} text={}",
            row.cursor.position.0, range.high_water.0, record.text
        );
    };
    let j = open();
    sample(&root, "after-publish", None);
    assert!(!root.join("metadata.pending").exists());
    let published = manifest();
    assert!(published.get("processes").is_none());
    assert!(published["process_segments"]
        .as_object()
        .is_some_and(|boots| !boots.is_empty()));
    check(&j);
    j.close().unwrap();
    let j = open();
    sample(&root, "after-reopen", None);
    check(&j);
    j.close().unwrap();

    let samples = SAMPLES.take().unwrap();
    assert_eq!(samples[0].boundary, "before-migration");
    assert_eq!(samples[0].process_allocation, 0);
    assert_eq!(samples[1].boundary, "metadata.pending");
    assert!(samples[1].process_allocation > 0);
    assert!(samples
        .iter()
        .all(|s| s.capture_count == 1 && s.capture_allocation == samples[0].capture_allocation));
    let peak = samples.iter().max_by_key(|s| s.allocation).unwrap();
    println!(
        "migration peak={} joint={} journal={} capture={}",
        peak.boundary,
        peak.allocation,
        peak.allocation - peak.capture_allocation,
        peak.capture_allocation
    );
}

const BOOT: u64 = 134_393_633_875_000_000;
const TIME: u64 = BOOT + 130 * 10_000_000;
const NOW: u64 = (TIME - 116_444_736_000_000_000) / 10_000;
const PID: u32 = 42;

fn decode_fragment(
    decoder: &mut Decoder,
    index: u32,
) -> Option<crate::user_events::UserEvent<'static>> {
    let mut bytes = Vec::new();
    for word in [TIME, BOOT + 1, 123, 456] {
        bytes.extend(word.to_le_bytes());
    }
    bytes.extend(index.to_le_bytes());
    bytes.extend(2u32.to_le_bytes());
    bytes.extend(b"warn\0default\0");
    bytes.extend(if index == 0 {
        vec![b'a'; 60 * 1024]
    } else {
        b"graphics tail".to_vec()
    });
    decoder.decode(&bytes, BOOT, 7, 1, 4, PID, 43).unwrap()
}

fn receipt(source: &ImportSource, through: u64, complete: bool) -> ImportReceipt {
    ImportReceipt {
        source: source.clone(),
        through,
        complete,
    }
}

#[test]
#[ignore = "shared storage pressure; requires the r3 capacity.etl and 1.2 GiB temporary space"]
fn capture_and_journal_replacement_peak() {
    let dir = Directory::new();
    let capture = dir.0.join("capture");
    fs::create_dir(&capture).unwrap();
    let etl = fs::read(std::env::var_os("VENERA_CAPACITY_ETL").unwrap()).unwrap();
    assert_eq!(etl.len(), 32 << 20);
    for prefix in ["kernel", "legacy"] {
        for name in [
            "boot.etl.1",
            "boot.etl.2",
            "boot.etl.3",
            "live-1.etl",
            "live-2.etl",
        ] {
            let mut file = File::create(capture.join(format!("{prefix}-{name}"))).unwrap();
            file.write_all(&etl).unwrap();
            file.sync_all().unwrap();
        }
    }
    let policy = RetentionPolicy {
        max_bytes: capture_storage::JOURNAL_BYTES,
        ..Default::default()
    };
    let expiry = NOW + policy.runtime_ms;
    let root = dir.0.join("journal");
    let open = || Journal::open(&root, "journal-a", policy.clone()).unwrap();
    let mut j = open();
    let payload = "x".repeat(128 << 10);
    let load = |j: &mut Journal| {
        j.append_batch((0..7).map(|_| input("a", NOW, true, &payload)).collect())
            .unwrap();
    };
    for _ in 0..799 {
        load(&mut j);
    }

    // These ETL copies occupy the capture slots; the journal inputs below model
    // the decoded output of two sealed files across a rotation.
    let first = ImportSource {
        id: "kernel-live-1.etl".into(),
        boot_filetime: BOOT,
    };
    let next = ImportSource {
        id: "kernel-live-2.etl".into(),
        boot_filetime: BOOT,
    };
    let identity = ProcessIdentity {
        pid: PID,
        start_time_filetime: Some((BOOT + 1).to_string()),
        boot_time_filetime: Some(BOOT.to_string()),
        start_key: Some("314".into()),
        exe_path: Some(r"C:\Games\fixture.exe".into()),
    };
    let import = |j: &mut Journal, source: &ImportSource, through, entries| {
        j.import_batch(ImportBatch {
            source: source.clone(),
            through,
            complete: false,
            entries,
            now_ms: NOW,
        })
        .unwrap()
    };
    SAMPLES.set(Some(Vec::new()));
    j.discover_imports(vec![first.clone(), next.clone()])
        .unwrap();
    j.checkpoint_processes(
        &next,
        vec![Lifecycle::Start {
            identity: identity.clone(),
            parent: 0,
            parent_key: None,
            session: Some(1),
        }],
        TIME,
        NOW,
    )
    .unwrap();
    let mut decoder = Decoder::default();
    assert!(decode_fragment(&mut decoder, 0).is_none());
    let fragments = decoder.checkpoint(BOOT);
    assert_eq!(import(&mut j, &first, 1, vec![]), receipt(&first, 1, false));
    assert_eq!(
        j.complete_graphics_import(&first, None, BOOT, NOW, Some(decoder.checkpoint(BOOT)))
            .unwrap(),
        receipt(&first, 1, true)
    );

    sample(&root, "before-capacity-eviction", None);
    let oldest = j.range().unwrap().oldest.0;
    load(&mut j);
    assert!(
        j.range().unwrap().oldest.0 > oldest,
        "saturated append must reclaim legacy history"
    );
    sample(&root, "after-capacity-eviction", None);

    let marker = || ImportEntry {
        ordinal: 1,
        source_filetime: BOOT + 10_000,
        event: event("original capture row"),
    };
    let after = j.range().unwrap().high_water.0;
    assert_eq!(
        import(&mut j, &next, 1, vec![marker()]),
        receipt(&next, 1, false)
    );
    let original = read(&j, after, None, 1, 4096).unwrap().records.remove(0);
    assert_eq!(original.cursor.position.0, after + 1);
    assert_eq!(original.event, marker().event);
    let kept = j
        .append_batch(vec![
            input("a", NOW - 1, false, "expires"),
            input("a", NOW + 1, false, "survives"),
        ])
        .unwrap()
        .pop()
        .unwrap();
    j.prune(expiry).unwrap();
    j.close().unwrap();

    let mut j = open();
    sample(&root, "reopened", None);
    assert_eq!(fs::read(capture.join(&next.id)).unwrap(), etl);
    assert_eq!(j.import_receipt(&first).unwrap(), receipt(&first, 1, true));
    assert_eq!(j.import_receipt(&next).unwrap(), receipt(&next, 1, false));
    assert_eq!(j.graphics_fragments(BOOT).unwrap(), fragments.bytes);
    assert_eq!(
        read(&j, after, None, 10, 4096).unwrap().records,
        [original.clone(), kept.clone()]
    );

    let mut decoder = Decoder::restore(BOOT, &j.graphics_fragments(BOOT).unwrap()).unwrap();
    let decoded = decode_fragment(&mut decoder, 1).expect("reassemble across capture files");
    let mut record = LogRecord {
        stream_id: "fixture".into(),
        seq: 1,
        observed_at: "2026-11-20T12:00:00Z".into(),
        source_time: None,
        source: "etw".into(),
        component: decoded.component.into(),
        raw_level: "warn".into(),
        level: "warning".into(),
        session_id: None,
        process: Some(ProcessIdentity {
            pid: PID,
            ..Default::default()
        }),
        ancestors: Vec::new(),
        tid: Some(43),
        text: String::from_utf8(decoded.message.to_vec()).unwrap(),
        structured: Some(decoded.structured(BOOT, PID)),
    };
    assert_eq!(
        j.attribute_process_record(&mut record, BOOT, TIME, true)
            .unwrap(),
        Attribution::Ready
    );
    let entries = || {
        vec![
            marker(),
            ImportEntry {
                ordinal: 2,
                source_filetime: TIME,
                event: HistoryEvent::Log {
                    record: record.clone(),
                },
            },
        ]
    };
    assert_eq!(
        import(&mut j, &next, 2, entries()),
        receipt(&next, 2, false)
    );
    sample(&root, "resumed", None);
    assert_eq!(
        j.complete_graphics_import(&next, None, BOOT, NOW, Some(decoder.checkpoint(BOOT)))
            .unwrap(),
        receipt(&next, 2, true)
    );
    sample(&root, "graphics-reclaimed", None);
    j.close().unwrap();

    let mut j = open();
    assert_eq!(j.import_receipt(&first).unwrap(), receipt(&first, 1, true));
    assert_eq!(j.import_receipt(&next).unwrap(), receipt(&next, 2, true));
    assert_eq!(import(&mut j, &next, 2, entries()), receipt(&next, 2, true));
    assert!(j.graphics_fragments(BOOT).unwrap().is_empty());
    let page = read(&j, after, None, 10, 128 << 10).unwrap();
    assert_eq!(page.records.len(), 3);
    assert_eq!(&page.records[..2], &[original, kept.clone()]);
    let resumed = &page.records[2];
    assert_eq!(resumed.cursor.position.0, kept.cursor.position.0 + 1);
    assert_eq!(page.range.high_water, resumed.cursor.position);
    let HistoryEvent::Log { record } = &resumed.event else {
        panic!("expected graphics log")
    };
    assert_eq!(record.text, "a".repeat(60 * 1024) + "graphics tail");
    assert_eq!(record.process, Some(identity));
    println!(
        "resume source={} through=2 original={} compacted={} graphics={} pid={PID} body_bytes={}",
        next.id,
        after + 1,
        kept.cursor.position.0,
        resumed.cursor.position.0,
        record.text.len()
    );

    // Expire runtime rows and move the recent-boot window past this process segment.
    j.prune(expiry + 2).unwrap();
    j.discover_imports(
        (1..=policy.startup_boots)
            .map(|n| ImportSource {
                id: format!("new-boot-{n}"),
                boot_filetime: BOOT + n as u64 * 10_000,
            })
            .collect(),
    )
    .unwrap();
    sample(&root, "process-reclaimed", None);
    j.close().unwrap();

    let samples = SAMPLES.take().unwrap();
    let both = |s: &Sample| s.process_allocation > 0 && s.graphics_allocation > 0;
    for boundary in [
        "metadata.pending",
        ".jsonl",
        ".compact",
        "reopened",
        "resumed",
    ] {
        assert!(
            samples
                .iter()
                .any(|s| s.boundary.ends_with(boundary) && both(s)),
            "sidecars absent at {boundary}"
        );
    }
    let at = |name| samples.iter().find(|s| s.boundary == name).unwrap();
    assert!(at("resumed").allocation > at("graphics-reclaimed").allocation);
    assert_eq!(at("graphics-reclaimed").graphics_allocation, 0);
    assert!(at("graphics-reclaimed").process_allocation > 0);
    assert_eq!(at("process-reclaimed").process_allocation, 0);
    let peak = samples.iter().max_by_key(|s| s.allocation).unwrap();
    assert!(
        peak.allocation >= capture_storage::TOTAL_BYTES - (16 << 20),
        "insufficient pressure: {peak:?}"
    );
    for s in samples {
        assert_eq!(s.capture_count, 10, "{s:?}");
        assert!(
            s.capture_allocation <= capture_storage::CAPTURE_RESERVATION,
            "{s:?}"
        );
        assert!(
            s.allocation - s.capture_allocation <= policy.max_bytes,
            "{s:?}"
        );
        assert!(
            s.eof <= capture_storage::TOTAL_BYTES && s.allocation <= capture_storage::TOTAL_BYTES,
            "{s:?}"
        );
    }
}

#[test]
fn allocated_peak_includes_capture_and_both_compaction_slots() {
    let dir = Directory::new();
    let unit = space::unit(&dir.0).unwrap();
    let capture = dir.0.join("capture");
    fs::create_dir(&capture).unwrap();
    fs::write(capture.join("kernel-boot.etl.1"), vec![1; unit as usize]).unwrap();
    let root = dir.0.join("journal");
    let mut j = Journal::open(
        &root,
        "journal-a",
        RetentionPolicy {
            runtime_ms: 10,
            max_bytes: 5 * unit,
            ..Default::default()
        },
    )
    .unwrap();
    SAMPLES.set(Some(Vec::new()));
    j.append_batch(vec![
        input("a", 0, false, "expired"),
        input("a", 9, false, "kept"),
    ])
    .unwrap();
    j.prune(12).unwrap();
    let samples = SAMPLES.take().unwrap();
    let peak = samples.iter().map(|s| s.allocation).max().unwrap();
    assert!(peak <= 6 * unit, "{samples:?}");
    assert_eq!(
        read(&j, 0, None, 10, 8192).unwrap().records[0].event,
        event("kept")
    );
    assert_eq!(
        samples
            .iter()
            .filter(|s| s.boundary == "metadata.pending")
            .count(),
        2
    );
}
