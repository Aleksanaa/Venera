use crate::transport::{Connector, Stream, VsockConnector};
use std::{io, process::Command, time::Duration};

fn virsh(args: &[&str]) -> io::Result<String> {
    let output = Command::new("virsh")
        .args(["--readonly", "--connect", "qemu:///system"])
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| io::Error::other(format!("starting virsh: {e}")))?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "virsh {}: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(io::Error::other)
}

fn domains() -> io::Result<Vec<(String, String)>> {
    virsh(&["list", "--all", "--uuid", "--name"])?
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

fn live_cid(uuid: &str) -> io::Result<Option<u32>> {
    // Only an active domain XML has a runtime id. Inactive XML may still have a static CID.
    let attr = virsh(&[
        "dumpxml",
        uuid,
        "--xpath",
        "/domain[@id]/devices/vsock/cid/@address",
    ])?;
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
        let state = virsh(&["domstate", &uuid])?;
        let vsock =
            live_cid(&uuid)?.map_or_else(|| "unavailable".into(), |cid| format!("CID {cid}"));
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
                let cid = live_cid(uuid)?
                    .ok_or_else(|| io::Error::other(format!("no live vsock CID for {uuid}")))?;
                VsockConnector::new(cid, *port)?.connect(timeout)
            }
        }
    }
}
