use crate::engine_events::Decoder;

const BOOT: u64 = 134_393_633_875_000_000;
const TIME: u64 = BOOT + 130 * 10_000_000;
const NOW: u64 = (TIME - 116_444_736_000_000_000) / 10_000;

fn fragment(index: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [TIME, BOOT + 1, 123, 456] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(index.to_le_bytes());
    bytes.extend(2u32.to_le_bytes());
    bytes.extend(b"warn\0default\0");
    let (length, byte) = if index == 0 {
        (61_440, b'a')
    } else {
        (26_560, b'b')
    };
    bytes.extend(vec![byte; length]);
    bytes
}

fn source(id: &str) -> ImportSource {
    ImportSource {
        id: id.into(),
        boot_filetime: BOOT,
    }
}

fn import_fragment(journal: &mut Journal, source: &ImportSource, index: u32) -> Decoder {
    let mut decoder = Decoder::restore(BOOT, &journal.graphics_fragments(BOOT).unwrap()).unwrap();
    let decoded = decoder
        .decode(&fragment(index), BOOT, 7, 1, 4, 42, 43)
        .unwrap();
    let entries = decoded
        .into_iter()
        .map(|log| ImportEntry {
            ordinal: 1,
            source_filetime: TIME,
            event: HistoryEvent::Log {
                record: venera_debug_protocol::v1::LogRecord {
                    stream_id: "fixture".into(),
                    seq: 1,
                    observed_at: "2026-11-20T12:00:00Z".into(),
                    source_time: None,
                    source: "etw".into(),
                    component: log.component.into(),
                    raw_level: "warn".into(),
                    level: "warning".into(),
                    session_id: None,
                    process: None,
                    ancestors: Vec::new(),
                    tid: Some(43),
                    text: String::from_utf8(log.message.clone().into_owned()).unwrap(),
                    structured: Some(log.structured(BOOT, 42)),
                },
            },
        })
        .collect();
    journal
        .import_batch(ImportBatch {
            source: source.clone(),
            through: 1,
            complete: false,
            entries,
            now_ms: NOW,
        })
        .unwrap();
    decoder
}

#[test]
fn graphics_rotation_reopens_at_receipt_publication() {
    for point in ["publish", "cleanup"] {
        let dir = Directory::new();
        let open = || dir.open(RetentionPolicy::default());
        let first = source("kernel-live-left");
        let next = source("kernel-live-right");
        let mut journal = open();
        journal
            .discover_imports(vec![first.clone(), next.clone()])
            .unwrap();
        let decoder = import_fragment(&mut journal, &first, 0);
        INTERRUPT.set(Some("publish"));
        assert!(journal
            .complete_graphics_import(&first, None, BOOT, NOW, Some(decoder.checkpoint(BOOT)))
            .is_err());
        drop(journal);
        let mut journal = open();
        assert!(!journal.import_receipt(&first).unwrap().complete);
        assert!(journal.graphics_fragments(BOOT).unwrap().is_empty());
        let decoder = import_fragment(&mut journal, &first, 0);
        journal
            .complete_graphics_import(&first, None, BOOT, NOW, Some(decoder.checkpoint(BOOT)))
            .unwrap();
        journal.forget_import(&first).unwrap();
        journal.close().unwrap();

        let mut journal = open();
        assert_eq!(journal.graphics_boots().unwrap(), vec![BOOT]);
        assert!(journal.graphics_fragments(BOOT + 1).unwrap().is_empty());
        assert!(read(&journal, 0, None, 10, 256 * 1024)
            .unwrap()
            .records
            .is_empty());
        import_fragment(&mut journal, &next, 1);
        journal.close().unwrap();

        let mut journal = open();
        let decoder = import_fragment(&mut journal, &next, 1);
        INTERRUPT.set(Some(point));
        assert!(journal
            .complete_graphics_import(&next, None, BOOT, NOW, Some(decoder.checkpoint(BOOT)))
            .is_err());
        drop(journal);

        let mut journal = open();
        assert_eq!(
            journal.import_receipt(&next).unwrap().complete,
            point == "cleanup"
        );
        if point == "publish" {
            let decoder = import_fragment(&mut journal, &next, 1);
            journal
                .complete_graphics_import(&next, None, BOOT, NOW, Some(decoder.checkpoint(BOOT)))
                .unwrap();
        }
        journal.finish_graphics_boot(BOOT, NOW).unwrap();
        let page = read(&journal, 0, None, 10, 256 * 1024).unwrap();
        assert_eq!(page.records.len(), 1);
        let HistoryEvent::Log { record } = &page.records[0].event else {
            panic!("expected graphics text")
        };
        assert_eq!(record.text.len(), 88_000);
        assert!(record.text == "a".repeat(61_440) + &"b".repeat(26_560));
        assert!(journal.graphics_boots().unwrap().is_empty());
        assert!(!fs::read_dir(&dir.0).unwrap().any(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "frag")));
    }
}

