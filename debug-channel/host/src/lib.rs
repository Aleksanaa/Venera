mod history_sync;
mod store;
pub mod transport;
#[cfg(feature = "web")]
mod web;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    time::{Duration, Instant},
};
use store::{Result, Store};
use venera_debug_protocol::client::Client;
use venera_debug_protocol::{v1, Filter, DEFAULT_MAX_LINE_BYTES};

#[derive(Parser)]
#[command(about = "Venera debug collection and history")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, value_parser = clap::value_parser!(u32).range(3..=4294967294))]
        cid: u32,
        #[arg(long, default_value_t = 49152, value_parser = clap::value_parser!(u32).range(1..=4294967294))]
        port: u32,
        #[arg(long)]
        database: PathBuf,
        #[arg(long, value_parser = clap::value_parser!(u64).range(store::MIN_STORAGE_BYTES..))]
        max_bytes: u64,
        #[arg(long)]
        control: PathBuf,
        #[arg(long)]
        assets: Option<PathBuf>,
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: std::net::SocketAddr,
        #[arg(long)]
        no_web: bool,
    },
    Discover {
        #[arg(long)]
        control: PathBuf,
    },
    Status {
        #[arg(long)]
        control: PathBuf,
    },
    SystemInfo {
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
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        until: Option<String>,
    },
}
pub fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Serve {
            cid,
            port,
            database,
            max_bytes,
            control,
            assets,
            listen,
            no_web,
        } => serve(
            transport::VsockConnector::new(cid, port)?,
            database,
            max_bytes,
            control,
            assets,
            listen,
            no_web,
        ),
        Command::Export {
            database,
            from,
            until,
        } => {
            let range = store::ExportRange::parse(from.as_deref(), until.as_deref())?;
            store::export(&database, &mut std::io::stdout().lock(), &range)
        }
        Command::Discover { control } => command(control, "rpc.discover", json!({})),
        Command::Status { control } => command(control, "status.get", json!({})),
        Command::SystemInfo { control } => command(control, "system.info.get", json!({})),
        Command::FilterReset { control } => command(control, "filter.reset", json!({})),
        Command::FilterSet {
            control,
            filter,
            duration_ms,
        } => {
            let filter: Filter = serde_json::from_str(&filter)?;
            command(
                control,
                "filter.set",
                json!({"filter":filter,"duration_ms":duration_ms.map(|v| v.to_string())}),
            )
        }
    }
}
fn command(control: PathBuf, method: &str, params: Value) -> Result<()> {
    let mut stream = UnixStream::connect(control)?;
    stream.set_read_timeout(Some(Duration::from_secs(if method == "system.info.get" {
        50
    } else {
        8
    })))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(&rpc_request("cli", method, params)?)?;
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
    if response.get("error").is_some() {
        return Err(response["error"]["message"]
            .as_str()
            .unwrap_or("request failed")
            .into());
    }
    Ok(())
}
fn rpc_request(id: &str, method: &str, params: Value) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn host_endpoint() -> v1::EndpointInfo {
    v1::EndpointInfo::new(v1::EndpointRole::Host, env!("CARGO_PKG_VERSION"))
}
fn outcome(message: &str, code: &str) -> v1::RpcError {
    v1::RpcError {
        code: -32000,
        message: message.into(),
        data: Some(json!({"code":code,"message":message})),
    }
}
fn encode_value(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).expect("JSON value");
    bytes.push(b'\n');
    bytes
}
fn error_value(id: v1::RpcId, error: v1::RpcError) -> Value {
    serde_json::to_value(v1::RpcResponse::Error {
        jsonrpc: v1::Version::V2,
        id,
        error,
    })
    .unwrap()
}
struct Pending {
    client: UnixStream,
    framer: v1::JsonlFramer,
    batch: Option<v1::RpcBatch>,
    slot: Option<v1::Correlation>,
    id: Option<String>,
    method: Option<v1::Method>,
    since: Instant,
    response: Option<Vec<u8>>,
    response_offset: usize,
}
impl Pending {
    fn new(client: UnixStream) -> Self {
        Self {
            client,
            framer: Default::default(),
            batch: None,
            slot: None,
            id: None,
            method: None,
            since: Instant::now(),
            response: None,
            response_offset: 0,
        }
    }
    fn finish(&mut self, result: v1::RpcResult<Value>) {
        if let (Some(batch), Some(slot)) = (&mut self.batch, self.slot.take()) {
            let _ = batch.complete(slot, result);
        }
        self.flush_batch();
    }
    fn flush_batch(&mut self) {
        if let Some(value) = self.batch.as_mut().and_then(v1::RpcBatch::take_response) {
            self.response = Some(encode_value(&value));
        }
    }
}
// connection.get is a host-local query, decoded through the shared empty-params call shape.
fn read_control(p: &mut Pending) -> Option<(v1::RpcPacket, bool)> {
    let mut bytes = [0; 4096];
    match p.client.read(&mut bytes) {
        Ok(0) => {
            p.response = Some(encode_value(&error_value(
                v1::RpcId::Null,
                v1::RpcError::new(-32600, "Incomplete request"),
            )))
        }
        Ok(n) => {
            for frame in p.framer.push(&bytes[..n]) {
                match frame {
                    Err(error) => {
                        p.response = Some(encode_value(&error_value(v1::RpcId::Null, error)))
                    }
                    Ok(frame) => {
                        let mut value: Value =
                            serde_json::from_slice(frame.bytes()).unwrap_or(Value::Null);
                        let local = value["method"] == "connection.get";
                        if local {
                            value["method"] = json!("endpoint.info");
                        }
                        let packet = if local {
                            v1::RpcPacket::decode(
                                &serde_json::to_vec(&value).unwrap(),
                                v1::Receiver::Guest,
                            )
                        } else {
                            v1::RpcPacket::decode(frame.bytes(), v1::Receiver::Guest)
                        };
                        return Some((packet, local));
                    }
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock => (),
        Err(_) => {
            p.response = Some(encode_value(&error_value(
                v1::RpcId::Null,
                v1::RpcError::new(-32600, "Control connection interrupted"),
            )))
        }
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
        Err(e) => e.kind() != ErrorKind::WouldBlock,
    }
}
fn control_packet(
    p: &mut Pending,
    packet: v1::RpcPacket,
    local: bool,
    busy: bool,
    connection: &Value,
    client: &mut Client,
) -> Result<()> {
    p.batch = Some(packet.batch);
    for item in packet.items {
        if let v1::PacketItem::Request(request) = item {
            let method = v1::Method::from_name(request.incoming.method()).unwrap();
            let Some(slot) = request.correlation else {
                continue;
            };
            let immediate = if local {
                Some(Ok(connection.clone()))
            } else if method == v1::Method::EndpointInfo {
                Some(Ok(serde_json::to_value(host_endpoint())?))
            } else if busy || p.id.is_some() {
                Some(Err(outcome(
                    "Another control request is in progress; query status before retrying",
                    "busy",
                )))
            } else {
                client.require(method).err().map(Err)
            };
            if let Some(result) = immediate {
                let _ = p.batch.as_mut().unwrap().complete(slot, result);
                continue;
            }
            let params = match request.incoming {
                v1::Incoming::FilterSet { params, .. } => serde_json::to_value(params)?,
                v1::Incoming::ForwardingSet { params, .. } => serde_json::to_value(params)?,
                v1::Incoming::HistoryRead { params, .. } => serde_json::to_value(params)?,
                v1::Incoming::HistoryAck { params, .. } => serde_json::to_value(params)?,
                _ => json!({}),
            };
            let id = match client.request(method, params) {
                Ok(id) => id,
                Err(error) => {
                    let _ = p.batch.as_mut().unwrap().complete(slot, Err(error));
                    continue;
                }
            };
            p.id = Some(id);
            p.method = Some(method);
            p.slot = Some(slot);
            p.since = Instant::now();
        }
    }
    p.flush_batch();
    if p.batch.as_ref().is_some_and(v1::RpcBatch::is_complete) && p.response.is_none() {
        p.response = Some(Vec::new());
    }
    Ok(())
}
fn project(incoming: &v1::Incoming) -> Option<Value> {
    match incoming {
        v1::Incoming::Log(record) => serde_json::to_value(record).ok().map(|mut v| {
            v["major"] = json!(1);
            v["type"] = json!("log");
            v
        }),
        v1::Incoming::Gap(gap) => serde_json::to_value(gap).ok().map(|mut v| {
            v["major"] = json!(1);
            v["type"] = json!("gap");
            v
        }),
        v1::Incoming::StatusChanged(status) => serde_json::to_value(status).ok().map(|mut v| {
            v["major"] = json!(1);
            v["type"] = json!("status");
            v
        }),
        _ => None,
    }
}
fn guest_packet(
    bytes: &[u8],
    diagnostics: &history_sync::Diagnostics,
    client: &mut Client,
    mut pending: Option<&mut Pending>,
) -> Result<()> {
    for reply in client.receive(bytes).map_err(|error| error.message)? {
        if let Some(p) = pending.as_deref_mut() {
            if p.id.as_ref() == Some(&reply.id) && p.response.is_none() {
                p.finish(reply.result);
            }
        }
    }
    for (event, source) in client.take_events() {
        if let Some(projected) = project(&event) {
            if projected["type"] == "status" {
                diagnostics.append(projected, Some(source));
            }
        }
    }
    for error in client.take_errors() {
        diagnostics.append(
            json!({"major":1,"type":"host.protocol_error","error":error}),
            None,
        );
    }
    Ok(())
}
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
struct Collector {
    archive: Store,
    listener: UnixListener,
    _cleanup: Cleanup,
}
async fn initialize(
    database: &std::path::Path,
    max_bytes: u64,
    control: PathBuf,
) -> Result<Collector> {
    let store = Store::open(database, max_bytes)?;
    let listener = match UnixListener::bind(&control) {
        Ok(listener) => listener,
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            use std::os::unix::fs::FileTypeExt;
            if !std::fs::symlink_metadata(&control)?.file_type().is_socket() {
                return Err(error.into());
            }
            match tokio::time::timeout(
                Duration::from_millis(200),
                tokio::net::UnixStream::connect(&control),
            )
            .await
            {
                Ok(Err(e)) if e.kind() == ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(&control)?;
                    UnixListener::bind(&control)?
                }
                _ => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    let cleanup = Cleanup(control);
    listener.set_nonblocking(true)?;
    store.append(&json!({"major":1,"type":"host.started"}))?;
    Ok(Collector {
        archive: store,
        listener,
        _cleanup: cleanup,
    })
}
pub fn serve(
    connector: impl transport::Connector,
    database: PathBuf,
    max_bytes: u64,
    control: PathBuf,
    assets: Option<PathBuf>,
    listen: std::net::SocketAddr,
    no_web: bool,
) -> Result<()> {
    #[cfg(not(feature = "web"))]
    if !no_web {
        return Err("Build with the web feature to serve the dashboard, or select --no-web".into());
    }
    #[cfg(feature = "web")]
    let assets = if no_web {
        assets
    } else {
        Some(assets.ok_or("--assets is required to serve the dashboard")?)
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let collector = initialize(&database, max_bytes, control.clone()).await?;
        #[cfg(feature = "web")]
        let listener = if no_web {
            None
        } else {
            Some(tokio::net::TcpListener::bind(listen).await?)
        };
        #[cfg(not(feature = "web"))]
        let _ = (assets, listen);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (completed, mut completion) = tokio::sync::oneshot::channel();
        let worker = std::thread::Builder::new()
            .name("collector".into())
            .spawn(move || {
                let result = collect(collector, connector, worker_stop).map_err(|e| e.to_string());
                let _ = completed.send(result);
            })?;
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let http = async move {
            #[cfg(feature = "web")]
            if let Some(listener) = listener {
                return web::serve(database, assets.unwrap(), listener, control, shutdown_rx).await;
            }
            #[cfg(not(feature = "web"))]
            let _ = shutdown_rx;
            std::future::pending::<Result<()>>().await
        };
        tokio::pin!(http);
        let mut http_done = false;
        let mut collector_done = false;
        let mut result: Result<()> = tokio::select! {
            _ = terminate.recv() => Ok(()),
            _ = interrupt.recv() => Ok(()),
            done = &mut completion => {
                collector_done = true;
                match done {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(error.into()),
                    Err(_) => Err("Collector stopped unexpectedly".into()),
                }
            },
            done = &mut http => { http_done = true; done },
        };
        stop.store(true, Ordering::Release);
        let _ = shutdown.send(());
        if !collector_done {
            if let Ok(Err(error)) = completion.await {
                if result.is_ok() {
                    result = Err(error.into());
                }
            }
        }
        if worker.join().is_err() && result.is_ok() {
            result = Err("Collector panicked".into());
        }
        if !http_done && !no_web {
            if let Ok(Err(error)) = tokio::time::timeout(Duration::from_secs(3), &mut http).await {
                if result.is_ok() {
                    result = Err(error);
                }
            }
        }
        result
    })
}
fn collect(
    collector: Collector,
    mut connector: impl transport::Connector,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    use std::sync::{Arc, Mutex};
    let Collector {
        archive,
        listener,
        _cleanup,
    } = collector;
    let mut initial_stream = connector.connect(Duration::from_millis(200)).ok();
    let connector = Arc::new(Mutex::new(connector));
    let history_status = Arc::new(Mutex::new(json!({"state":"connecting"})));
    let archive_connector = connector.clone();
    let archive_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_stop = archive_stop.clone();
    let archive_status = history_status.clone();
    let (diagnostics, diagnostic_writer) = history_sync::diagnostics();
    let archive_worker = std::thread::Builder::new()
        .name("history-archive".into())
        .spawn(move || {
            history_sync::run(
                archive,
                archive_connector,
                worker_stop,
                archive_status,
                diagnostic_writer,
            )
            .map_err(|e| e.to_string())
        })?;
    let result: Result<()> = (|| {
        let mut guest: Option<Box<dyn transport::Stream>> = None;
        let mut client = Client::default();
        let mut status_sent = false;
        let mut pending: Option<Pending> = None;
        let mut busy: Vec<Pending> = Vec::new();
        let mut next_connect = Instant::now();
        let mut last_received_at: Option<String> = None;
        while !stop.load(std::sync::atomic::Ordering::Acquire) {
            if guest.is_none() && Instant::now() >= next_connect {
                next_connect = Instant::now() + Duration::from_millis(200);
                if let Some(stream) = initial_stream.take().or_else(|| {
                    connector
                        .lock()
                        .unwrap()
                        .connect(Duration::from_millis(200))
                        .ok()
                }) {
                    client.connect();
                    status_sent = false;
                    guest = Some(stream);
                    diagnostics.append(
                        json!({"major":1,"type":"host.connection","connected":true}),
                        None,
                    );
                }
            }
            if guest.is_some() && !status_sent && client.negotiation().failure.is_none() {
                if client.peer().endpoint().is_some() && client.peer().discovery().is_some() {
                    status_sent = true;
                    if client.require(v1::Method::StatusGet).is_ok() {
                        client
                            .request(v1::Method::StatusGet, json!({}))
                            .map_err(|error| error.message)?;
                    }
                } else if client.negotiation().since.elapsed() > Duration::from_secs(5) {
                    client.fail(v1::RpcError::new(-32001, "Handshake timed out"));
                }
            }
            for _ in 0..16 {
                match listener.accept() {
                    Ok((client, _)) => {
                        client.set_nonblocking(true)?;
                        let p = Pending::new(client);
                        if pending.is_none() {
                            pending = Some(p)
                        } else if busy.len() < 32 {
                            busy.push(p)
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
            let endpoint = client.peer().endpoint();
            let connection = json!({"transport":"VirtIO socket","connected":guest.is_some(),"last_received_at":last_received_at,
            "history":history_status.lock().unwrap().clone(),
            "archive_writes":diagnostics.status.lock().unwrap().clone(),
            "protocol_version":endpoint.map(|v|&v.protocol_version),"software_version":endpoint.map(|v|&v.software_version),"role":endpoint.map(|v|v.role),
            "discovered_methods":client.peer().discovery().map(|d|d.methods.iter().map(|m|m.name.clone()).collect::<Vec<_>>()),"ready":client.require(v1::Method::Discover).is_ok()});
            for p in &mut busy {
                if p.response.is_none() {
                    if let Some((packet, local)) = read_control(p) {
                        control_packet(p, packet, local, true, &connection, &mut client)?;
                    }
                }
            }
            busy.retain_mut(|p| !write_response(p) && p.since.elapsed() < Duration::from_secs(7));
            if let Some(p) = pending.as_mut() {
                if p.id.is_none() && p.response.is_none() {
                    if let Some((packet, local)) = read_control(p) {
                        control_packet(p, packet, local, false, &connection, &mut client)?;
                    }
                }
            }
            let mut disconnected = false;
            if let Some(stream) = guest.as_mut() {
                if client.write(stream.as_mut()).is_err() {
                    disconnected = true;
                }
                let mut bytes = [0; 65536];
                match stream.read(&mut bytes) {
                    Ok(0) => disconnected = true,
                    Ok(n) => {
                        last_received_at = Some(store::now());
                        guest_packet(&bytes[..n], &diagnostics, &mut client, pending.as_mut())?;
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => (),
                    Err(_) => disconnected = true,
                }
            }
            if disconnected {
                guest = None;
                client.disconnect();
                diagnostics.append(
                    json!({"major":1,"type":"host.connection","connected":false}),
                    None,
                );
                if let Some(p) = pending.as_mut() {
                    if p.response.is_none() && p.id.is_some() {
                        p.finish(Err(outcome(
                            "Connection interrupted; query status after reconnection",
                            "outcome_unknown",
                        )));
                    }
                }
            }
            let mut done = false;
            if let Some(p) = pending.as_mut() {
                let timeout = if p.method == Some(v1::Method::SystemInfoGet) {
                    45
                } else {
                    5
                };
                if p.response.is_none() && p.since.elapsed() > Duration::from_secs(timeout) {
                    if p.slot.is_some() {
                        p.finish(Err(outcome(
                        "Request timed out; query status to confirm the effective configuration",
                        "outcome_unknown",
                    )));
                    } else {
                        p.response = Some(encode_value(&error_value(
                            v1::RpcId::Null,
                            v1::RpcError::new(-32600, "Incomplete request"),
                        )));
                    }
                }
                done = write_response(p) || p.since.elapsed() > Duration::from_secs(timeout + 2);
            }
            if done {
                if let Some(id) = pending.as_ref().and_then(|p| p.id.as_deref()) {
                    client.cancel(id);
                }
                pending = None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Dropping control clients completes waiting HTTP and CLI requests promptly.
        Ok(())
    })();
    stop.store(true, std::sync::atomic::Ordering::Release);
    archive_stop.store(true, std::sync::atomic::Ordering::Release);
    let archive_result = archive_worker
        .join()
        .map_err(|_| "History worker panicked")?;
    result?;
    archive_result.map_err(Into::into)
}
