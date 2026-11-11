use super::*;
use crate::serial_connection::{self, Connection};
use crate::state::serial;
use venera_debug_protocol::{Envelope, Message};
use windows_sys::{
    core::GUID,
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Storage::FileSystem::*,
        System::{Services::*, IO::*},
    },
};

const PORT: &str = "org.venera.debug.0";
const CONTROLLER: GUID = GUID::from_u128(0xf55f7844_6a0c_11d2_b841_00c04fad5171);
const IOCTL_GET_INFORMATION: u32 = 0x00222002;

pub fn health(state: &Shared, key: &str, value: serde_json::Value) {
    let mut state = state.lock().unwrap();
    if state.status.health[key] != value {
        state.status.health[key] = value;
        state.notify();
    }
}
fn probe(state: &Shared) {
    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if !manager.is_null() {
            let service =
                OpenServiceW(manager, wide("VirtioSerial").as_ptr(), SERVICE_QUERY_STATUS);
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
        let devices = SetupDiGetClassDevsW(
            &CONTROLLER,
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if devices == -1 {
            health(
                state,
                "controller",
                serde_json::json!({"error":error().to_string()}),
            );
            return;
        }
        let mut interface: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
        interface.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        let present =
            SetupDiEnumDeviceInterfaces(devices, std::ptr::null(), &CONTROLLER, 0, &mut interface)
                != 0;
        let mut controller = serde_json::json!("waiting");
        if present {
            let mut required = 0;
            SetupDiGetDeviceInterfaceDetailW(
                devices,
                &interface,
                std::ptr::null_mut(),
                0,
                &mut required,
                std::ptr::null_mut(),
            );
            let mut storage = vec![0u64; (required as usize).div_ceil(8)];
            if required < std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 {
                SetupDiDestroyDeviceInfoList(devices);
                health(state, "controller", serde_json::json!("probing"));
                return;
            }
            let detail = storage.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut device: SP_DEVINFO_DATA = std::mem::zeroed();
            device.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(
                devices,
                &interface,
                detail,
                required,
                &mut required,
                &mut device,
            ) != 0
            {
                let mut flags = 0;
                let mut problem = 0;
                if CM_Get_DevNode_Status(&mut flags, &mut problem, device.DevInst, 0) == CR_SUCCESS
                {
                    controller = serde_json::json!({"state":if flags & DN_STARTED != 0 {"running"} else {"present"},"problem":problem});
                }
            }
        }
        health(state, "controller", controller);
        SetupDiDestroyDeviceInfoList(devices);
    }
}

/// Each operation owns its buffer and OVERLAPPED until completion is reaped.
unsafe fn cancel_and_wait(handle: HANDLE, ov: &OVERLAPPED) {
    CancelIoEx(handle, ov);
    let mut bytes = 0;
    GetOverlappedResult(handle, ov, &mut bytes, 1);
}
unsafe fn complete(
    handle: HANDLE,
    ov: &mut OVERLAPPED,
    state: &Shared,
    generation: Option<u64>,
) -> io::Result<u32> {
    let started = std::time::Instant::now();
    loop {
        let wait = WaitForSingleObject(ov.hEvent, 100);
        let shared = state.lock().unwrap();
        if !shared.write_allowed(generation) {
            drop(shared);
            cancel_and_wait(handle, ov);
            return Err(io::ErrorKind::Interrupted.into());
        }
        if wait == WAIT_OBJECT_0 {
            let mut bytes = 0;
            return if GetOverlappedResult(handle, ov, &mut bytes, 0) != 0 {
                Ok(bytes)
            } else {
                Err(error())
            };
        }
        drop(shared);
        if wait != WAIT_TIMEOUT
            || stopped()
            || started.elapsed() >= std::time::Duration::from_secs(2)
        {
            cancel_and_wait(handle, ov);
            return Err(io::ErrorKind::Interrupted.into());
        }
    }
}
unsafe fn info(handle: HANDLE, state: &Shared) -> io::Result<bool> {
    let event = Handle(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()));
    if event.0.is_null() {
        return Err(error());
    }
    let mut ov: OVERLAPPED = std::mem::zeroed();
    ov.hEvent = event.0;
    let mut data = vec![0u8; 4096];
    let mut transferred = 0;
    if DeviceIoControl(
        handle,
        IOCTL_GET_INFORMATION,
        std::ptr::null(),
        0,
        data.as_mut_ptr().cast(),
        data.len() as u32,
        &mut transferred,
        &mut ov,
    ) == 0
    {
        if GetLastError() != ERROR_IO_PENDING {
            return Err(error());
        }
        transferred = complete(handle, &mut ov, state, None)?;
    }
    if transferred < 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VirtIO port information is incomplete",
        ));
    }
    let name = &data[7..transferred as usize];
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    if &name[..end] != PORT.as_bytes() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VirtIO debug port name mismatch",
        ));
    }
    Ok(data[5] != 0)
}
unsafe fn write_line(
    handle: HANDLE,
    bytes: &[u8],
    state: &Shared,
    generation: Option<u64>,
) -> io::Result<()> {
    let event = Handle(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()));
    if event.0.is_null() {
        return Err(error());
    }
    let mut offset = 0;
    while offset < bytes.len() {
        let shared = state.lock().unwrap();
        if !shared.write_allowed(generation) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        ResetEvent(event.0);
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.hEvent = event.0;
        let mut count = 0;
        let submitted = WriteFile(
            handle,
            bytes[offset..].as_ptr(),
            (bytes.len() - offset) as u32,
            &mut count,
            &mut ov,
        );
        let issue = if submitted == 0 { Some(error()) } else { None };
        drop(shared);
        if let Some(issue) = issue {
            if issue.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(issue);
            }
            count = complete(handle, &mut ov, state, generation)?;
        }
        if !state.lock().unwrap().write_allowed(generation) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if count == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        offset += count as usize;
    }
    Ok(())
}
pub fn serve(state: Shared) {
    let mut serial = Connection::default();
    while !stopped() {
        probe(&state);
        let result = unsafe { connection(&state, &mut serial) };
        serial.disconnect();
        {
            let mut shared = state.lock().unwrap();
            shared.controls.retain(|bytes| {
                matches!(
                    venera_debug_protocol::decode(bytes),
                    Ok(Envelope {
                        body: Message::Gap { .. },
                        ..
                    })
                )
            });
            if shared.status.health["host"] == "connected" {
                let message = Envelope::new(Message::Gap {
                    stream_id: shared.stream_id.clone(),
                    reason: "transport_interrupted".into(),
                    count: None,
                    first_seq: None,
                    last_seq: None,
                });
                shared.broadcast(venera_debug_protocol::encode(&message).unwrap());
                shared.queue_control(&message);
            }
        }
        if let Err(e) = result {
            health(
                &state,
                "port",
                serde_json::json!({"state":"retrying","error":e.to_string()}),
            );
        }
        health(&state, "host", serde_json::json!("waiting"));
        for _ in 0..5 {
            if stopped() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}
unsafe fn connection(state: &Shared, serial: &mut Connection) -> io::Result<()> {
    let port = Handle(CreateFileW(
        wide(&format!("\\\\.\\Global\\{PORT}")).as_ptr(),
        GENERIC_READ | GENERIC_WRITE,
        0,
        std::ptr::null(),
        OPEN_EXISTING,
        FILE_FLAG_OVERLAPPED,
        std::ptr::null_mut(),
    ));
    if port.0 == INVALID_HANDLE_VALUE {
        return Err(error());
    }
    health(state, "port", serde_json::json!("present"));
    let read_event = Handle(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()));
    if read_event.0.is_null() {
        return Err(error());
    }
    let mut ov: OVERLAPPED = std::mem::zeroed();
    ov.hEvent = read_event.0;
    let mut buffer = [0u8; 16384];
    let mut pending = false;
    let mut connected = false;
    let result = (|| -> io::Result<()> {
        while !stopped() {
            let host = info(port.0, state)?;
            if host != connected {
                connected = host;
                serial.disconnect();
                health(
                    state,
                    "host",
                    serde_json::json!(if host { "connected" } else { "waiting" }),
                );
                if pending {
                    CancelIoEx(port.0, &ov);
                    let mut bytes = 0;
                    GetOverlappedResult(port.0, &ov, &mut bytes, 1);
                    pending = false;
                }
                if host {
                    write_line(port.0, b"\n", state, None)?;
                    for request in serial.connect() {
                        let bytes = serial_connection::line(&request)?;
                        write_line(port.0, &bytes, state, None)?;
                    }
                    state.lock().unwrap().notify();
                }
            }
            if !host {
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
            let mut received = None;
            if pending {
                if WaitForSingleObject(read_event.0, 0) == WAIT_OBJECT_0 {
                    let mut count = 0;
                    if GetOverlappedResult(port.0, &ov, &mut count, 0) == 0 {
                        return Err(error());
                    }
                    pending = false;
                    received = Some(count);
                }
            } else {
                ResetEvent(read_event.0);
                ov = std::mem::zeroed();
                ov.hEvent = read_event.0;
                let mut count = 0;
                if ReadFile(
                    port.0,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    &mut count,
                    &mut ov,
                ) != 0
                {
                    received = Some(count);
                } else if GetLastError() == ERROR_IO_PENDING {
                    pending = true;
                } else {
                    return Err(error());
                }
            }
            if let Some(count) = received {
                if count == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                let received = serial.receive(&buffer[..count as usize], |message| {
                    let expires = match &message {
                        Message::FilterSet {
                            duration_ms: Some(ms),
                            ..
                        } => Some(timestamp_after(*ms)),
                        _ => None,
                    };
                    let mut shared = state.lock().unwrap();
                    if shared.controls.len() >= 60 {
                        let (request_id, operation) = match &message {
                            Message::StatusGet { request_id } => (request_id.clone(), "status.get"),
                            Message::FilterSet { request_id, .. } => {
                                (request_id.clone(), "filter.set")
                            }
                            Message::FilterReset { request_id } => {
                                (request_id.clone(), "filter.reset")
                            }
                            _ => return None,
                        };
                        let mut response = shared.result(request_id, operation);
                        if let Message::RequestResult { result } = &mut response.body {
                            result.ok = false;
                            result.error = Some(venera_debug_protocol::RequestError {
                                code: "busy".into(),
                                message: "Control queue is busy; retry the request".into(),
                            });
                        }
                        Some(response)
                    } else {
                        shared.control(message, expires)
                    }
                });
                if received.parse_errors != 0 {
                    state.lock().unwrap().parse_errors += received.parse_errors;
                }
                for response in received.output {
                    write_line(port.0, &serial_connection::line(&response)?, state, None)?;
                }
                for request_id in received.reports {
                    if let Some(response) = system_info::request(state.clone(), request_id) {
                        if let Some(value) = serial.report(&response) {
                            write_line(port.0, &serial_connection::line(&value)?, state, None)?;
                        }
                    }
                }
            }
            let next = state.lock().unwrap().next_line();
            if let Some((generation, bytes)) = next {
                let envelope = venera_debug_protocol::decode(&bytes)
                    .map_err(|_| io::ErrorKind::InvalidData)?;
                let bytes = if matches!(envelope.body, Message::SystemInfoResult { .. }) {
                    match serial.report(&envelope) {
                        Some(value) => serial_connection::line(&value)?,
                        None => continue,
                    }
                } else {
                    serial::encode(&envelope)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                };
                if let Err(issue) = write_line(port.0, &bytes, state, generation) {
                    if generation.is_some()
                        && !stopped()
                        && !state.lock().unwrap().write_allowed(generation)
                    {
                        // The canceled line may have reached the peer in part.
                        write_line(port.0, b"\n", state, None)?;
                        let mut shared = state.lock().unwrap();
                        let message = Envelope::new(Message::Gap {
                            stream_id: shared.stream_id.clone(),
                            reason: "local_forwarding_interrupted".into(),
                            count: None,
                            first_seq: None,
                            last_seq: None,
                        });
                        shared.broadcast(venera_debug_protocol::encode(&message).unwrap());
                        shared.queue_control(&message);
                    } else {
                        return Err(issue);
                    }
                }
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        Ok(())
    })();
    if pending {
        CancelIoEx(port.0, &ov);
        let mut bytes = 0;
        GetOverlappedResult(port.0, &ov, &mut bytes, 1);
    }
    result
}
