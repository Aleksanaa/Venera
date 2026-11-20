// SPDX-License-Identifier: MIT
use super::*;
use crate::windows::ipc::tests::{native_graphics_live_pair as live, native_graphics_pipe as pipe};
use serde_json::json;
use std::process::{Command, ExitStatus, Stdio};
use venera_debug_protocol::v1::{EventValue, HistoryEvent, HistoryPage, LogRecord};

fn run(kit: &Path, dir: &Path, name: &str, args: &[&str]) -> (u32, ExitStatus) {
    let mut child = Command::new(kit.join(name))
        .current_dir(dir)
        .args(args)
        .envs([
            ("DXVK_LOG_LEVEL", "warn"), ("DXVK_LOG_PATH", "."),
            ("VKD3D_DEBUG", "err"), ("VKD3D_SHADER_DEBUG", "warn"),
            ("MESA_LOG", "windbg"), ("MESA_LOG_LEVEL", "debug"),
            ("VENERA_VERBOSE", "0"),
        ])
        .env("VENERA_LOG", format!("{name}-umd.log"))
        .env("VKD3D_LOG_FILE", format!("{name}-shader.log"))
        .env_remove("VKD3D_LOG_BUFFERED")
        .stdout(Stdio::from(fs::File::create(dir.join(format!("{name}-{}.images.txt", args[0]))).unwrap()))
        .stderr(Stdio::from(fs::File::create(dir.join(format!("{name}-{}.stderr.txt", args[0]))).unwrap()))
        .spawn().unwrap();
    let pid = child.id();
    let exit = child.wait().unwrap();
    println!("{name} {} pid={pid} status={exit}", args[0]);
    (pid, exit)
}

fn entries(page: &HistoryPage, pid: u32) -> impl Iterator<Item = &LogRecord> {
    page.records.iter().filter_map(move |entry| match &entry.event {
        HistoryEvent::Log { record } if record.process.as_ref().is_some_and(|p| p.pid == pid) => Some(record),
        _ => None,
    })
}

