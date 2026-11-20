use crate::capture_storage;
use std::cell::RefCell;

thread_local! {
    static SAMPLES: RefCell<Option<Vec<(String, u64, u64)>>> = const { RefCell::new(None) };
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
    SAMPLES.with_borrow_mut(|samples| {
        let Some(samples) = samples else { return };
        let journal = pending.parent().unwrap();
        let capture = journal.parent().unwrap().join("capture");
        let mut totals = [0u64; 2];
        for dir in [&capture, journal] {
            let mut sizes = Vec::new();
            let mut sum = [0u64; 2];
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let len = entry.metadata().unwrap().len();
                sum[0] += len;
                sum[1] += allocated(&entry.path());
                sizes.push((entry.file_name().into_string().unwrap(), len));
            }
            if dir == capture {
                capture_storage::validate(&sizes).unwrap();
            }
            println!(
                "{} files={} EOF={} allocated={}",
                dir.display(),
                sizes.len(),
                sum[0],
                sum[1]
            );
            totals[0] += sum[0];
            totals[1] += sum[1];
        }
        let name = pending.file_name().unwrap().to_str().unwrap();
        println!(
            "boundary {name}={} original {}={} joint EOF={} allocated={}",
            pending.metadata().unwrap().len(),
            original.file_name().unwrap().to_string_lossy(),
            original.metadata().unwrap().len(),
            totals[0],
            totals[1]
        );
        samples.push((name.to_owned(), totals[0], totals[1]));
    });
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
    let expiry = policy.runtime_ms + 1;
    let root = dir.0.join("journal");
    let mut j = Journal::open(&root, "journal-a", policy).unwrap();
    let payload = "x".repeat(128 << 10);
    for _ in 0..800 {
        j.append_batch((0..7).map(|_| input("a", 1, true, &payload)).collect())
            .unwrap();
    }
    SAMPLES.set(Some(Vec::new()));
    j.append_batch(vec![
        input("a", 0, false, "expires"),
        input("a", 2, false, "survives"),
    ])
    .unwrap();
    j.prune(expiry).unwrap();
    j.close().unwrap();
    let samples = SAMPLES.take().unwrap();
    assert_eq!(
        samples.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        ["metadata.pending", "segment.pending"]
    );
    assert!(
        samples
            .iter()
            .all(|s| s.1 <= capture_storage::TOTAL_BYTES && s.2 <= capture_storage::TOTAL_BYTES),
        "{samples:?}"
    );
}
