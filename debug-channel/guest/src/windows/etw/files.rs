use super::*;
use serde::Serialize;
use std::{io::Write, os::windows::ffi::OsStrExt, path::Path};

#[derive(Debug, Serialize)]
pub struct FileSummary {
    pub boot_time_filetime: String,
    pub start_time_filetime: String,
    pub end_time_filetime: String,
    pub clock_type: u32,
    pub perf_frequency: i64,
    pub events: u64,
    pub events_lost: u32,
    pub buffers_lost: u32,
    pub physical_bytes: u64,
}

pub fn file_identity(path: &Path) -> io::Result<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let file = std::fs::File::open(path)?;
    unsafe {
        let mut identity: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut identity) == 0 {
            return Err(error());
        }
        Ok(format!(
            "{}:{}:{}:{}:{}",
            identity.dwVolumeSerialNumber,
            identity.nFileIndexHigh,
            identity.nFileIndexLow,
            identity.ftCreationTime.dwHighDateTime,
            identity.ftCreationTime.dwLowDateTime
        ))
    }
}

pub fn header(path: &Path) -> io::Result<crate::journal::ImportSource> {
    let identity = file_identity(path)?;
    let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut log: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
        log.LogFileName = name.as_mut_ptr();
        log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_EVENT_RECORD;
        let trace = OpenTraceW(&mut log);
        if trace.Value == u64::MAX {
            return Err(error());
        }
        checked(CloseTrace(trace))?;
        if log.LogfileHeader.BootTime <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Trace header has no boot time",
            ));
        }
        Ok(crate::journal::ImportSource {
            id: format!(
                "{}:{}:{}",
                identity, log.LogfileHeader.StartTime, log.LogfileHeader.BootTime
            ),
            boot_filetime: log.LogfileHeader.BootTime as u64,
        })
    }
}

struct Reader<'a> {
    visit: &'a mut dyn FnMut(&EVENT_RECORD, u64, u64) -> io::Result<()>,
    boot: u64,
    ordinal: u64,
    failure: Option<io::Error>,
}