#[test]
#[ignore = "Uses the interactive Windows listener and a private ETW session"]
fn native_graphics_package_matrix() {
    let kit = fs::canonicalize(PathBuf::from(std::env::var_os("VENERA_GRAPHICS_MATRIX").unwrap())).unwrap();
    let kit = PathBuf::from(kit.to_str().unwrap().trim_start_matches(r"\\?\"));
    let payload = kit.join("payload");
    let payload = payload.to_str().unwrap();
    let tag = format!("{}-{}", std::process::id(), clock::filetime());
    let marker = format!("Venera graphics same text {tag}");
    let dir = std::env::temp_dir().join(format!("venera-graphics-matrix-{tag}"));
    fs::create_dir(&dir).unwrap();
    let trace = dir.join("graphics.etl");
    let session_name = format!("VeneraGraphicsMatrix-{tag}");
    println!("session={session_name} artifacts={}", dir.display());

    let session = FileSession::start(&session_name, &trace, 0, 32).unwrap();
    session.enable(&etw::KERNEL_PROVIDER, 5, 1).unwrap();
    let before = live::range();
    let (pair_pid, pair_status) = run(&kit, &dir, "native-graphics-matrix-x64.exe", &["pair", payload, &marker]);
    let (pid64, status64) = run(&kit, &dir, "native-graphics-matrix-x64.exe", &["matrix", payload]);
    let (pid32, status32) = run(&kit, &dir, "native-graphics-matrix-x86.exe", &["matrix", payload]);
    let loss = session.control_status();
    let stopped = session.stop();
    let (live_pass, live_report) = live::observe(before, pair_pid, &marker);
    fs::write(dir.join("live-pair.json"), serde_json::to_vec_pretty(&live_report).unwrap()).unwrap();
    println!("live history: {} ETW matches={} DBWIN matches={}", live_report["status"], live_report["etw_matches"], live_report["dbwin_matches"]);
    stopped.unwrap();
    let loss = loss.unwrap();

    let replay = dir.join("kernel-live-graphics.etl");
    fs::copy(&trace, &replay).unwrap();
    let source = files::header(&replay).unwrap();
    let state = Arc::new(Mutex::new(State::new(tag.clone(), true)));
    state.lock().unwrap().boot_id = source.boot_filetime.to_string();
    let open = || {
        let journal = crate::journal::Journal::open(dir.join("history"), &tag, Default::default()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (history, owner) = crate::history::History::start(journal, stop.clone()).unwrap();
        (history, owner, stop)
    };
    let (history, owner, stop) = open();
    history.discover_imports(vec![source.clone()]).unwrap();
    import_file(&state, &history, &replay, source, false, false).unwrap();
    crate::history::finish(Vec::new(), &stop, owner).unwrap();
    let (history, owner, stop) = open();
    state.lock().unwrap().history = Some(history);
    let result = pipe::read(state, json!({"after":null,"through":null,"limit":256}));
    crate::history::finish(Vec::new(), &stop, owner).unwrap();
    fs::write(dir.join("private-history-page.json"), serde_json::to_vec(&result).unwrap()).unwrap();
    let page: HistoryPage = serde_json::from_value(result).unwrap();
    assert_eq!((loss.2, loss.3, loss.4), (0, 0, 0));
    assert_eq!(page.range.high_water, page.next.position, "private history page ended early");
    assert!(page.missing.is_empty());
    assert!(page.records.iter().all(|r| !matches!(r.event, HistoryEvent::Gap { .. })));
    let pair: Vec<_> = entries(&page, pair_pid).filter(|r| r.text.contains(&marker)).collect();
    assert_eq!(pair.len(), 1, "private same-text records: {pair:?}");
    assert_eq!((pair[0].text.as_str(), pair[0].source.as_str(), pair[0].component.as_str(), pair[0].structured.as_ref().map(|e| e.event_id)),
        (marker.as_str(), "etw", "venera_d3d11", Some(5)));

    let shader = "mov r0, r1\n".repeat(8000);
    assert_eq!(shader.len(), 88_000);
    let vkd = format!("compile: {shader}");
    let expected = [
        (5, "venera_d3d11", "warning", None, "graphics bundle D3D11 warning"),
        (6, "venera_d3d12", "warning", None, "graphics bundle D3D12 warning"),
        (7, "dxvk", "warn", Some("default"), shader.as_str()),
        (8, "vkd3d-proton", "warn", Some("shader"), vkd.as_str()),
        (9, "mesa", "unknown", Some("default"), "graphics bundle Vulkan OS\n"),
        (10, "venus", "info", Some("MESA-VIRTIO"), "MESA-VIRTIO: info: graphics bundle Vulkan log\n"),
        (9, "mesa", "unknown", Some("default"), "graphics bundle WGL OS\n"),
        (10, "mesa", "info", Some("MESA"), "MESA: info: graphics bundle WGL log\n"),
    ];
    for (pid, bits, status) in [(pid64, 64, status64), (pid32, 32, status32)] {
        assert!(status.success(), "producer {bits}-bit failed: {status}");
        for (id, component, level, channel, text) in expected {
            let count = entries(&page, pid).filter(|r| {
                r.source == "etw" && r.component == component && r.raw_level == level && r.text == text
                    && r.structured.as_ref().is_some_and(|event| event.event_id == id
                        && event.fields.get("channel") == channel.map(|s| EventValue::AnsiString(s.into())).as_ref())
            }).count();
            assert_eq!(count, 1, "{bits}-bit pid={pid} event={id} {component} bytes={}", text.len());
            println!("private history {bits}-bit pid={pid} event={id} {component} bytes={}", text.len());
        }
    }
    assert!(pair_status.success(), "same-text producer failed: {pair_status}");
    assert!(live_pass, "live history report: {live_report}");
}
