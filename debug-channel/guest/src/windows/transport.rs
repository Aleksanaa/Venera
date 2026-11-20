use super::*;
use crate::{
    connection::{self, Connection},
    state::wire,
};
use std::time::{Duration, Instant};
#[path = "transport_write.rs"]
mod transport_write;
use transport_write::{send_error, PendingWrite, WriteStep};
use windows_sys::Win32::Networking::WinSock::accept;
use windows_sys::Win32::System::Services::*;
use windows_sys::Win32::{
    Networking::WinSock::*, Storage::FileSystem::*, System::IO::DeviceIoControl,
};

const PORT: u32 = 49152;
const IOCTL_GET_AF: u32 = 0x0801300c;

#[repr(C)]
struct VmAddress {
    family: u16,
    reserved: u16,
    port: u32,
    cid: u32,
}

pub fn health(state: &Shared, key: &str, value: serde_json::Value) {
    let mut state = state.lock().unwrap();
    if state.status.health[key] != value {
        state.status.health[key] = value;
        state.notify();
    }
}

struct Socket(SOCKET);
impl Drop for Socket {
    fn drop(&mut self) {
        unsafe {
            closesocket(self.0);
        }
    }
}
fn socket_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}
fn would_block() -> bool {
    unsafe { WSAGetLastError() == WSAEWOULDBLOCK }
}
unsafe fn probe_driver(state: &Shared) {
    let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
    if manager.is_null() {
        health(
            state,
            "driver",
            serde_json::json!({"error":error().to_string()}),
        );
        return;
    }
    let service = OpenServiceW(manager, wide("VirtioSocket").as_ptr(), SERVICE_QUERY_STATUS);
    let driver = if service.is_null() {
        let issue = error();
        if issue.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
            serde_json::json!("missing")
        } else {
            serde_json::json!({"error":issue.to_string()})
        }
    } else {
        serde_json::json!("installed")
    };
    health(state, "driver", driver);
    if !service.is_null() {
        CloseServiceHandle(service);
    }
    CloseServiceHandle(manager);
}
unsafe fn nonblocking(socket: SOCKET) -> io::Result<()> {
    let mut enabled = 1;
    if ioctlsocket(socket, FIONBIO, &mut enabled) == SOCKET_ERROR {
        return Err(socket_error());
    }
    Ok(())
}

// ViosockGetAF's device query, as defined by the vendor's vio_sockets.h.
unsafe fn address_family() -> io::Result<u16> {
    let device = Handle(CreateFileW(
        wide("\\??\\Viosock").as_ptr(),
        GENERIC_READ,
        FILE_SHARE_READ,
        std::ptr::null(),
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        std::ptr::null_mut(),
    ));
    if device.0 == INVALID_HANDLE_VALUE {
        return Err(error());
    }
    let mut family: u32 = 0;
    let mut returned = 0;
    if DeviceIoControl(
        device.0,
        IOCTL_GET_AF,
        std::ptr::null(),
        0,
        (&mut family as *mut u32).cast(),
        4,
        &mut returned,
        std::ptr::null_mut(),
    ) == 0
    {
        return Err(error());
    }
    if returned != 4 || family == 0 || family > u16::MAX as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VirtIO socket address family is unavailable",
        ));
    }
    Ok(family as u16)
}

unsafe fn listener(state: &Shared) -> io::Result<Socket> {
    probe_driver(state);
    let family = match address_family() {
        Ok(family) => family,
        Err(issue) => {
            health(state, "controller", serde_json::json!("waiting"));
            return Err(issue);
        }
    };
    health(
        state,
        "controller",
        serde_json::json!({"state":"running","problem":0}),
    );
    let socket = Socket(WSASocketW(
        family as i32,
        SOCK_STREAM,
        0,
        std::ptr::null(),
        0,
        0,
    ));
    if socket.0 == INVALID_SOCKET {
        return Err(socket_error());
    }
    nonblocking(socket.0)?;
    let address = VmAddress {
        family,
        reserved: 0,
        port: PORT,
        cid: u32::MAX,
    };
    if bind(
        socket.0,
        (&address as *const VmAddress).cast(),
        std::mem::size_of::<VmAddress>() as i32,
    ) == SOCKET_ERROR
        || listen(socket.0, SOMAXCONN as i32) == SOCKET_ERROR
    {
        return Err(socket_error());
    }
    health(state, "port", serde_json::json!("present"));
    Ok(socket)
}