unsafe extern "system" fn file_event(event: *mut EVENT_RECORD) {
    let event = &*event;
    let reader = &mut *(event.UserContext as *mut Reader<'_>);
    if reader.failure.is_none() {
        reader.ordinal += 1;
        reader.failure = (reader.visit)(event, reader.boot, reader.ordinal).err();
    }
}

unsafe extern "system" fn buffer(log: *mut EVENT_TRACE_LOGFILEW) -> u32 {
    let reader = &*((*log).Context as *const Reader<'_>);
    u32::from(reader.failure.is_none())
}

pub fn read(
    path: &Path,
    visit: &mut dyn FnMut(&EVENT_RECORD, u64, u64) -> io::Result<()>,
) -> io::Result<FileSummary> {
    let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut reader = Reader {
        visit,
        boot: 0,
        ordinal: 0,
        failure: None,
    };
    unsafe {
        let mut log: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
        log.LogFileName = name.as_mut_ptr();
        log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_EVENT_RECORD;
        log.Anonymous2.EventRecordCallback = Some(file_event);
        log.BufferCallback = Some(buffer);
        log.Context = (&mut reader as *mut Reader<'_>).cast();
        let handle = OpenTraceW(&mut log);
        if handle.Value == u64::MAX {
            return Err(error());
        }
        reader.boot = log.LogfileHeader.BootTime as u64;
        if log.LogfileHeader.BootTime <= 0 {
            CloseTrace(handle);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Trace header has no boot time",
            ));
        }
        let code = ProcessTrace(&handle, 1, std::ptr::null(), std::ptr::null());
        let close = CloseTrace(handle);
        if let Some(error) = reader.failure {
            return Err(error);
        }
        checked(code)?;
        checked(close)?;
        Ok(FileSummary {
            boot_time_filetime: reader.boot.to_string(),
            start_time_filetime: log.LogfileHeader.StartTime.to_string(),
            end_time_filetime: log.LogfileHeader.EndTime.to_string(),
            clock_type: log.LogfileHeader.ReservedFlags,
            perf_frequency: log.LogfileHeader.PerfFreq,
            events: reader.ordinal,
            events_lost: recorded_loss(&log).0,
            buffers_lost: recorded_loss(&log).1,
            physical_bytes: path.metadata()?.len(),
        })
    }
}

fn recorded_loss(log: &EVENT_TRACE_LOGFILEW) -> (u32, u32) {
    unsafe {
        (
            log.LogfileHeader.Anonymous2.Anonymous.EventsLost,
            log.LogfileHeader.BuffersLost,
        )
    }
}

fn checked(code: u32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

fn properties(name: &[u16], path: Option<&Path>) -> io::Result<Vec<u64>> {
    // ControlTrace returns both names; reserve each documented 1024-character limit and its NUL.
    const NAME_UNITS: usize = 1025;
    let file: Vec<u16> = path
        .map(|path| path.as_os_str().encode_wide().chain(Some(0)).collect())
        .unwrap_or_else(|| vec![0]);
    if name.len() > NAME_UNITS || file.len() > NAME_UNITS {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let size = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + NAME_UNITS * 2 * 2;
    let mut storage = vec![0u64; size.div_ceil(8)];
    unsafe {
        let props = storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
        (*props).Wnode.BufferSize = (storage.len() * 8) as u32;
        (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        let base = props.cast::<u8>();
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            base.add((*props).LoggerNameOffset as usize).cast(),
            name.len(),
        );
        (*props).LogFileNameOffset = (*props).LoggerNameOffset + NAME_UNITS as u32 * 2;
        if path.is_some() {
            std::ptr::copy_nonoverlapping(
                file.as_ptr(),
                base.add((*props).LogFileNameOffset as usize).cast(),
                file.len(),
            );
        }
    }
    Ok(storage)
}

pub struct FileSession {
    name: Vec<u16>,
    handle: Option<CONTROLTRACE_HANDLE>,
    flags: u32,
    stop_on_drop: bool,
}

impl FileSession {
    pub fn start(name: &str, path: &Path, flags: u32, max_megabytes: u32) -> io::Result<Self> {
        if path.exists() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let name = wide(name);
        let mut storage = properties(&name, Some(path))?;
        unsafe {
            let props = storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
            (*props).Wnode.ClientContext = 2;
            (*props).LogFileMode = EVENT_TRACE_FILE_MODE_SEQUENTIAL
                | if flags != 0 {
                    EVENT_TRACE_SYSTEM_LOGGER_MODE
                } else {
                    0
                };
            (*props).EnableFlags = flags;
            (*props).MaximumFileSize = max_megabytes;
            (*props).BufferSize = 64;
            (*props).FlushTimer = 1;
            let mut handle = CONTROLTRACE_HANDLE { Value: 0 };
            checked(StartTraceW(&mut handle, name.as_ptr(), props))?;
            Ok(Self {
                name,
                handle: Some(handle),
                flags,
                stop_on_drop: true,
            })
        }
    }

    pub fn retain(&mut self) {
        self.stop_on_drop = false;
    }

    #[cfg(test)]
    pub fn control_status(&self) -> io::Result<(u64, u32, u32, u32, u32)> {
        let mut storage = properties(&self.name, None)?;
        unsafe {
            let props = storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
            checked(ControlTraceW(
                self.handle.unwrap(),
                self.name.as_ptr(),
                props,
                EVENT_TRACE_CONTROL_QUERY,
            ))?;
            Ok((
                (*props).Wnode.Anonymous1.HistoricalContext,
                (*props).LogFileMode,
                (*props).EventsLost,
                (*props).LogBuffersLost,
                (*props).RealTimeBuffersLost,
            ))
        }
    }

    pub fn attach(name: &str, directory: &Path) -> io::Result<(Self, std::path::PathBuf)> {
        Self::attach_sized(name, directory, crate::capture_storage::TRACE_MEGABYTES)
    }

    fn attach_sized(
        name: &str,
        directory: &Path,
        maximum: u32,
    ) -> io::Result<(Self, std::path::PathBuf)> {
        let name = wide(name);
        let mut storage = properties(&name, None)?;
        unsafe {
            let props = storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
            (*props).Wnode.BufferSize = (storage.len() * 8) as u32;
            checked(ControlTraceW(
                CONTROLTRACE_HANDLE { Value: 0 },
                name.as_ptr(),
                props,
                EVENT_TRACE_CONTROL_QUERY,
            ))?;
            let offset = (*props).LogFileNameOffset as usize;
            if offset == 0 || offset >= storage.len() * 8 || offset % 2 != 0 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let text = std::slice::from_raw_parts(
                storage.as_ptr().cast::<u8>().add(offset).cast::<u16>(),
                (storage.len() * 8 - offset) / 2,
            );
            let end = text
                .iter()
                .position(|v| *v == 0)
                .ok_or(io::ErrorKind::InvalidData)?;
            let path = std::path::PathBuf::from(
                String::from_utf16(&text[..end]).map_err(|_| io::ErrorKind::InvalidData)?,
            );
            if path.parent().and_then(|p| p.canonicalize().ok()) != Some(directory.canonicalize()?)
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Trace session output belongs to another capture",
                ));
            }
            if (*props).MaximumFileSize != maximum
                || (*props).LogFileMode & EVENT_TRACE_FILE_MODE_SEQUENTIAL == 0
                || (*props).LogFileMode & EVENT_TRACE_FILE_MODE_CIRCULAR != 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Capture session file settings differ from the storage reservation",
                ));
            }
            Ok((
                Self {
                    name,
                    handle: Some(CONTROLTRACE_HANDLE {
                        Value: (*props).Wnode.Anonymous1.HistoricalContext,
                    }),
                    flags: (*props).EnableFlags,
                    stop_on_drop: false,
                },
                path,
            ))
        }
    }

    pub fn enable(&self, provider: &GUID, level: u8, keyword: u64) -> io::Result<()> {
        unsafe {
            let params = ENABLE_TRACE_PARAMETERS {
                Version: ENABLE_TRACE_PARAMETERS_VERSION_2,
                EnableProperty: EVENT_ENABLE_PROPERTY_PROCESS_START_KEY,
                ..Default::default()
            };
            checked(EnableTraceEx2(
                self.handle.unwrap(),
                provider,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER,
                level,
                keyword,
                0,
                5000,
                &params,
            ))
        }
    }

    pub fn rotate(&mut self, path: &Path) -> io::Result<std::path::PathBuf> {
        if path.exists() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let mut storage = properties(&self.name, Some(path))?;
        unsafe {
            let props = storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
            (*props).EnableFlags = self.flags;
            checked(ControlTraceW(
                self.handle.unwrap(),
                self.name.as_ptr(),
                props,
                EVENT_TRACE_CONTROL_UPDATE,
            ))?;
        }
        let name = String::from_utf16(&self.name[..self.name.len() - 1])
            .map_err(|_| io::ErrorKind::InvalidData)?;
        let (queried, active) =
            Self::attach(&name, path.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
        if queried.handle.unwrap().Value != self.handle.unwrap().Value {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Capture session changed during rotation",
            ));
        }
        Ok(active)
    }

    pub fn running(&self) -> io::Result<bool> {
        let mut storage = properties(&self.name, None)?;
        let code = unsafe {
            ControlTraceW(
                self.handle.unwrap(),
                self.name.as_ptr(),
                storage.as_mut_ptr().cast(),
                EVENT_TRACE_CONTROL_QUERY,
            )
        };
        match code {
            ERROR_WMI_INSTANCE_NOT_FOUND => Ok(false),
            ERROR_SUCCESS | ERROR_MORE_DATA => Ok(true),
            code => {
                checked(code)?;
                Ok(true)
            }
        }
    }

    pub fn restart(mut self, path: &Path) -> io::Result<Self> {
        let running = self.running();
        // The completed session's control handle must retire before its name is reused.
        self.handle = None;
        if running? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Trace session is still running",
            ));
        }
        let name = String::from_utf16(&self.name[..self.name.len() - 1])
            .map_err(|_| io::ErrorKind::InvalidData)?;
        let mut session = Self::start(
            &name,
            path,
            self.flags,
            crate::capture_storage::TRACE_MEGABYTES,
        )?;
        session.retain();
        Ok(session)
    }

    pub fn stop(mut self) -> io::Result<()> {
        self.close()
    }

    fn close(&mut self) -> io::Result<()> {
        self.close_observed(|_, _| {})
    }

    fn close_observed(
        &mut self,
        stopped: impl FnOnce(u32, &EVENT_TRACE_PROPERTIES),
    ) -> io::Result<()> {
        if let Some(handle) = self.handle {
            // Query/stop can return the complete session and output names.
            let mut storage = properties(&self.name, None)?;
            unsafe {
                let props = storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
                (*props).Wnode.BufferSize = storage.len() as u32 * 8;
                (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
                let code =
                    ControlTraceW(handle, self.name.as_ptr(), props, EVENT_TRACE_CONTROL_STOP);
                stopped(code, &*props);
                if code != ERROR_MORE_DATA && code != ERROR_WMI_INSTANCE_NOT_FOUND {
                    checked(code)?;
                }
                self.handle = None;
            }
        }
        Ok(())
    }
}