#[test]
fn graphics_end_publishes_one_incomplete_message_diagnostic() {
    let dir = Directory::new();
    let source = source("kernel-live-terminal");
    let mut journal = dir.open(RetentionPolicy::default());
    journal.discover_imports(vec![source.clone()]).unwrap();
    let decoder = import_fragment(&mut journal, &source, 0);
    journal
        .complete_graphics_import(&source, None, BOOT, NOW, Some(decoder.checkpoint(BOOT)))
        .unwrap();
    journal.finish_graphics_boot(BOOT, NOW).unwrap();
    journal.close().unwrap();
    let mut journal = dir.open(RetentionPolicy::default());
    journal.finish_graphics_boot(BOOT, NOW).unwrap();
    let page = read(&journal, 0, None, 10, 4096).unwrap();
    assert_eq!(page.records.len(), 1);
    let HistoryEvent::Gap { gap } = &page.records[0].event else {
        panic!("expected a gap")
    };
    assert_eq!(
        (&*gap.reason, gap.count),
        ("graphics_fragments_incomplete", Some(DecimalU64(1)))
    );
}

fn capacity_fixture(
    dir: &Directory,
) -> (
    u64,
    u64,
    Vec<HistoryRecord>,
    Vec<u8>,
    ImportSource,
    ImportSource,
) {
    let mut journal = dir.open(RetentionPolicy::default());
    let first = source("kernel-live-capacity-left");
    let next = source("kernel-live-capacity-right");
    journal
        .discover_imports(vec![first.clone(), next.clone()])
        .unwrap();
    let decoder = import_fragment(&mut journal, &first, 0);
    let old = decoder.checkpoint(BOOT).bytes;
    journal
        .complete_graphics_import(&first, None, BOOT, NOW, Some(decoder.checkpoint(BOOT)))
        .unwrap();
    let records = journal
        .append_batch(
            (0..50)
                .map(|n| {
                    input(
                        &BOOT.to_string(),
                        NOW + n,
                        false,
                        &format!("row {n:02}: {}", "x".repeat(8192)),
                    )
                })
                .collect(),
        )
        .unwrap();
    let disk = journal.disk_bytes;
    let prepare = journal.prepare_limit();
    journal.close().unwrap();
    (disk, prepare, records, old, first, next)
}

fn replacement_fragment() -> crate::engine_events::GraphicsUpdate {
    let mut decoder = Decoder::default();
    let mut bytes = fragment(0);
    bytes[24..32].copy_from_slice(&457u64.to_le_bytes());
    decoder.decode(&bytes, BOOT, 7, 1, 4, 42, 43).unwrap();
    decoder.checkpoint(BOOT)
}

#[test]
fn graphics_stage_create_failure_preserves_fifty_published_rows() {
    let dir = Directory::new();
    let (disk, prepare, records, old, first, next) = capacity_fixture(&dir);
    let update = replacement_fragment();
    let charge = space::rounded(space::unit(&dir.0).unwrap(), update.bytes.len() as u64);
    let policy = RetentionPolicy {
        max_bytes: disk + charge + prepare - 1,
        ..Default::default()
    };
    let mut journal = dir.open(policy.clone());
    let old_range = journal.range().unwrap();
    let receipts = [
        journal.import_receipt(&first).unwrap(),
        journal.import_receipt(&next).unwrap(),
    ];
    let collision = dir.0.join(format!("graphics-{BOOT}-1.frag"));
    File::create(&collision).unwrap();
    let error = journal
        .complete_graphics_import(&next, None, BOOT, NOW, Some(update))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    drop(journal);
    fs::remove_file(collision).unwrap();

    let journal = dir.open(policy);
    let page = read(&journal, 0, None, 100, 900 * 1024).unwrap();
    eprintln!(
        "reopened rows={}, oldest={}, high_water={}",
        page.records.len(),
        page.range.oldest.0,
        page.range.high_water.0
    );
    assert_eq!(journal.graphics_fragments(BOOT).unwrap(), old);
    assert_eq!(
        [
            journal.import_receipt(&first).unwrap(),
            journal.import_receipt(&next).unwrap()
        ],
        receipts
    );
    assert_eq!(page.records.len(), 50);
    assert_eq!(page.records, records);
    assert_eq!(page.range, old_range);
    assert_eq!(page.next, records.last().unwrap().cursor);
}

