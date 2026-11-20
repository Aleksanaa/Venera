// SPDX-License-Identifier: MIT
use super::*;
use serde_json::{json, Value};
use std::process::Command;
use venera_debug_protocol::v1::{EventValue, HistoryEvent, HistoryPage};

fn run(root: &Path, dir: &Path, exe: &str, args: &[&Path]) -> String {
    let output = Command::new(root.join(exe))
        .current_dir(dir)
        .args(args)
        .envs([
            ("DXVK_LOG_LEVEL", "warn"), ("DXVK_LOG_PATH", "."),
            ("VKD3D_DEBUG", "err"), ("VKD3D_SHADER_DEBUG", "warn"),
            ("VKD3D_LOG_FILE", "shader.log"),
            ("MESA_LOG", "windbg"), ("MESA_LOG_LEVEL", "debug"),
            ("TEST_SHADER_TEXT", "1"), ("VENERA_LOG", "umd.log"), ("VENERA_VERBOSE", "0"),
        ])
        .env_remove("VKD3D_LOG_BUFFERED")
        .output().unwrap();
    fs::write(dir.join(format!("{exe}.stdout.txt")), &output.stdout).unwrap();
    fs::write(dir.join(format!("{exe}.stderr.txt")), &output.stderr).unwrap();
    assert!(output.status.success(), "{exe}: {}", output.status);
    String::from_utf8(output.stdout).unwrap()
}

#[test]
#[ignore = "Native Windows graphics producers and ETW file session"]
fn native_graphics_history() {
    let kit = fs::canonicalize(PathBuf::from(std::env::var_os("VENERA_GRAPHICS_KIT").unwrap())).unwrap();
    // Win32 module paths are reported in DOS form for a DOS input path.
    let kit = PathBuf::from(kit.to_str().unwrap().trim_start_matches(r"\\?\"));
    let tag = format!("{}-{}", std::process::id(), clock::filetime());
    let dir = std::env::temp_dir().join(format!("venera-native-graphics-{tag}"));
    fs::create_dir(&dir).unwrap();
    let trace = dir.join("graphics.etl");
    let name = format!("VeneraGraphicsQA-{tag}");
    println!("session={name} artifacts={}", dir.display());
    let mut session = FileSession::start(&name, &trace, 0, 32).unwrap();
    session.enable(&etw::KERNEL_PROVIDER, 5, 1).unwrap();
    let mut pids = Vec::new();
    for engine in ["dxvk", "vkd3d", "mesa", "venera"] {
        let out = run(&kit, &dir, &format!("source-runtime-{engine}.exe"), &[]);
        let result: Value = serde_json::from_str(out.trim()).unwrap();
        pids.push(result["pid"].as_u64().unwrap() as u32);
    }
    let out = run(&kit, &dir, "native-graphics-package.exe", &[&kit.join("payload")]);
    let package: u32 = out.lines().find_map(|line| line.strip_prefix("package-pid="))
        .unwrap().trim().parse().unwrap();
    let ods = "native-graphics ordinary ODS control";
    assert_eq!(out.lines().filter(|line| *line == format!("debugger pid={package} text={ods}")).count(), 1);
    let post = session.control_status().unwrap();
    assert_eq!((post.2, post.3, post.4), (0, 0, 0), "{post:?}");
    session.stop().unwrap();

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

    let result = crate::windows::ipc::tests::native_graphics_pipe::read(state, json!({"after":null,"through":null,"limit":256}));
    fs::write(dir.join("history-result.json"), serde_json::to_vec(&result).unwrap()).unwrap();
    let page: HistoryPage = serde_json::from_value(result.clone()).unwrap();
    crate::history::finish(Vec::new(), &stop, owner).unwrap();
    assert_eq!(page.range.high_water, page.next.position, "history page ended early");
    assert!(page.missing.is_empty());
    let shader = "mov r0, r1\n".repeat(8000);
    assert_eq!(shader.len(), 88_000);
    let vkd = format!("compile: {shader}");
    let expected = [
        (pids[0], 7, "dxvk", "warn", Some("default"), shader.as_str()),
        (pids[1], 8, "vkd3d-proton", "warn", Some("shader"), vkd.as_str()),
        (pids[2], 9, "mesa", "unknown", Some("default"), "source-runtime mesa os_log_message literal\n"),
        (pids[2], 10, "venus", "info", Some("MESA-VIRTIO"), "MESA-VIRTIO: info: source-runtime mesa virtio info literal\n"),
        (pids[3], 5, "venera_d3d11", "warning", None, "source-runtime venera warning literal"),
        (package, 5, "venera_d3d11", "warning", None, "native-graphics package UMD warning"),
        (package, 7, "dxvk", "warn", Some("default"), shader.as_str()),
        (package, 8, "vkd3d-proton", "warn", Some("shader"), vkd.as_str()),
        (package, 9, "mesa", "unknown", Some("default"), "native-graphics package Mesa OS\n"),
        (package, 10, "venus", "info", Some("MESA-VIRTIO"), "MESA-VIRTIO: info: native-graphics package Mesa log\n"),
    ];
    for (pid, id, component, level, channel, text) in expected {
        let count = page.records.iter().filter(|entry| {
            let HistoryEvent::Log { record } = &entry.event else { return false };
            let Some(structured) = &record.structured else { return false };
            record.source == "etw" && record.component == component && record.raw_level == level
                && record.process.as_ref().is_some_and(|process| process.pid == pid)
                && structured.event_id == id && record.text == text
                && structured.fields.get("channel") == channel.map(|s| EventValue::AnsiString(s.into())).as_ref()
        }).count();
        assert_eq!(count, 1, "pid={pid} event={id} {component}/{level} body={} bytes", text.len());
        println!("history.read pid={pid} event={id} {component}/{level} body={} bytes", text.len());
    }
    for entry in &page.records {
        match &entry.event {
            HistoryEvent::Log { record } => assert!(!record.text.contains(ods)),
            HistoryEvent::Gap { gap } => panic!("history gap: {gap:?}"),
        }
    }
    fs::write(dir.join("history-page.json"), serde_json::to_vec(&result).unwrap()).unwrap();
}