impl Drop for FileSession {
    fn drop(&mut self) {
        if self.stop_on_drop {
            let _ = self.close();
        }
    }
}

pub fn replay(path: &Path) -> io::Result<()> {
    begin_replay(path)?.stop()
}

pub fn begin_replay(path: &Path) -> io::Result<FileSession> {
    begin_replay_observed(path, |_, _| Ok(()))
}

fn begin_replay_observed(
    path: &Path,
    mut observe: impl FnMut(&str, &FileSession) -> io::Result<()>,
) -> io::Result<FileSession> {
    let session = FileSession::start("Venera Recorder replay", path, 0, 4)?;
    observe("started", &session)?;
    session.enable(
        &GUID::from_u128(0x772013fb_617e_4269_a691_66a6847d2856),
        255,
        0xff,
    )?;
    observe("after_772013fb_enable", &session)?;
    session.enable(&RECORDER_PROVIDER, 255, 0x40000000)?;
    observe("after_recorder_enable", &session)?;
    Ok(session)
}

pub fn recover_recorders(directory: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let mut capacity = 64;
    for _ in 0..2 {
        let mut storage: Vec<Vec<u64>> = (0..capacity).map(|_| vec![0u64; 8192]).collect();
        let mut pointers: Vec<_> = storage
            .iter_mut()
            .map(|buffer| {
                let props = buffer.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
                unsafe {
                    (*props).Wnode.BufferSize = (buffer.len() * 8) as u32;
                    (*props).LoggerNameOffset =
                        std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
                    (*props).LogFileNameOffset = (*props).LoggerNameOffset + 2048;
                }
                props
            })
            .collect();
        let mut count = 0;
        let code = unsafe { QueryAllTracesW(pointers.as_mut_ptr(), capacity as u32, &mut count) };
        if code == ERROR_MORE_DATA && count <= 1024 && count as usize > capacity {
            capacity = count as usize;
            continue;
        }
        checked(code)?;
        let mut recovered = Vec::new();
        for &props in pointers.iter().take(count as usize) {
            let name = unsafe {
                let offset = (*props).LoggerNameOffset as usize;
                if offset == 0 || offset >= 65536 || offset % 2 != 0 {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                let text = std::slice::from_raw_parts(
                    props.cast::<u8>().add(offset).cast::<u16>(),
                    (65536 - offset) / 2,
                );
                let end = text
                    .iter()
                    .position(|v| *v == 0)
                    .ok_or(io::ErrorKind::InvalidData)?;
                String::from_utf16(&text[..end]).map_err(|_| io::ErrorKind::InvalidData)?
            };
            if name != "Venera Recorder replay"
                && !name
                    .strip_prefix("Venera Recorder replay ")
                    .is_some_and(|pid| pid.parse::<u32>().is_ok())
            {
                continue;
            }
            let (session, path) = match FileSession::attach_sized(&name, directory, 4) {
                Ok(pair) => pair,
                Err(error)
                    if error.raw_os_error() == Some(ERROR_WMI_INSTANCE_NOT_FOUND as i32)
                        || (error.kind() == io::ErrorKind::PermissionDenied
                            && error.raw_os_error().is_none()) =>
                {
                    continue
                }
                Err(error) => return Err(error),
            };
            if !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("recorder-v1-"))
            {
                continue;
            }
            session.stop()?;
            match FileSession::attach_sized(&name, directory, 4) {
                Err(error) if error.raw_os_error() == Some(ERROR_WMI_INSTANCE_NOT_FOUND as i32) => {
                    ()
                }
                Err(error) => return Err(error),
                Ok(_) => return Err(io::Error::other("Recorder export session is still active")),
            }
            recovered.push(path);
        }
        return Ok(recovered);
    }
    Err(io::Error::other(
        "Trace session inventory changed during Recorder recovery",
    ))
}