pub fn serve(state: Shared) {
    unsafe {
        let mut data = std::mem::zeroed();
        let result = WSAStartup(0x0202, &mut data);
        if result != 0 {
            health(&state, "host", serde_json::json!("waiting"));
            health(
                &state,
                "port",
                serde_json::json!({"error":io::Error::from_raw_os_error(result).to_string()}),
            );
            return;
        }
        while !stopped() {
            if let Err(issue) = accept_peers(&state) {
                health(
                    &state,
                    "port",
                    serde_json::json!({"state":"retrying","error":issue.to_string()}),
                );
            }
            for _ in 0..10 {
                if stopped() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        health(&state, "port", serde_json::json!("waiting"));
        health(&state, "host", serde_json::json!("waiting"));
        WSACleanup();
    }
}

unsafe fn accept_peers(state: &Shared) -> io::Result<()> {
    let listener = listener(state)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let mut peers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let result = (|| {
        while !stopped() {
            let socket = Socket(accept(
                listener.0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ));
            if socket.0 == INVALID_SOCKET {
                if !would_block() {
                    return Err(socket_error());
                }
                std::thread::sleep(Duration::from_millis(20));
            } else {
                nonblocking(socket.0)?;
                let peer = state.lock().unwrap().add_peer();
                let shared = state.clone();
                let stop = cancel.clone();
                match std::thread::Builder::new()
                    .name(format!("vsock-peer-{peer}"))
                    .spawn(move || {
                        let _ = serve_peer(socket, &shared, peer, &stop);
                        let mut state = shared.lock().unwrap();
                        state.remove_peer(peer);
                        state.notify();
                    }) {
                    Ok(thread) => peers.push(thread),
                    Err(issue) => {
                        state.lock().unwrap().remove_peer(peer);
                        return Err(issue);
                    }
                }
            }
            let mut index = 0;
            while index < peers.len() {
                if peers[index].is_finished() {
                    let _ = peers.swap_remove(index).join();
                } else {
                    index += 1;
                }
            }
        }
        Ok(())
    })();
    cancel.store(true, Ordering::Release);
    drop(listener);
    health(state, "port", serde_json::json!("waiting"));
    for peer in peers {
        let _ = peer.join();
    }
    health(state, "host", serde_json::json!("waiting"));
    result
}

unsafe fn serve_peer(
    socket: Socket,
    state: &Shared,
    peer: u64,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut connection = Connection::new(peer);
    for request in connection.connect() {
        queue_reply(state, peer, &request)?;
    }
    let mut pending: Option<(Option<u64>, PendingWrite)> = None;
    health(state, "host", serde_json::json!("connected"));
    state.lock().unwrap().notify();
    let mut buffer = [0u8; 16384];
    while !stopped() && !cancel.load(Ordering::Acquire) {
        if state.lock().unwrap().peer_closed(peer) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Peer queue is full",
            ));
        }
        if state.lock().unwrap().peer_control_room(peer) {
            let count = recv(socket.0, buffer.as_mut_ptr(), buffer.len() as i32, 0);
            if count == 0 {
                return Ok(());
            }
            if count == SOCKET_ERROR {
                if !would_block() {
                    return Err(socket_error());
                }
            } else {
                let generation = state.lock().unwrap().history_generation();
                let received = connection.receive_with(
                    &buffer[..count as usize],
                    |message| {
                        let expires = match &message {
                            Message::FilterSet {
                                duration_ms: Some(ms),
                                ..
                            } => Some(timestamp_after(*ms)),
                            _ => None,
                        };
                        state.lock().unwrap().control(message, expires)
                    },
                    |request, local| history_request(state, request, local),
                );
                state.lock().unwrap().parse_errors += received.parse_errors;
                for response in received.output {
                    if connection::history_response(&response) {
                        let bytes = connection::line(&response)?;
                        if !state
                            .lock()
                            .unwrap()
                            .queue_reply(peer, Some(generation), bytes)
                        {
                            return Err(io::ErrorKind::OutOfMemory.into());
                        }
                    } else {
                        queue_reply(state, peer, &response)?;
                    }
                }
                for request in received.reports {
                    if let Some(response) = system_info::request(state.clone(), peer, request) {
                        if let Some(response) = connection.report(&response) {
                            queue_reply(state, peer, &response)?;
                        }
                    }
                }
            }
        }
        if pending.is_none() {
            let reply = state.lock().unwrap().next_reply(peer);
            if let Some((generation, bytes)) = reply {
                pending = Some((generation, PendingWrite::new(bytes, Instant::now())));
            } else {
                let next = state.lock().unwrap().next_line(peer);
                if let Some((generation, bytes)) = next {
                    let envelope = venera_debug_protocol::decode(&bytes)
                        .map_err(|_| io::ErrorKind::InvalidData)?;
                    let bytes = if matches!(envelope.body, Message::SystemInfoResult { .. }) {
                        connection
                            .report(&envelope)
                            .map(|value| connection::line(&value))
                            .transpose()?
                    } else {
                        Some(wire::encode(&envelope)?)
                    };
                    if let Some(bytes) = bytes {
                        pending = Some((generation, PendingWrite::new(bytes, Instant::now())));
                    }
                }
            }
        }
        if let Some((generation, write)) = pending.as_mut() {
            let allowed = state.lock().unwrap().write_allowed(*generation);
            match write.write(Instant::now(), allowed, |bytes| {
                let count = send(socket.0, bytes.as_ptr(), bytes.len() as i32, 0);
                if count == SOCKET_ERROR {
                    Err(send_error(WSAGetLastError()))
                } else {
                    Ok(count as usize)
                }
            })? {
                WriteStep::Complete => {
                    pending = None;
                    continue;
                }
                WriteStep::Progress => continue,
                WriteStep::Blocked => (),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn queue_reply(state: &Shared, peer: u64, value: &serde_json::Value) -> io::Result<()> {
    let bytes = connection::line(value)?;
    if state.lock().unwrap().queue_peer_reply(peer, bytes) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "Peer response queue is full",
        ))
    }
}
