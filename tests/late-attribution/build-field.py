#!/usr/bin/env python3
"""Build the late-attribution field kit with the project cross toolchain."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
TARGET = "x86_64-pc-windows-gnu"


def replace(path, old, new):
    path.write_text(splice(path.read_text(), old, new))


def splice(text, old, new):
    if text.count(old) != 1:
        raise ValueError(f"Staging anchor changed: {old}")
    return text.replace(old, new, 1)


def stage_pipe(win):
    ipc = win / "ipc.rs"
    text = ipc.read_text()
    begin = text.index("    fn helper_pair()")
    end = text.index("    #[test]", begin)
    # Same connected pair as native-graphics-pipe.rs; this build has no test-only IO probe field.
    pair = splice(text[begin:end], "                io_pending: None,\n", "")
    pair = splice(pair, "VeneraDebugGuest-test-", "VeneraAttributionField-test-")
    pair = splice(pair, "                PIPE_ACCESS_DUPLEX,", "                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,")
    begin = text.index("fn follow(panel: bool)")
    end = text.index("\n#[cfg(test)]\nmod tests", begin)
    follow = text[begin:end]
    follow = splice(follow, "fn follow(panel: bool)", "fn field_panel_loop(state: &Shared, owner: &mut PipeWorkers)")
    follow = splice(follow, "    loop {", "    while !stopped() {")
    follow = splice(follow, '''                connection = Some(if panel {
                    rpc_connect_with(Client::with_history_attribution())?
                } else {
                    rpc_connect()?
                });''', '''                let connected = field_connect(state, owner, Client::with_history_attribution())?;
                if connected.0.history_encoding() != HistoryEncoding::Attribution {
                    return Err(io::Error::other("Field pipe did not negotiate attribution"));
                }
                connection = Some(connected);''')
    follow = splice(follow, "                panel_output.page(&mut consumer, &page, filter, &mut output)?;", '''                panel_output.page(&mut consumer, &page, filter, &mut output)?;
                let (client, file) = connection.as_mut().unwrap();
                super::field_fixture::observe(page.next.position.0,
                    || field_pipe_check(client, file, page.next.position.0))?;''')
    follow = splice(follow, "match recover_panel(&mut panel_output, &page.next.journal_id, &mut output)",
                    "match field_recover_panel(state, owner, &mut panel_output, &page.next.journal_id, &mut output)")
    follow = splice(follow, "}\n\nfn recover_panel(\n", "    Ok(())\n}\n\nfn field_recover_panel(\n    state: &Shared,\n    owner: &mut PipeWorkers,\n")
    follow = splice(follow, "let (mut client, mut file) = rpc_connect()?;",
                    "let (mut client, mut file) = field_connect(state, owner, venera_debug_protocol::client::Client::default())?;")
    (win / "field_pipe.rs").write_text((HERE / "field-pipe.rs").read_text() + "\n" + pair + follow)
    ipc.write_text(text + '\ninclude!("field_pipe.rs");\n')


def run(*args, **kwargs):
    subprocess.run([str(a) for a in args], check=True, **kwargs)


def build(args, stage, targets):
    cargo = stage / "debug-channel"
    cargo.mkdir()
    for name in ("guest", "host", "protocol"):
        shutil.copytree(ROOT / "debug-channel" / name, cargo / name)
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copyfile(ROOT / "debug-channel" / name, cargo / name)
    tmf = Path("kmd/logging/generated/venera-recorder.tmf")
    (stage / tmf.parent).mkdir(parents=True)
    shutil.copyfile(ROOT / tmf, stage / tmf)
    win = cargo / "guest/src/windows"
    shutil.copyfile(HERE / "field-fixture.rs", win / "field_fixture.rs")
    stage_pipe(win)
    replace(win / "transport.rs", "const PORT: u32 = 49152;", f"const PORT: u32 = {args.port};")
    mod = win / "mod.rs"
    text = mod.read_text()
    begin = text.index("pub fn run(args:")
    end = text.index("pub fn history_request(", begin)
    mod.write_text(text[:begin] + '''mod field_fixture;
pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["logs", "--follow", "--panel"] => field_fixture::run(),
        ["logs", "--follow"] | ["status", "--json"] => ipc::cli(&args),
        _ => Err("Commands: logs --follow [--panel]; status --json".into()),
    }
}

''' + text[end:])
    replace(mod, "    let history = history.ok_or_else", '''    if matches!(request, Incoming::ForwardingSet { .. }) {
        return Err(RpcError::new(-32601, "Fixture forwarding is fixed"));
    }
    let history = history.ok_or_else''')
    replace(mod, "    history.rpc_encoded(request, local || forwarding, encoding)", '''    let ack = matches!(request, Incoming::HistoryAck { .. });
    let result = history.rpc_encoded(request, local || forwarding, encoding);
    if let Ok(value) = &result {
        if ack { field_fixture::ack(local, value); }
    }
    result''')
    env = dict(os.environ, CARGO_TARGET_DIR=str(targets))
    opts = ["--locked", "--offline", "--manifest-path", cargo / "Cargo.toml"]
    if args.vendor is not None:
        opts += ["--config", 'source.crates-io.replace-with="vendor"',
                 "--config", f"source.vendor.directory={json.dumps(str(args.vendor.resolve()), ensure_ascii=False)}"]
    # Discover Cargo configuration from the project while building the staged sources.
    cargo_cwd = ROOT / "debug-channel"
    run("cargo", "build", *opts, "--release", "-p", "venera-debug-guest", "--target", TARGET, cwd=cargo_cwd, env=env)
    run("cargo", "build", *opts, "--release", "-p", "venera-debug-host", cwd=cargo_cwd, env=env)

    panel = stage / "panel"
    shutil.copytree(ROOT / "panel", panel)
    panel_main = panel / "main.cpp"
    text = panel_main.read_text()
    if text.count('L"venera-panel"') != 2:
        raise ValueError("Panel window class anchors changed")
    text = text.replace('L"venera-panel"', 'L"venera-attribution-field-panel"')
    text = splice(text, r'L"Local\\venera-panel"', r'L"Local\\venera-attribution-field-panel"')
    panel_main.write_text(splice(text, 'L"venera-panel-activate"', 'L"venera-attribution-field-panel-activate"'))
    path = panel / "debugchannel.cpp"
    text = path.read_text()
    begin = text.index("std::wstring guestPath() {")
    end = text.index("locale::Message run(", begin)
    path.write_text(text[:begin] + r'''std::wstring guestPath() {
    wchar_t self[32768];
    DWORD n = GetModuleFileNameW(nullptr, self, 32768);
    if (!n || n >= 32768) return {};
    std::wstring path(self, n);
    return path.substr(0, path.find_last_of(L'\\') + 1) + L"venera-debug-guest.exe";
}
''' + text[end:])
    replace(path, 'if(!revisable || (!received && !error.empty() && WaitForSingleObject(cancel_,0)!=WAIT_OBJECT_0)) {',
            'if(!revisable) {')
    for directory in ("docs", "refs", "third_party", "cmake", "umd"):
        (stage / directory).symlink_to(ROOT / directory, target_is_directory=True)
    top = (ROOT / "CMakeLists.txt").read_text().split("# The D3D12 and D3D11 implementations", 1)
    if len(top) != 2:
        raise ValueError("Panel CMake staging anchor changed")
    (stage / "CMakeLists.txt").write_text(top[0] + "add_subdirectory(panel)\n")
    obj = stage / "panel-build"
    run("cmake", "-S", stage, "-B", obj, "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release",
        "-DCMAKE_TOOLCHAIN_FILE=cmake/mingw-w64.cmake", *args.cmake_arg)
    run("cmake", "--build", obj, "--target", "venera-panel", "--parallel", args.jobs)
    return obj / "panel/venera-panel.exe"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--vendor", type=Path, default=os.environ.get("CARGO_VENDOR_DIR") or None)
    parser.add_argument("--cmake-arg", action="append", default=[])
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()
    if not 49153 <= args.port <= 65535:
        parser.error("Choose a field port from 49153 through 65535")
    out = args.out.resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    targets = out.parent / "late-attribution-field-target"
    if out.exists():
        parser.error(f"Choose a new kit directory: {out}")
    with tempfile.TemporaryDirectory(dir=out.parent, prefix="field-build-") as work:
        stage = Path(work)
        panel = build(args, stage, targets)
        kit = stage / "kit"
        windows = kit / "windows"
        windows.mkdir(parents=True)
        shutil.copyfile(panel, windows / "venera-panel.exe")
        shutil.copyfile(targets / TARGET / "release/venera-debug-guest.exe", windows / "venera-debug-guest.exe")
        shutil.copy(targets / "release/venera-debug-host", kit / "venera-debug-host")
        for name in ("FIELD.md", "check-field.py"):
            shutil.copyfile(HERE / name, kit / name)
        (kit / "endpoint.json").write_text(json.dumps({"port": args.port, "pipe": "VeneraAttributionField-test-<pid>-<n>"}) + "\n")
        kit.rename(out)
    print(out)


if __name__ == "__main__":
    main()