pub fn inspect(path: &Path, output: &mut dyn Write) -> io::Result<()> {
    let mut decoder = Decoder::default();
    let guid = recorder_message();
    let summary = read(path, &mut |record, boot, ordinal| {
        let capture = unsafe { decode_record_from(record, boot, guid, &mut decoder) };
        let provider = guid_text(record.EventHeader.ProviderId);
        let mut value = serde_json::json!({"type":"event", "ordinal":ordinal.to_string(), "provider":provider,
            "id":record.EventHeader.EventDescriptor.Id,"version":record.EventHeader.EventDescriptor.Version,
            "level":record.EventHeader.EventDescriptor.Level,
            "opcode":record.EventHeader.EventDescriptor.Opcode,"task":record.EventHeader.EventDescriptor.Task,
            "process_id":record.EventHeader.ProcessId,"thread_id":record.EventHeader.ThreadId,
            "flags":record.EventHeader.Flags,"source_time_filetime":record.EventHeader.TimeStamp.to_string()});
        if record.EventHeader.Flags & EVENT_HEADER_FLAG_TRACE_MESSAGE as u16 != 0 {
            match unsafe { format_wpp(record) } {
                Ok(text) => value["wpp_message"] = serde_json::json!(text),
                Err(error) => value["wpp_format_error"] = serde_json::json!(error.to_string()),
            }
        }
        match unsafe { process_lifecycle(record, boot) } {
            Ok(Some(change)) => value["process_lifecycle"] = serde_json::to_value(change)?,
            Err(error) => value["process_lifecycle_error"] = serde_json::json!(error.to_string()),
            _ => (),
        }
        match unsafe { process_start_key(record) } {
            Ok(Some(key)) => {
                value["execution_process_start_key"] = serde_json::json!(key.to_string())
            }
            Err(error) => value["process_key_error"] = serde_json::json!(error.to_string()),
            _ => (),
        }
        match capture {
            Ok(Some(capture)) => value["capture"] = serde_json::to_value(capture)?,
            result => {
                if let Err(error) = result {
                    value["decode_error"] = serde_json::json!(error.to_string());
                }
                if !record.UserData.is_null() {
                    let data = unsafe {
                        std::slice::from_raw_parts(
                            record.UserData as *const u8,
                            record.UserDataLength as usize,
                        )
                    };
                    value["data"] = serde_json::json!(data
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>());
                }
            }
        }
        serde_json::to_writer(&mut *output, &value)?;
        output.write_all(b"\n")
    })?;
    serde_json::to_writer(
        &mut *output,
        &serde_json::json!({"type":"summary","trace":summary}),
    )?;
    output.write_all(b"\n")
}

