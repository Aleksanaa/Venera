mod store;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    time::{Duration, Instant},
};
use store::{Result, Store};
use venera_debug_protocol::{encode, Envelope, Filter, Framer, Message, DEFAULT_MAX_LINE_BYTES};

#[derive(Parser)]
#[command(about = "Venera debug collection and history")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Receive {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        max_bytes: u64,
        #[arg(long)]
        control: PathBuf,
    },
    Status {
        #[arg(long)]
        control: PathBuf,
    },
    FilterSet {
        #[arg(long)]
        control: PathBuf,
        #[arg(long)]
        filter: String,
        #[arg(long)]
        duration_ms: Option<u64>,
    },
    FilterReset {
        #[arg(long)]
        control: PathBuf,
    },
    Export {
        #[arg(long)]
        database: PathBuf,
        /// Include records received at or after this RFC3339 timestamp.
        #[arg(long)]
        from: Option<String>,
        /// Include records received before this RFC3339 timestamp.
        #[arg(long)]
        until: Option<String>,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Receive {
            socket,
            database,
            max_bytes,
            control,
        } => receive(socket, database, max_bytes, control),
        Command::Export {
            database,
            from,
            until,
        } => {
            let range = store::ExportRange::parse(from.as_deref(), until.as_deref())?;
            store::export(&database, &mut std::io::stdout().lock(), &range)
        }
        Command::Status { control } => command(
            control,
            Message::StatusGet {
                request_id: "cli".into(),
            },
        ),
        Command::FilterReset { control } => command(
            control,
            Message::FilterReset {
                request_id: "cli".into(),
            },
        ),
        Command::FilterSet {
            control,
            filter,
            duration_ms,
        } => {
            let filter: Filter = serde_json::from_str(&filter)?;
            command(
                control,
                Message::FilterSet {
                    request_id: "cli".into(),
                    filter,
                    duration_ms,
                },
            )
        }
    }
}
fn command(control: PathBuf, message: Message) -> Result<()> {
    let mut stream = UnixStream::connect(control)?;
    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(&encode(&Envelope::new(message))?)?;
    let mut line = String::new();
    BufReader::new(stream)
        .take((DEFAULT_MAX_LINE_BYTES + 1) as u64)
        .read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(
            "control connection ended; query status to confirm the effective configuration".into(),
        );
    }
    print!("{line}");
    let response: Value = serde_json::from_str(&line)?;
    if response.get("ok") == Some(&Value::Bool(false)) {
        return Err("request failed; query status for the effective configuration".into());
    }
    Ok(())
}

struct Pending {
    client: UnixStream,
    framer: Framer,
    id: Option<String>,
    original_id: Option<String>,
    operation: Option<&'static str>,
    since: Instant,
    response: Option<Vec<u8>>,
    response_offset: usize,
}
impl Pending {
    fn new(client: UnixStream) -> Self {
        Self {
            client,
            framer: Framer::new(65536),
            id: None,
            original_id: None,
            operation: None,
            since: Instant::now(),
            response: None,
            response_offset: 0,
        }
    }
}
fn outcome(code: &str, message: &str) -> Value {
    json!({"major":1,"type":"host.request.result","ok":false,"error":{"code":code,"message":message}})
}
fn reply(pending: &mut Pending, mut value: Value) {
    if let (Some(id), Some(operation)) = (&pending.original_id, pending.operation) {
        value["type"] = json!("request.result");
        value["request_id"] = json!(id);
        value["operation"] = json!(operation);
    }
    let mut bytes = serde_json::to_vec(&value).expect("JSON value");
    bytes.push(b'\n');
    pending.response = Some(bytes);
}

