use crate::transport::{Connector, Stream, VsockConnector};
use std::{
    io,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command};

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

fn virsh(args: &[&str], deadline: Instant) -> io::Result<String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut child = Command::new("virsh")
                .args(["--readonly", "--connect", "qemu:///system"])
                .args(args)
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| io::Error::other(format!("starting virsh: {e}")))?;
            let mut out = child.stdout.take().unwrap();
            let mut err = child.stderr.take().unwrap();
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            let result = tokio::time::timeout_at(deadline.into(), async {
                tokio::try_join!(
                    child.wait(),
                    out.read_to_end(&mut stdout),
                    err.read_to_end(&mut stderr)
                )
            })
            .await;
            let status = match result {
                Ok(result) => result?.0,
                Err(_) => {
                    child.kill().await?;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("virsh {} timed out", args[0]),
                    ));
                }
            };
            if !status.success() {
                return Err(io::Error::other(format!(
                    "virsh {}: {}",
                    args[0],
                    String::from_utf8_lossy(&stderr).trim()
                )));
            }
            String::from_utf8(stdout).map_err(io::Error::other)
        })
}

fn domains() -> io::Result<Vec<(String, String)>> {
    virsh(
        &["list", "--all", "--uuid", "--name"],
        Instant::now() + QUERY_TIMEOUT,
    )?
    .lines()
    .filter(|l| !l.is_empty())
    .map(|line| {
        let (uuid, name) = line
            .split_once(' ')
            .ok_or_else(|| io::Error::other("invalid virsh domain listing"))?;
        if uuid.len() != 36
            || !uuid.bytes().enumerate().all(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            })
        {
            return Err(io::Error::other("invalid domain UUID"));
        }
        Ok((uuid.into(), name.into()))
    })
    .collect()
}

pub fn resolve(name: &str) -> io::Result<String> {
    domains()?
        .into_iter()
        .find(|(_, n)| n == name)
        .map(|(u, _)| u)
        .ok_or_else(|| io::Error::other(format!("unknown libvirt domain: {name}")))
}

fn live_cid(uuid: &str, deadline: Instant) -> io::Result<Option<u32>> {
    // Only an active domain XML has a runtime id. Inactive XML may still have a static CID.
    let attr = virsh(
        &[
            "dumpxml",
            uuid,
            "--xpath",
            "/domain[@id]/devices/vsock/cid/@address",
        ],
        deadline,
    )?;
    if attr.trim().is_empty() {
        return Ok(None);
    }
    attr.trim()
        .strip_prefix("address=\"")
        .and_then(|s| s.strip_suffix('"'))
        .and_then(|s| s.parse().ok())
        .filter(|n| (3..u32::MAX).contains(n))
        .map(Some)
        .ok_or_else(|| io::Error::other(format!("invalid live vsock CID for {uuid}")))
}

pub fn list() -> io::Result<()> {
    let domains = domains()?;
    println!("NAME\tSTATE\tVSOCK");
    for (uuid, name) in domains {
        let state = virsh(&["domstate", &uuid], Instant::now() + QUERY_TIMEOUT)?;
        let vsock = live_cid(&uuid, Instant::now() + QUERY_TIMEOUT)?
            .map_or_else(|| "unavailable".into(), |cid| format!("CID {cid}"));
        println!("{name}\t{}\t{vsock}", state.trim());
    }
    Ok(())
}

pub enum Target {
    Domain { uuid: String, port: u32 },
    Direct(VsockConnector),
}

impl Connector for Target {
    fn connect(&mut self, timeout: Duration) -> io::Result<Box<dyn Stream>> {
        match self {
            Self::Direct(vsock) => vsock.connect(timeout),
            Self::Domain { uuid, port } => {
                let deadline = Instant::now() + timeout;
                let cid = live_cid(uuid, deadline)?
                    .ok_or_else(|| io::Error::other(format!("no live vsock CID for {uuid}")))?;
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or(io::ErrorKind::TimedOut)?;
                VsockConnector::new(cid, *port)?.connect(remaining)
            }
        }
    }
}