unsafe fn format_wpp(record: &EVENT_RECORD) -> io::Result<String> {
    let tmf = std::env::current_exe()?.with_file_name("venera-recorder.tmf");
    if std::fs::read_to_string(&tmf)? != crate::kernel_events::RECORDER_TMF {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Recorder formatting data differs from the collector build",
        ));
    }
    let path: Vec<u16> = tmf.as_os_str().encode_wide().chain(Some(0)).collect();
    let context = TDH_CONTEXT {
        ParameterValue: path.as_ptr() as u64,
        ParameterType: TDH_CONTEXT_WPP_TMFFILE,
        ParameterSize: (path.len() * 2) as u32,
    };
    let name = wide("FormattedString");
    let descriptor = PROPERTY_DATA_DESCRIPTOR {
        PropertyName: name.as_ptr() as u64,
        ArrayIndex: u32::MAX,
        Reserved: 0,
    };
    let mut size = 0;
    checked(TdhGetPropertySize(
        record,
        1,
        &context,
        1,
        &descriptor,
        &mut size,
    ))?;
    if size > 65536 || size % 2 != 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut output = vec![0u16; size as usize / 2];
    checked(TdhGetProperty(
        record,
        1,
        &context,
        1,
        &descriptor,
        size,
        output.as_mut_ptr().cast(),
    ))?;
    let end = output
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(output.len());
    String::from_utf16(&output[..end])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::Duration};

    #[test]
    fn recorded_loss_preserves_trace_header_counters() {
        let mut log: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
        log.EventsLost = 58;
        assert_eq!(recorded_loss(&log), (0, 0));
        log.LogfileHeader.Anonymous2.Anonymous.EventsLost = 7;
        log.LogfileHeader.BuffersLost = 2;
        assert_eq!(recorded_loss(&log), (7, 2));
    }

    #[test]
    #[ignore = "Reports the loss fields of a preserved ETL file"]
    fn preserved_trace_loss_fields() {
        let path = std::path::PathBuf::from(std::env::var_os("VENERA_TEST_ETL").unwrap());
        let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let mut log: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
            log.LogFileName = name.as_mut_ptr();
            log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_EVENT_RECORD;
            let handle = OpenTraceW(&mut log);
            assert_ne!(handle.Value, u64::MAX);
            println!(
                "{}",
                serde_json::json!({"file":path,"start":log.LogfileHeader.StartTime,
                "end":log.LogfileHeader.EndTime,"header_events_lost":recorded_loss(&log).0,
                "header_buffers_lost":recorded_loss(&log).1,"unused_logfile_events_lost":log.EventsLost})
            );
            checked(CloseTrace(handle)).unwrap();
        }
        let summary = read(&path, &mut |_, _, _| Ok(())).unwrap();
        println!("{}", serde_json::to_string(&summary).unwrap());
    }

    #[test]
    #[ignore = "Observes one small-buffer UPDATE on a separately provisioned test AutoLogger"]
    fn controlled_autologger_update_reports_transition() {
        let name = std::env::var("VENERA_TEST_AUTOLOGGER").unwrap();
        assert!(name.starts_with("Venera AutoLogger test "));
        let root =
            std::path::PathBuf::from(std::env::var_os("VENERA_TEST_AUTOLOGGER_DIRECTORY").unwrap());
        let (mut session, original) = FileSession::attach(&name, &root).unwrap();
        let before = session.control_status().unwrap();
        assert_eq!(
            before.1,
            EVENT_TRACE_FILE_MODE_SEQUENTIAL | EVENT_TRACE_PERSIST_ON_HYBRID_SHUTDOWN
        );
        assert!(original
            .extension()
            .unwrap()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit()));
        let requested = root.join(format!("small-update-{}.etl", clock::filetime()));
        let file: Vec<u16> = requested.as_os_str().encode_wide().chain(Some(0)).collect();
        let size =
            std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + (session.name.len() + file.len()) * 2;
        let mut storage = vec![0u64; size.div_ceil(8)];
        let code = unsafe {
            let props = storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
            (*props).Wnode.BufferSize = size as u32;
            (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*props).EnableFlags = session.flags;
            (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            (*props).LogFileNameOffset = (*props).LoggerNameOffset + session.name.len() as u32 * 2;
            std::ptr::copy_nonoverlapping(
                session.name.as_ptr(),
                props
                    .cast::<u8>()
                    .add((*props).LoggerNameOffset as usize)
                    .cast(),
                session.name.len(),
            );
            std::ptr::copy_nonoverlapping(
                file.as_ptr(),
                props
                    .cast::<u8>()
                    .add((*props).LogFileNameOffset as usize)
                    .cast(),
                file.len(),
            );
            ControlTraceW(
                session.handle.unwrap(),
                session.name.as_ptr(),
                props,
                EVENT_TRACE_CONTROL_UPDATE,
            )
        };
        let (query, actual) = FileSession::attach(&name, &root).unwrap();
        let after = query.control_status().unwrap();
        println!(
            "{}",
            serde_json::json!({"update_result":code,"input_bytes":size,
            "requested":requested,"before_path":original,"after_path":actual,
            "before_control":before,"after_control":after})
        );
        assert_eq!(after.0, before.0);
        assert!(matches!(code, ERROR_SUCCESS | ERROR_MORE_DATA));
        let next = root.join(format!("full-update-{}.etl", clock::filetime()));
        let returned = session.rotate(&next).unwrap();
        let (query, actual) = FileSession::attach(&name, &root).unwrap();
        println!(
            "{}",
            serde_json::json!({"full_update_requested":next,"full_update_actual":actual,
            "control":query.control_status().unwrap()})
        );
        assert_eq!(returned, actual);
        assert_eq!(query.control_status().unwrap().0, before.0);
        assert!(actual.exists());
        assert_ne!(actual, original);
    }

    #[test]
    #[ignore = "Queries the running boot capture and its property-buffer requirements"]
    fn boot_rotation_property_capacity() {
        fn text(storage: &[u64], offset: u32) -> Option<String> {
            let offset = offset as usize;
            if offset == 0 || offset % 2 != 0 || offset >= storage.len() * 8 {
                return None;
            }
            let text = unsafe {
                std::slice::from_raw_parts(
                    storage.as_ptr().cast::<u8>().add(offset).cast::<u16>(),
                    (storage.len() * 8 - offset) / 2,
                )
            };
            let end = text.iter().position(|value| *value == 0)?;
            String::from_utf16(&text[..end]).ok()
        }
        fn query(label: &str, name: &[u16], storage: &mut [u64]) -> u32 {
            let allocated = storage.len() * 8;
            let props = storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
            let code = unsafe {
                ControlTraceW(
                    CONTROLTRACE_HANDLE { Value: 0 },
                    name.as_ptr(),
                    props,
                    EVENT_TRACE_CONTROL_QUERY,
                )
            };
            let props = unsafe { &*props };
            println!(
                "{}",
                serde_json::json!({
                    "query": label, "result": code, "allocated": allocated,
                    "returned_buffer_size": props.Wnode.BufferSize,
                    "logger_offset": props.LoggerNameOffset, "file_offset": props.LogFileNameOffset,
                    "logger": text(storage, props.LoggerNameOffset),
                    "file": text(storage, props.LogFileNameOffset),
                    "mode": props.LogFileMode, "maximum_mb": props.MaximumFileSize,
                    "buffer_kb": props.BufferSize, "minimum_buffers": props.MinimumBuffers,
                    "maximum_buffers": props.MaximumBuffers, "buffers_written": props.BuffersWritten,
                    "events_lost": props.EventsLost, "log_buffers_lost": props.LogBuffersLost,
                    "realtime_buffers_lost": props.RealTimeBuffersLost,
                    "clock": props.Wnode.ClientContext,
                    "handle": unsafe { props.Wnode.Anonymous1.HistoricalContext }
                })
            );
            code
        }
        let name = wide("VeneraKernelLog");
        let directory = std::path::PathBuf::from(std::env::var_os("ProgramData").unwrap())
            .join("Venera")
            .join("DebugCapture");
        let mut full = vec![0u64; 8192];
        unsafe {
            (*full.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>())
                .Wnode
                .BufferSize = 65536;
        }
        let full_code = query("attach_sized", &name, &mut full);
        assert_eq!(full_code, 0, "full-sized boot session query");
        let offset = unsafe { (*full.as_ptr().cast::<EVENT_TRACE_PROPERTIES>()).LogFileNameOffset };
        let active = text(&full, offset).unwrap();
        let base = active.strip_suffix(".001").unwrap_or(&active);
        let file: Vec<u16> = base.encode_utf16().chain(Some(0)).collect();
        let old_size =
            std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + (name.len() + file.len()) * 2;
        let mut old = vec![0u64; old_size.div_ceil(8)];
        unsafe {
            let props = old.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
            (*props).Wnode.BufferSize = old_size as u32;
            (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            (*props).LogFileNameOffset = (*props).LoggerNameOffset + name.len() as u32 * 2;
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                props
                    .cast::<u8>()
                    .add((*props).LoggerNameOffset as usize)
                    .cast(),
                name.len(),
            );
            std::ptr::copy_nonoverlapping(
                file.as_ptr(),
                props
                    .cast::<u8>()
                    .add((*props).LogFileNameOffset as usize)
                    .cast(),
                file.len(),
            );
        }
        query("r6b_input_sized_query", &name, &mut old);
        let mut small = properties(&name, Some(Path::new(base))).unwrap();
        let mut padded = small.clone();
        padded.resize(8192, 0);
        unsafe {
            (*padded.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>())
                .Wnode
                .BufferSize = 65536;
        }
        let small_code = query("rotation_sized_query", &name, &mut small);
        let padded_code = query("padded_rotation_sized_query", &name, &mut padded);
        let mut buffers: Vec<Vec<u64>> = (0..1024).map(|_| vec![0u64; 8192]).collect();
        let mut pointers: Vec<_> = buffers
            .iter_mut()
            .map(|buffer| {
                let props = buffer.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
                unsafe {
                    (*props).Wnode.BufferSize = 65536;
                    (*props).LoggerNameOffset =
                        std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
                    (*props).LogFileNameOffset = (*props).LoggerNameOffset + 2048;
                }
                props
            })
            .collect();
        let mut count = 0;
        let initial_code = unsafe { QueryAllTracesW(pointers.as_mut_ptr(), 64, &mut count) };
        println!(
            "{}",
            serde_json::json!({"query":"QueryAllTraces", "capacity":64,
            "result":initial_code,"count":count})
        );
        for props in &pointers {
            unsafe {
                (**props).Wnode.BufferSize = 65536;
                (**props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
                (**props).LogFileNameOffset = (**props).LoggerNameOffset + 2048;
            }
        }
        count = 0;
        let all_code = unsafe { QueryAllTracesW(pointers.as_mut_ptr(), 1024, &mut count) };
        println!(
            "{}",
            serde_json::json!({"query":"QueryAllTraces", "capacity":1024,
            "result":all_code,"count":count})
        );
        let attach = FileSession::attach("VeneraKernelLog", &directory);
        match attach {
            Ok((session, path)) => println!(
                "{}",
                serde_json::json!({"attach_path":path,
                "running":format!("{:?}", session.running())})
            ),
            Err(error) => println!(
                "{}",
                serde_json::json!({"attach_error":error.to_string(),
                "attach_os_error":error.raw_os_error()})
            ),
        }
        assert_eq!(padded_code, 0, "padded rotation property query");
        assert_eq!(all_code, 0, "session enumeration");
        assert_eq!(
            small_code, 0,
            "rotation property buffer accommodates the numbered output"
        );
    }

    fn directory(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "venera-{label}-{}-{}",
            std::process::id(),
            clock::filetime()
        ));
        fs::create_dir(&path).unwrap();
        println!("Trace evidence: {}", path.display());
        path
    }

    #[test]
    #[ignore = "Exercises a 32 MiB Windows trace file at capacity"]
    fn sequential_file_fits_the_physical_capture_reservation() {
        let root = directory("capacity");
        let path = root.join("capacity.etl");
        let session = FileSession::start(
            &format!("Venera capacity test {}", std::process::id()),
            &path,
            0,
            32,
        )
        .unwrap();
        let mut provider = 0;
        unsafe {
            checked(EventRegister(
                &KERNEL_PROVIDER,
                None,
                std::ptr::null(),
                &mut provider,
            ))
            .unwrap();
        }
        session.enable(&KERNEL_PROVIDER, 5, 1).unwrap();
        let payload = [1u8; 8192];
        let data = EVENT_DATA_DESCRIPTOR {
            Ptr: payload.as_ptr() as u64,
            Size: payload.len() as u32,
            ..Default::default()
        };
        let descriptor = EVENT_DESCRIPTOR {
            Id: 65535,
            Version: 1,
            Level: 4,
            Keyword: 1,
            ..Default::default()
        };
        for _ in 0..6000 {
            unsafe {
                checked(EventWrite(provider, &descriptor, 1, &data)).unwrap();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        unsafe {
            checked(EventUnregister(provider)).unwrap();
        }
        let result = session.stop();
        println!("Capacity session stop: {result:?}");
        let bytes = fs::metadata(path).unwrap().len();
        println!("Physical trace bytes: {bytes}");
        assert!((32 * 1024 * 1024 - 65536..=32 * 1024 * 1024 + 65536).contains(&bytes));
    }

    #[test]
    #[ignore = "Runs an elevated Windows trace session"]
    fn rotation_preserves_every_event_and_seals_files_while_emitting() {
        let root = directory("rotation");
        let mut file = root.join("part-0.etl");
        let mut session = FileSession::start(
            &format!("Venera rotation test {}", std::process::id()),
            &file,
            EVENT_TRACE_FLAG_DBGPRINT,
            32,
        )
        .unwrap();
        let mut provider = 0;
        unsafe {
            checked(EventRegister(
                &KERNEL_PROVIDER,
                None,
                std::ptr::null(),
                &mut provider,
            ))
            .unwrap();
        }
        session.enable(&KERNEL_PROVIDER, 5, 1).unwrap();
        let instance = clock::filetime();
        let producer = std::thread::spawn(move || {
            let descriptor = EVENT_DESCRIPTOR {
                Id: 1,
                Version: 1,
                Level: 4,
                Keyword: 1,
                ..Default::default()
            };
            for sequence in 1u64..=300 {
                let mut payload = Vec::new();
                payload.extend_from_slice(&instance.to_le_bytes());
                payload.extend_from_slice(&sequence.to_le_bytes());
                payload.extend_from_slice(&clock::filetime().to_le_bytes());
                payload.extend_from_slice(&[1, 0, 1, 4]);
                payload.extend_from_slice(&[0; 8]);
                payload.extend_from_slice(b"rotation-test\0equal text\0");
                let data = EVENT_DATA_DESCRIPTOR {
                    Ptr: payload.as_ptr() as u64,
                    Size: payload.len() as u32,
                    ..Default::default()
                };
                unsafe {
                    checked(EventWrite(provider, &descriptor, 1, &data)).unwrap();
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            unsafe {
                checked(EventUnregister(provider)).unwrap();
            }
        });
        let mut sequences = Vec::new();
        let mut latency = 0;
        let mut visit = |record: &EVENT_RECORD, boot, _| -> io::Result<()> {
            if let Some(capture) = unsafe { decode_record(record, boot) }? {
                if let Some(event) = capture
                    .structured
                    .filter(|event| event.producer_instance.0 == instance)
                {
                    latency =
                        latency.max(clock::filetime().saturating_sub(event.source_time_filetime.0));
                    sequences.push(event.producer_sequence.0);
                }
            }
            Ok(())
        };
        let mut part = 1;
        while !producer.is_finished() {
            std::thread::sleep(Duration::from_millis(100));
            let next = root.join(format!("part-{part}.etl"));
            let active = session.rotate(&next).unwrap();
            let size = fs::metadata(&file).unwrap().len();
            let summary = read(&file, &mut visit).unwrap();
            assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
            assert_eq!(fs::metadata(&file).unwrap().len(), size);
            file = active;
            part += 1;
        }
        producer.join().unwrap();
        session.stop().unwrap();
        let summary = read(&file, &mut visit).unwrap();
        assert_eq!((summary.events_lost, summary.buffers_lost), (0, 0));
        sequences.sort_unstable();
        assert_eq!(sequences, (1u64..=300).collect::<Vec<_>>());
        assert!(part >= 2);
        assert!(
            latency < 5 * 10_000_000,
            "maximum delivery latency: {latency} FILETIME ticks"
        );
        println!(
            "300 events, {} files, max latency {} ms",
            part,
            latency / 10_000
        );
    }

    #[test]
    #[ignore = "Reads the running Venera driver's Recorder on Windows"]
    fn recorder_replay_decodes_driver_initialization_and_registration() {
        let root = directory("recorder");
        let path = root.join("recorder.etl");
        let mut session = begin_replay_observed(&path, |stage, session| {
            let (handle, mode, events, log, realtime) = session.control_status()?;
            println!(
                "{}",
                serde_json::json!({"stage":stage,"handle":handle,"mode":mode,
                "events_lost":events,"log_buffers_lost":log,"realtime_buffers_lost":realtime})
            );
            Ok(())
        })
        .unwrap();
        session.close_observed(|code, props| {
            let counters = (code == ERROR_SUCCESS).then(|| serde_json::json!({
                "events_lost":props.EventsLost,"log_buffers_lost":props.LogBuffersLost,
                "realtime_buffers_lost":props.RealTimeBuffersLost,"buffers_written":props.BuffersWritten}));
            println!("{}", serde_json::json!({"stage":"stop","result":code,"counters":counters}));
        }).unwrap();
        let mut output = fs::File::create(root.join("recorder.jsonl")).unwrap();
        inspect(&path, &mut output).unwrap();
        let mut by_kind = std::collections::BTreeMap::new();
        let mut events = Vec::new();
        let mut formatted = Vec::new();
        let mut errors = Vec::new();
        let summary = read(&path, &mut |record, boot, ordinal| {
            let guid = guid_text(record.EventHeader.ProviderId);
            let kind = if record.EventHeader.Flags & EVENT_HEADER_FLAG_TRACE_MESSAGE as u16 != 0 {
                "wpp"
            } else if guid == "68fdd900-4a3e-11d1-84f4-0000f80464e3" {
                "system_trace"
            } else {
                "other"
            };
            *by_kind
                .entry(kind)
                .or_insert_with(std::collections::BTreeMap::new)
                .entry(guid)
                .or_insert(0u64) += 1;
            match unsafe { decode_record(record, boot) } {
                Ok(Some(capture)) if capture.source == "wpp_recorder" => {
                    match unsafe { format_wpp(record) } {
                        Ok(text) => formatted.push(text),
                        Err(err) => errors.push((ordinal, err.to_string())),
                    }
                    events.push(capture.structured.unwrap());
                }
                Err(err) => errors.push((ordinal, err.to_string())),
                _ => (),
            }
            Ok(())
        })
        .unwrap();
        let loss = (summary.events_lost, summary.buffers_lost);
        println!(
            "{}",
            serde_json::json!({"records":by_kind,"header":summary,
            "expected_message_guid":guid_text(recorder_message()),"recorder_messages":formatted,
            "decoded_events":events.len(),"zero_loss":loss==(0,0)})
        );
        let content = std::panic::catch_unwind(|| {
            assert!(errors.is_empty(), "record errors: {errors:?}");
            let initialization = events
                .iter()
                .find(|event| event.event_id == 2)
                .expect("initialization event");
            let registration = events
                .iter()
                .find(|event| event.event_id == 3)
                .expect("registration event");
            assert_eq!(
                initialization.fields["function"],
                venera_debug_protocol::v1::EventValue::AnsiString("DriverEntry".into())
            );
            assert_eq!(
                registration.fields["function"],
                venera_debug_protocol::v1::EventValue::AnsiString("DxgkInitialize".into())
            );
            assert_eq!(
                initialization.producer_instance,
                registration.producer_instance
            );
            assert!(initialization.producer_sequence.0 < registration.producer_sequence.0);
            assert!(formatted
                .iter()
                .any(|message| message.contains("Venera kernel initialization")));
        });
        println!("content_ok={}", content.is_ok());
        assert_eq!(loss, (0, 0));
        if let Err(failure) = content {
            std::panic::resume_unwind(failure);
        }
    }
}