fn read_request(p: &mut Pending) -> Option<Envelope> {
    let mut bytes = [0; 4096];
    match p.client.read(&mut bytes) {
        Ok(0) => reply(
            p,
            outcome(
                "incomplete",
                "Control request ended before a complete message",
            ),
        ),
        Ok(n) => {
            if let Some(item) = p.framer.push(&bytes[..n]).into_iter().next() {
                match item {
                    Ok(envelope) => {
                        let correlation = match &envelope.body {
                            Message::StatusGet { request_id } => Some((request_id, "status.get")),
                            Message::FilterSet { request_id, .. } => {
                                Some((request_id, "filter.set"))
                            }
                            Message::FilterReset { request_id } => {
                                Some((request_id, "filter.reset"))
                            }
                            _ => None,
                        };
                        if let Some((id, operation)) = correlation {
                            p.original_id = Some(id.clone());
                            p.operation = Some(operation);
                            return Some(envelope);
                        }
                        reply(
                            p,
                            outcome("invalid_request", "Expected a status or filter request"),
                        );
                    }
                    Err(error) => reply(p, outcome("invalid_request", &format!("{error:?}"))),
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => (),
        Err(_) => reply(p, outcome("incomplete", "Control connection interrupted")),
    }
    None
}

fn write_response(p: &mut Pending) -> bool {
    let Some(bytes) = p.response.as_ref() else {
        return false;
    };
    match p.client.write(&bytes[p.response_offset..]) {
        Ok(0) => true,
        Ok(n) => {
            p.response_offset += n;
            p.response_offset == bytes.len()
        }
        Err(error) => error.kind() != ErrorKind::WouldBlock,
    }
}

fn receive(socket: PathBuf, database: PathBuf, max_bytes: u64, control: PathBuf) -> Result<()> {
    let store = Store::open(&database, max_bytes)?;
    let listener = match UnixListener::bind(&control) {
        Ok(listener) => listener,
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            use std::os::unix::fs::FileTypeExt;
            if !std::fs::symlink_metadata(&control)?.file_type().is_socket() {
                return Err(error.into());
            }
            match UnixStream::connect(&control) {
                Err(connect) if connect.kind() == ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(&control)?;
                    UnixListener::bind(&control)?
                }
                _ => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    listener.set_nonblocking(true)?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(control);
    let mut guest: Option<UnixStream> = None;
    let mut parser = Framer::new(DEFAULT_MAX_LINE_BYTES);
    let mut pending: Option<Pending> = None;
    let mut busy_clients: Vec<Pending> = Vec::new();
    let mut output: Vec<u8> = Vec::new();
    let mut offset = 0;
    let mut next_connect = Instant::now();
    let mut serial = 0u64;
    let run_id = store::now();
    store.append(&json!({"major":1,"type":"host.started"}))?;
    loop {
        if guest.is_none() && Instant::now() >= next_connect {
            next_connect = Instant::now() + Duration::from_millis(200);
            if let Ok(stream) = UnixStream::connect(&socket) {
                stream.set_nonblocking(true)?;
                parser.reset();
                output = b"\n".to_vec();
                output.extend(encode(&Envelope::new(Message::Hello {
                    role: "host".into(),
                    software_version: env!("CARGO_PKG_VERSION").into(),
                    capabilities: vec!["status".into(), "filter".into()],
                }))?);
                serial += 1;
                output.extend(encode(&Envelope::new(Message::StatusGet {
                    request_id: format!("{run_id}-{serial}"),
                }))?);
                offset = 0;
                guest = Some(stream);
                store.append(&json!({"major":1,"type":"host.connection","connected":true}))?;
            }
        }
        for _ in 0..16 {
            match listener.accept() {
                Ok((client, _)) => {
                    client.set_nonblocking(true)?;
                    let client = Pending::new(client);
                    if pending.is_none() {
                        pending = Some(client);
                    } else if busy_clients.len() < 32 {
                        busy_clients.push(client);
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
        busy_clients.retain_mut(|p| {
            if p.response.is_none() && read_request(p).is_some() {
                reply(
                    p,
                    outcome(
                        "busy",
                        "Another control request is in progress; query status before retrying",
                    ),
                );
            }
            !write_response(p) && p.since.elapsed() < Duration::from_millis(500)
        });
        if let Some(p) = pending.as_mut() {
            if p.id.is_none() && p.response.is_none() {
                if let Some(mut envelope) = read_request(p) {
                    serial += 1;
                    let id = format!("{run_id}-{serial}");
                    let request_id = match &mut envelope.body {
                        Message::StatusGet { request_id }
                        | Message::FilterSet { request_id, .. }
                        | Message::FilterReset { request_id } => Some(request_id),
                        _ => None,
                    };
                    if let Some(request_id) = request_id {
                        if guest.is_some() && offset == output.len() {
                            *request_id = id.clone();
                            output = encode(&envelope)?;
                            offset = 0;
                            p.id = Some(id);
                        } else {
                            reply(
                                p,
                                outcome(
                                    "unavailable",
                                    "Guest connection is preparing; query status shortly",
                                ),
                            );
                        }
                    }
                }
            }
        }
        let mut disconnected = false;
        if let Some(stream) = guest.as_mut() {
            if offset < output.len() {
                match stream.write(&output[offset..]) {
                    Ok(0) => disconnected = true,
                    Ok(n) => offset += n,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => (),
                    Err(_) => disconnected = true,
                }
            }
            let mut bytes = [0; 65536];
            match stream.read(&mut bytes) {
                Ok(0) => disconnected = true,
                Ok(n) => {
                    for item in parser.push_raw(&bytes[..n]) {
                        match item {
                        Ok((envelope, raw)) => {
                            store.append(&raw)?;
                            if let Message::RequestResult { result } = envelope.body {
                                if let Some(p) = pending.as_mut() {
                                    if p.id.as_ref() == Some(&result.request_id) && p.operation == Some(result.operation.as_str()) && p.response.is_none() { reply(p, raw); }
                                }
                            }
                        }
                        Err(error) => store.append(&json!({"major":1,"type":"host.protocol_error","error":format!("{error:?}")}))?,
                    }
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => (),
                Err(_) => disconnected = true,
            }
        }
        if disconnected {
            guest = None;
            parser.reset();
            output.clear();
            offset = 0;
            store.append(&json!({"major":1,"type":"host.connection","connected":false}))?;
            if let Some(p) = pending.as_mut() {
                if p.response.is_none() && p.id.is_some() {
                    reply(
                        p,
                        outcome(
                            "outcome_unknown",
                            "Connection interrupted; query status after reconnection",
                        ),
                    );
                }
            }
        }
        let mut done = false;
        if let Some(p) = pending.as_mut() {
            if p.response.is_none() && p.since.elapsed() > Duration::from_secs(5) {
                reply(
                    p,
                    outcome(
                        "outcome_unknown",
                        "Request timed out; query status to confirm the effective configuration",
                    ),
                );
            }
            done = write_response(p) || p.since.elapsed() > Duration::from_secs(7);
        }
        if done {
            pending = None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