fn physical_bytes(dir: &Directory, unit: u64) -> u64 {
    fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| space::charge(unit, &File::open(entry.unwrap().path()).unwrap()).unwrap())
        .sum()
}

#[test]
fn graphics_capacity_publishes_evictions_with_receipt_and_diagnostic() {
    let dir = Directory::new();
    let (disk, _, records, old, first, next) = capacity_fixture(&dir);
    let unit = space::unit(&dir.0).unwrap();
    let charge = space::rounded(unit, replacement_fragment().bytes.len() as u64);
    let policy = RetentionPolicy {
        max_bytes: disk + charge + 3 * unit,
        ..Default::default()
    };
    let open = || dir.open(policy.clone());
    let mut journal = open();
    let complete = |j: &mut Journal| {
        j.complete_graphics_import(
            &next,
            Some(("fixture_capture_loss".into(), Some(1))),
            BOOT,
            NOW,
            Some(replacement_fragment()),
        )
    };
    INTERRUPT.set(Some("publish"));
    assert_eq!(complete(&mut journal).unwrap_err().to_string(), "publish");
    let peak = physical_bytes(&dir, unit);
    assert!(peak > disk + charge);
    assert!(peak <= policy.max_bytes);
    drop(journal);

    let mut journal = open();
    assert_eq!(
        read(&journal, 0, None, 100, 900 * 1024).unwrap().records,
        records
    );
    assert_eq!(journal.graphics_fragments(BOOT).unwrap(), old);
    assert!(!journal.import_receipt(&next).unwrap().complete);
    assert!(complete(&mut journal).unwrap().complete);
    journal.close().unwrap();

    let journal = open();
    let page = read(&journal, 0, None, 100, 900 * 1024).unwrap();
    let oldest = page.range.oldest.0;
    assert!((2..=50).contains(&oldest));
    assert_eq!(
        &page.records[..page.records.len() - 1],
        &records[(oldest - 1) as usize..]
    );
    let last = page.records.last().unwrap();
    assert_eq!(last.cursor.position.0, 51);
    let HistoryEvent::Gap { gap } = &last.event else {
        panic!("expected a capture diagnostic")
    };
    assert_eq!(
        gap.reason,
        "fixture_capture_loss; source=kernel-live-capacity-right"
    );
    assert_eq!(gap.count, Some(DecimalU64(1)));
    assert!(journal.import_receipt(&first).unwrap().complete);
    assert!(journal.import_receipt(&next).unwrap().complete);
    assert_eq!(
        journal.graphics_fragments(BOOT).unwrap(),
        replacement_fragment().bytes
    );
    assert!(physical_bytes(&dir, unit) < disk);
    eprintln!(
        "published oldest={oldest}, high_water=51, peak={peak}, budget={}",
        policy.max_bytes
    );
}

#[test]
fn graphics_capacity_defers_with_published_generation_intact() {
    let dir = Directory::new();
    let (disk, _, records, old, first, next) = capacity_fixture(&dir);
    let unit = space::unit(&dir.0).unwrap();
    let charge = space::rounded(unit, replacement_fragment().bytes.len() as u64);
    let policy = RetentionPolicy {
        max_bytes: disk + charge,
        ..Default::default()
    };
    let mut journal = dir.open(policy.clone());
    let receipts = [
        journal.import_receipt(&first).unwrap(),
        journal.import_receipt(&next).unwrap(),
    ];
    let error = journal
        .complete_graphics_import(&next, None, BOOT, NOW, Some(replacement_fragment()))
        .unwrap_err();
    assert_eq!(error.to_string(), "journal staging capacity exhausted");
    assert_eq!(physical_bytes(&dir, unit), disk);
    assert_eq!(journal.range().unwrap().high_water.0, 50);
    journal.close().unwrap();

    let journal = dir.open(policy);
    let page = read(&journal, 0, None, 100, 900 * 1024).unwrap();
    assert_eq!(page.records, records);
    assert_eq!((page.range.oldest.0, page.range.high_water.0), (1, 50));
    assert_eq!(journal.graphics_fragments(BOOT).unwrap(), old);
    assert_eq!(
        [
            journal.import_receipt(&first).unwrap(),
            journal.import_receipt(&next).unwrap()
        ],
        receipts
    );
    eprintln!("deferred with {disk} allocated bytes and {charge} bytes of sidecar headroom");
}
