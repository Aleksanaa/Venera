mod store;
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
use venera_debug_protocol::{v1, Filter, DEFAULT_MAX_LINE_BYTES};

#[derive(Parser)]
#[command(about = "Venera debug collection and history")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    #[cfg(feature = "web")]
    Web {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        assets: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: std::net::SocketAddr,
        #[arg(long)]
        control: Option<PathBuf>,
    },
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
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    match Cli::parse().command {
        #[cfg(feature = "web")]
        Command::Web {
            database,
            assets,
            listen,
            control,
        } => web::run(database, assets, listen, control),
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
fn host_discovery() -> Value {
    serde_json::from_str(include_str!("../../protocol/v1/host.openrpc.json"))
        .expect("host discovery artifact")
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
fn valid_result(method: v1::Method, value: &Value) -> v1::RpcResult<()> {
    let valid = match method {
        v1::Method::StatusGet | v1::Method::FilterSet | v1::Method::FilterReset => {
            serde_json::from_value::<v1::Status>(value.clone()).is_ok()
        }
        v1::Method::SystemInfoGet => {
            serde_json::from_value::<v1::SystemInfo>(value.clone()).is_ok()
        }
        v1::Method::Discover => serde_json::from_value::<v1::Discovery>(value.clone()).is_ok(),
        v1::Method::EndpointInfo => {
            serde_json::from_value::<v1::EndpointInfo>(value.clone()).is_ok()
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(v1::RpcError::new(-32004, "Invalid guest result"))
    }
}
struct Negotiation {
    peer: v1::PeerProtocol,
    calls: Vec<(String, v1::Method)>,
    failure: Option<v1::RpcError>,
    since: Instant,
    status_sent: bool,
}
impl Negotiation {
    fn new() -> Self {
        Self {
            peer: Default::default(),
            calls: Vec::new(),
            failure: None,
            since: Instant::now(),
            status_sent: false,
        }
    }
    fn require(&self, method: v1::Method) -> v1::RpcResult<()> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        self.peer.require(method)?;
        if self
            .peer
            .endpoint()
            .is_some_and(|info| info.role != v1::EndpointRole::Guest)
        {
            return Err(v1::RpcError::new(-32003, "incompatible_protocol"));
        }
        Ok(())
    }
    fn accept(&mut self, method: v1::Method, result: v1::RpcResult<Value>) {
        let accepted = result.and_then(|value| {
            valid_result(method, &value)?;
            match method {
                v1::Method::EndpointInfo => self
                    .peer
                    .accept_endpoint(serde_json::from_value(value).unwrap()),
                v1::Method::Discover => self
                    .peer
                    .accept_discovery(serde_json::from_value(value).unwrap()),
                _ => Ok(()),
            }
        });
        if let Err(error) = accepted {
            self.failure = Some(error);
        }
        if self
            .peer
            .endpoint()
            .is_some_and(|info| info.role != v1::EndpointRole::Guest)
        {
            self.failure = Some(v1::RpcError::new(-32003, "incompatible_protocol"));
        }
    }
}
fn control_packet(
    p: &mut Pending,
    packet: v1::RpcPacket,
    local: bool,
    busy: bool,
    connection: &Value,
    negotiation: &Negotiation,
    serial: &mut u64,
    run_id: &str,
    output: &mut Vec<u8>,
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
                negotiation.require(method).err().map(Err)
            };
            if let Some(result) = immediate {
                let _ = p.batch.as_mut().unwrap().complete(slot, result);
                continue;
            }
            *serial += 1;
            let id = format!("{run_id}-{serial}");
            let params = match request.incoming {
                v1::Incoming::FilterSet { params, .. } => serde_json::to_value(params)?,
                _ => json!({}),
            };
            output.extend(rpc_request(&id, method.name(), params)?);
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
    text: &str,
    store: &Store,
    negotiation: &mut Negotiation,
    mut pending: Option<&mut Pending>,
    output: &mut Vec<u8>,
) -> Result<()> {
    let original: Option<Value> = serde_json::from_str(text).ok();
    let array = matches!(original, Some(Value::Array(_)));
    let sources: Vec<String> = match &original {
        Some(Value::Array(values)) if !values.is_empty() => {
            values.iter().map(Value::to_string).collect()
        }
        _ => vec![text.to_owned()],
    };
    let mut accepted = Vec::new();
    for source in &sources {
        let value: Option<Value> = serde_json::from_str(source).ok();
        let packet = v1::RpcPacket::decode(source.as_bytes(), v1::Receiver::Host);
        let response_like = value.as_ref().is_some_and(|v| {
            v.get("method").is_none() && (v.get("result").is_some() || v.get("error").is_some())
        });
        if response_like
            && packet
                .items
                .iter()
                .any(|item| matches!(item, v1::PacketItem::Rejected(_)))
        {
            store.append(
                &json!({"major":1,"type":"host.protocol_error","error":"Invalid guest response"}),
            )?;
            continue;
        }
        for item in &packet.items {
            if let v1::PacketItem::Request(request) = item {
                if let Some(projected) = project(&request.incoming) {
                    store.append_with_raw(&projected, Some(source))?;
                }
            }
        }
        accepted.push(source.as_str());
    }
    if accepted.is_empty() {
        return Ok(());
    }
    let filtered;
    let bytes = if array && matches!(original,Some(Value::Array(ref values)) if !values.is_empty())
    {
        filtered = format!("[{}]", accepted.join(","));
        filtered.as_bytes()
    } else {
        text.as_bytes()
    };
    let mut packet = v1::RpcPacket::decode(bytes, v1::Receiver::Host);
    for item in packet.items {
        match item {
            v1::PacketItem::Request(request) => {
                let result = match request.incoming {
                    v1::Incoming::EndpointInfo { .. } => Ok(serde_json::to_value(host_endpoint())?),
                    v1::Incoming::Discover { .. } => Ok(host_discovery()),
                    _ => Ok(Value::Null),
                };
                if let Some(slot) = request.correlation {
                    let _ = packet.batch.complete(slot, result);
                }
            }
            v1::PacketItem::Response(response) => {
                let (id, result) = match response {
                    v1::RpcResponse::Success { id, result, .. } => (id, Ok(result)),
                    v1::RpcResponse::Error { id, error, .. } => (id, Err(error)),
                };
                if let Some(index) = negotiation
                    .calls
                    .iter()
                    .position(|(known, _)| id == v1::RpcId::String(known.clone()))
                {
                    let (_, method) = negotiation.calls.remove(index);
                    negotiation.accept(method, result);
                } else if let Some(p) = pending.as_deref_mut() {
                    if p.id
                        .as_ref()
                        .is_some_and(|known| id == v1::RpcId::String(known.clone()))
                        && p.response.is_none()
                    {
                        let method = p.method.unwrap();
                        let checked = result.and_then(|value| {
                            valid_result(method, &value)?;
                            Ok(value)
                        });
                        if matches!(method, v1::Method::EndpointInfo | v1::Method::Discover) {
                            negotiation.accept(method, checked.clone());
                        }
                        p.finish(checked);
                    }
                }
            }
            v1::PacketItem::Rejected(error) => {
                store.append(&json!({"major":1,"type":"host.protocol_error","error":error}))?
            }
        }
    }
    if let Some(response) = packet.batch.take_response() {
        output.extend(encode_value(&response));
    }
    Ok(())
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
                Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
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
    let mut framer = v1::JsonlFramer::default();
    let mut negotiation = Negotiation::new();
    let mut pending: Option<Pending> = None;
    let mut busy: Vec<Pending> = Vec::new();
    let mut output = Vec::new();
    let mut offset = 0;
    let mut next_connect = Instant::now();
    let mut serial = 0u64;
    let run_id = store::now();
    let mut last_received_at: Option<String> = None;
    store.append(&json!({"major":1,"type":"host.started"}))?;
    loop {
        if guest.is_none() && Instant::now() >= next_connect {
            next_connect = Instant::now() + Duration::from_millis(200);
            if let Ok(stream) = UnixStream::connect(&socket) {
                stream.set_nonblocking(true)?;
                framer.clear();
                negotiation = Negotiation::new();
                negotiation.peer.connect();
                output = vec![b'\n'];
                offset = 0;
                for method in [v1::Method::EndpointInfo, v1::Method::Discover] {
                    serial += 1;
                    let id = format!("{run_id}-{serial}");
                    output.extend(rpc_request(&id, method.name(), json!({}))?);
                    negotiation.calls.push((id, method));
                }
                guest = Some(stream);
                store.append(&json!({"major":1,"type":"host.connection","connected":true}))?;
            }
        }
        if guest.is_some() && !negotiation.status_sent && negotiation.failure.is_none() {
            if negotiation.peer.endpoint().is_some() && negotiation.peer.discovery().is_some() {
                negotiation.status_sent = true;
                if negotiation.require(v1::Method::StatusGet).is_ok() {
                    serial += 1;
                    let id = format!("{run_id}-{serial}");
                    output.extend(rpc_request(&id, "status.get", json!({}))?);
                    negotiation.calls.push((id, v1::Method::StatusGet));
                }
            } else if negotiation.since.elapsed() > Duration::from_secs(5) {
                negotiation.failure = Some(v1::RpcError::new(-32001, "Handshake timed out"));
                negotiation.calls.clear();
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
        let endpoint = negotiation.peer.endpoint();
        let connection = json!({"transport":"VirtIO Serial","channel":"org.venera.debug.0","socket_path":socket,"connected":guest.is_some(),"last_received_at":last_received_at,
            "protocol_version":endpoint.map(|v|&v.protocol_version),"software_version":endpoint.map(|v|&v.software_version),"role":endpoint.map(|v|v.role),
            "discovered_methods":negotiation.peer.discovery().map(|d|d.methods.iter().map(|m|m.name.clone()).collect::<Vec<_>>()),"ready":negotiation.require(v1::Method::Discover).is_ok()});
        for p in &mut busy {
            if p.response.is_none() {
                if let Some((packet, local)) = read_control(p) {
                    control_packet(
                        p,
                        packet,
                        local,
                        true,
                        &connection,
                        &negotiation,
                        &mut serial,
                        &run_id,
                        &mut output,
                    )?;
                }
            }
        }
        busy.retain_mut(|p| !write_response(p) && p.since.elapsed() < Duration::from_secs(7));
        if let Some(p) = pending.as_mut() {
            if p.id.is_none() && p.response.is_none() {
                if let Some((packet, local)) = read_control(p) {
                    control_packet(
                        p,
                        packet,
                        local,
                        false,
                        &connection,
                        &negotiation,
                        &mut serial,
                        &run_id,
                        &mut output,
                    )?;
                }
            }
        }
        let mut disconnected = false;
        if let Some(stream) = guest.as_mut() {
            if offset < output.len() {
                match stream.write(&output[offset..]) {
                    Ok(0) => disconnected = true,
                    Ok(n) => offset += n,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => (),
                    Err(_) => disconnected = true,
                }
            }
            if offset == output.len() {
                output.clear();
                offset = 0;
            }
            let mut bytes = [0; 65536];
            match stream.read(&mut bytes) {
                Ok(0) => disconnected = true,
                Ok(n) => {
                    last_received_at = Some(store::now());
                    for frame in framer.push(&bytes[..n]) {
                        match frame {
                            Err(error) => {
                                store.append(
                                    &json!({"major":1,"type":"host.protocol_error","error":error}),
                                )?;
                                output.extend(encode_value(&error_value(v1::RpcId::Null, error)));
                            }
                            Ok(frame) => {
                                guest_packet(
                                    frame.text(),
                                    &store,
                                    &mut negotiation,
                                    pending.as_mut(),
                                    &mut output,
                                )?;
                            }
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => (),
                Err(_) => disconnected = true,
            }
        }
        if output.len().saturating_sub(offset) > 2 * v1::MAX_FRAME_BYTES {
            disconnected = true;
        }
        if disconnected {
            guest = None;
            framer.clear();
            negotiation.peer.disconnect();
            negotiation.calls.clear();
            negotiation.failure = None;
            output.clear();
            offset = 0;
            store.append(&json!({"major":1,"type":"host.connection","connected":false}))?;
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
            pending = None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
