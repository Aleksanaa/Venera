use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::Path,
    process::{Child, Command},
    thread::sleep,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_venera-debug-host");
const UUID: &str = "11223344-5566-7788-99aa-bbccddeeff00";
const OTHER: &str = "00000000-0000-0000-0000-000000000001";

fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("domains"),
        format!("{UUID} 73\n{OTHER} Build guest\n"),
    )
    .unwrap();
    fs::write(dir.path().join("cid"), " address=\"51\"\n").unwrap();
    let script = dir.path().join("virsh");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
[ "$1 $2 $3" = '--readonly --connect qemu:///system' ] || exit 1
shift 3
case "$*" in
  'list --all --uuid --name') cat domains;;
  'domstate {UUID}') echo running;;
  'domstate {OTHER}') echo 'shut off';;
  'dumpxml {UUID} --xpath /domain[@id]/devices/vsock/cid/@address') cat cid;;
  'dumpxml {OTHER} --xpath /domain[@id]/devices/vsock/cid/@address') echo;;
  *) echo "unexpected domain query: $*" >&2; exit 1;;
esac
"#
        ),
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn cli(dir: &Path) -> Command {
    let mut command = Command::new(BIN);
    let path = std::env::join_paths(
        std::iter::once(dir.into())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    command
        .current_dir(dir)
        .env("PATH", path)
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_RUNTIME_DIR", dir);
    command
}

#[test]
fn list_reports_names_states_and_live_addresses() {
    let dir = fixture();
    let out = cli(dir.path()).arg("list").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "NAME\tSTATE\tVSOCK\n73\trunning\tCID 51\nBuild guest\tshut off\tunavailable\n"
    );
}

struct Service(Child);
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_history_error(dir: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(mut stream) = UnixStream::connect(dir.join("venera/control.sock")) {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            writeln!(
                stream,
                r#"{{"jsonrpc":"2.0","method":"connection.get","params":{{}},"id":"c"}}"#
            )
            .unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            let packet: serde_json::Value = serde_json::from_str(&line).unwrap();
            if packet["result"]["history"]["error"] == expected {
                return;
            }
        }
        assert!(Instant::now() < deadline, "waiting for {expected}");
        sleep(Duration::from_millis(20));
    }
}

#[test]
fn domain_service_recovers_after_a_cid_lookup_times_out() {
    let dir = fixture();
    let script = dir.path().join("virsh");
    let contents = fs::read_to_string(&script).unwrap().replace(
        "cat cid",
        "if [ -f stall ]; then exec sleep 30; else cat cid; fi",
    );
    fs::write(script, contents).unwrap();
    fs::write(dir.path().join("stall"), "").unwrap();
    let _service = Service(
        cli(dir.path())
            .args(["serve", "73", "--no-web"])
            .spawn()
            .unwrap(),
    );
    wait_history_error(dir.path(), "virsh dumpxml timed out");
    fs::write(dir.path().join("cid"), "").unwrap();
    fs::remove_file(dir.path().join("stall")).unwrap();
    wait_history_error(dir.path(), &format!("no live vsock CID for {UUID}"));
}

#[test]
fn numeric_guest_keeps_its_archive_and_identity_across_renames() {
    let dir = fixture();
    fs::write(dir.path().join("cid"), "").unwrap();
    let _service = Service(
        cli(dir.path())
            .args(["serve", "73", "--no-web"])
            .spawn()
            .unwrap(),
    );
    wait_history_error(dir.path(), &format!("no live vsock CID for {UUID}"));
    let database = dir.path().join(format!("data/venera/{UUID}/history.db"));
    let export = cli(dir.path())
        .args(["export", "--database"])
        .arg(database)
        .output()
        .unwrap();
    assert!(
        String::from_utf8(export.stdout)
            .unwrap()
            .contains("host.started")
    );
    let conflict = cli(dir.path())
        .args(["serve", "73", "--no-web"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8(conflict.stderr)
            .unwrap()
            .contains("is in use")
    );
    fs::write(
        dir.path().join("domains"),
        format!("{UUID} renamed\n{OTHER} 73\n"),
    )
    .unwrap();
    fs::write(dir.path().join("cid"), " address=\"2\"\n").unwrap();
    wait_history_error(dir.path(), &format!("invalid live vsock CID for {UUID}"));
}

#[test]
fn direct_connection_requires_a_database_and_exclusive_address() {
    for (args, diagnostic) in [
        (vec!["serve", "--cid", "73"], "--database"),
        (
            vec!["serve", "73", "--cid", "73", "--database", "archive"],
            "cannot be used with",
        ),
    ] {
        let result = Command::new(BIN).args(args).output().unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(
            String::from_utf8(result.stderr)
                .unwrap()
                .contains(diagnostic)
        );
    }
}
