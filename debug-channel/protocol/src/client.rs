//! Connection-scoped RPC negotiation and history consumption.
use crate::v1::*;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    time::Instant,
};

pub struct Negotiation {
    pub peer: PeerProtocol,
    pub failure: Option<RpcError>,
    pub since: Instant,
}
impl Default for Negotiation {
    fn default() -> Self {
        Self::new()
    }
}
impl Negotiation {
    pub fn new() -> Self {
        Self {
            peer: PeerProtocol::default(),
            failure: None,
            since: Instant::now(),
        }
    }
    pub fn require(&self, method: Method) -> RpcResult<()> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        self.peer.require(method)?;
        if self
            .peer
            .endpoint()
            .is_some_and(|info| info.role != EndpointRole::Guest)
        {
            return Err(RpcError::new(-32003, "incompatible_protocol"));
        }
        Ok(())
    }
    pub fn accept(&mut self, method: Method, result: RpcResult<Value>) {
        let accepted = result.and_then(|value| {
            validate_result(method, &value)?;
            match method {
                Method::EndpointInfo => self
                    .peer
                    .accept_endpoint(serde_json::from_value(value).unwrap()),
                Method::Discover => self
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
            .is_some_and(|info| info.role != EndpointRole::Guest)
        {
            self.failure = Some(RpcError::new(-32003, "incompatible_protocol"));
        }
    }
}

pub struct Reply {
    pub id: String,
    pub method: Method,
    pub result: RpcResult<Value>,
}

#[derive(Default)]
pub struct Client {
    negotiation: Negotiation,
    history_attribution: bool,
    framer: JsonlFramer,
    serial: u64,
    calls: BTreeMap<String, Method>,
    output: Vec<u8>,
    offset: usize,
    events: Vec<(Incoming, String)>,
    errors: Vec<Value>,
}

pub fn encode(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).expect("JSON value");
    bytes.push(b'\n');
    bytes
}

pub fn validate_result(method: Method, value: &Value) -> RpcResult<()> {
    let valid = match method {
        Method::EndpointInfo => serde_json::from_value::<EndpointInfo>(value.clone()).is_ok(),
        Method::Discover => serde_json::from_value::<Discovery>(value.clone()).is_ok(),
        Method::StatusGet | Method::FilterSet | Method::FilterReset | Method::ForwardingSet => {
            serde_json::from_value::<Status>(value.clone()).is_ok()
        }
        Method::SystemInfoGet => serde_json::from_value::<SystemInfo>(value.clone()).is_ok(),
        Method::HistoryRange => serde_json::from_value::<HistoryRange>(value.clone()).is_ok(),
        Method::HistoryRead => serde_json::from_value::<HistoryPage>(value.clone()).is_ok(),
        Method::HistoryAck => serde_json::from_value::<HistoryAck>(value.clone()).is_ok(),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(RpcError::new(-32004, "Invalid guest result"))
    }
}

impl Client {
    /// Opt in when the caller can consume attribution pages and commit their projections.
    pub fn with_history_attribution() -> Self {
        Self {
            history_attribution: true,
            ..Self::default()
        }
    }
    fn endpoint_info(&self) -> EndpointInfo {
        let info = EndpointInfo::new(EndpointRole::Host, env!("CARGO_PKG_VERSION"));
        if self.history_attribution {
            info.with_history_attribution()
        } else {
            info
        }
    }
    pub fn history_encoding(&self) -> HistoryEncoding {
        if self.negotiation.require(Method::HistoryRead).is_ok() {
            HistoryEncoding::negotiate(&self.endpoint_info(), self.peer())
        } else {
            HistoryEncoding::Compatibility
        }
    }
    pub fn connect(&mut self) {
        self.disconnect();
        self.negotiation.peer.connect();
        self.queue(Method::EndpointInfo, json!({}));
        self.queue(Method::Discover, json!({}));
    }
    pub fn disconnect(&mut self) {
        self.negotiation = Negotiation::new();
        self.framer.clear();
        self.calls.clear();
        self.output.clear();
        self.offset = 0;
        self.events.clear();
        self.errors.clear();
    }
    pub fn peer(&self) -> &PeerProtocol {
        &self.negotiation.peer
    }
    pub fn has_output(&self) -> bool {
        self.offset < self.output.len()
    }
    pub fn negotiation(&self) -> &Negotiation {
        &self.negotiation
    }
    pub fn fail(&mut self, error: RpcError) {
        self.negotiation.failure = Some(error);
        self.calls.clear();
    }
    pub fn cancel(&mut self, id: &str) {
        self.calls.remove(id);
    }
    pub fn take_events(&mut self) -> Vec<(Incoming, String)> {
        std::mem::take(&mut self.events)
    }
    pub fn take_errors(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.errors)
    }
    pub fn require(&self, method: Method) -> RpcResult<()> {
        self.negotiation.require(method)
    }
    fn queue(&mut self, method: Method, params: Value) -> String {
        self.serial += 1;
        let id = format!("client-{}", self.serial);
        self.output.extend(encode(
            &json!({"jsonrpc":"2.0","id":id,"method":method.name(),"params":params}),
        ));
        self.calls.insert(id.clone(), method);
        id
    }
    pub fn request(&mut self, method: Method, params: Value) -> RpcResult<String> {
        self.require(method)?;
        if self.calls.len() >= 64 {
            return Err(RpcError::new(-32000, "Request queue is busy"));
        }
        Ok(self.queue(method, params))
    }
    pub fn receive(&mut self, bytes: &[u8]) -> RpcResult<Vec<Reply>> {
        self.events.clear();
        self.errors.clear();
        let mut replies = Vec::new();
        for frame in self.framer.push(bytes) {
            let frame = match frame {
                Ok(frame) => frame,
                Err(error) => {
                    self.errors.push(json!(error));
                    self.output.extend(encode(&json!(RpcResponse::Error {
                        jsonrpc: Version::V2,
                        id: RpcId::Null,
                        error
                    })));
                    continue;
                }
            };
            let original: Option<Value> = serde_json::from_slice(frame.bytes()).ok();
            let array = matches!(original, Some(Value::Array(_)));
            let sources: Vec<String> = match &original {
                Some(Value::Array(values)) if !values.is_empty() => {
                    values.iter().map(Value::to_string).collect()
                }
                _ => vec![frame.text().to_owned()],
            };
            let mut accepted = Vec::new();
            for source in &sources {
                let value: Option<Value> = serde_json::from_str(source).ok();
                let packet = RpcPacket::decode(source.as_bytes(), Receiver::Host);
                let response_like = value.as_ref().is_some_and(|v| {
                    v.get("method").is_none()
                        && (v.get("result").is_some() || v.get("error").is_some())
                });
                if response_like
                    && packet
                        .items
                        .iter()
                        .any(|item| matches!(item, PacketItem::Rejected(_)))
                {
                    self.errors.push(json!("Invalid guest response"));
                    continue;
                }
                for item in &packet.items {
                    if let PacketItem::Request(request) = item {
                        if matches!(
                            request.incoming,
                            Incoming::StatusChanged(_) | Incoming::Log(_) | Incoming::Gap(_)
                        ) {
                            self.events.push((request.incoming.clone(), source.clone()));
                        }
                    }
                }
                accepted.push(source.as_str());
            }
            if accepted.is_empty() {
                continue;
            }
            let filtered;
            let bytes = if array
                && matches!(original, Some(Value::Array(ref values)) if !values.is_empty())
            {
                filtered = format!("[{}]", accepted.join(","));
                filtered.as_bytes()
            } else {
                frame.bytes()
            };
            let mut packet = RpcPacket::decode(bytes, Receiver::Host);
            for item in packet.items {
                match item {
                    PacketItem::Response(response) => {
                        let (id, result) = match response {
                            RpcResponse::Success { id, result, .. } => (id, Ok(result)),
                            RpcResponse::Error { id, error, .. } => (id, Err(error)),
                        };
                        let RpcId::String(id) = id else { continue };
                        let Some(method) = self.calls.remove(&id) else {
                            continue;
                        };
                        let result = result.and_then(|value| {
                            if method == Method::HistoryRead
                                && self.history_encoding() == HistoryEncoding::Attribution
                            {
                                serde_json::from_value::<AttributionHistoryPage>(value.clone())
                                    .map_err(|_| RpcError::new(-32004, "Invalid guest result"))?
                                    .validate_revisions()?;
                            } else {
                                validate_result(method, &value)?;
                            }
                            Ok(value)
                        });
                        if matches!(method, Method::EndpointInfo | Method::Discover) {
                            self.negotiation.accept(method, result.clone());
                        }
                        replies.push(Reply { id, method, result });
                    }
                    PacketItem::Request(request) => {
                        let result = match request.incoming {
                            Incoming::EndpointInfo { .. } => Ok(json!(self.endpoint_info())),
                            Incoming::Discover { .. } => Ok(serde_json::from_str(include_str!(
                                "../v1/host.openrpc.json"
                            ))
                            .expect("host discovery")),
                            _ => Ok(Value::Null),
                        };
                        if let Some(slot) = request.correlation {
                            packet.batch.complete(slot, result)?;
                        }
                    }
                    PacketItem::Rejected(error) => self.errors.push(json!(error)),
                }
            }
            if let Some(value) = packet.batch.take_response() {
                self.output.extend(encode(&value));
            }
        }
        Ok(replies)
    }
    pub fn write(&mut self, stream: &mut (impl Write + ?Sized)) -> io::Result<()> {
        if self.output.len().saturating_sub(self.offset) > 2 * MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "Protocol output queue is full",
            ));
        }
        if self.offset < self.output.len() {
            match stream.write(&self.output[self.offset..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => self.offset += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                Err(error) => return Err(error),
            }
        }
        if self.offset == self.output.len() {
            self.output.clear();
            self.offset = 0;
        }
        Ok(())
    }
    pub fn read(&mut self, stream: &mut (impl Read + ?Sized)) -> io::Result<Vec<Reply>> {
        let mut bytes = [0; 65536];
        match stream.read(&mut bytes) {
            Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => self
                .receive(&bytes[..n])
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.message)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Start {
    Beginning,
    Now,
}

pub struct HistoryConsumer {
    start: Start,
    cursor: Option<HistoryCursor>,
    through: u64,
    page: Option<HistoryCursor>,
}

impl HistoryConsumer {
    pub fn new(start: Start) -> Self {
        Self {
            start,
            cursor: None,
            through: 0,
            page: None,
        }
    }
    pub fn cursor(&self) -> Option<&HistoryCursor> {
        self.cursor.as_ref()
    }
    pub fn display(
        &mut self,
        page: &HistoryPage,
        filter: &crate::Filter,
        output: &mut impl Write,
    ) -> io::Result<HistoryAck> {
        let invalid = |error: RpcError| io::Error::new(io::ErrorKind::InvalidData, error.message);
        self.page(page).map_err(invalid)?;
        let mut entries = Vec::new();
        for missing in &page.missing {
            entries.push((missing.first.0, json!({"type":"gap","reason":missing.reason,"first_position":missing.first,"last_position":missing.last})));
        }
        for entry in &page.records {
            let mut value = match &entry.event {
                HistoryEvent::Log { record } => {
                    if !crate::matches_filter(filter, record, &record.ancestors) {
                        continue;
                    }
                    let mut value = serde_json::to_value(record)?;
                    value["type"] = json!("log");
                    value
                }
                HistoryEvent::Gap { gap } => {
                    if gap.is_history_attribution_marker() {
                        continue;
                    }
                    let mut value = serde_json::to_value(gap)?;
                    value["type"] = json!("gap");
                    value
                }
            };
            value["cursor"] = json!(entry.cursor);
            value["boot_id"] = json!(entry.boot_id);
            value["recorded_at_ms"] = json!(entry.recorded_at_ms);
            value["startup"] = json!(entry.startup);
            entries.push((entry.cursor.position.0, value));
        }
        entries.sort_by_key(|(position, _)| *position);
        for (_, value) in entries {
            serde_json::to_writer(&mut *output, &value)?;
            output.write_all(b"\n")?;
        }
        output.flush()?;
        self.consumed().map_err(invalid)
    }
    pub fn range(&mut self, range: HistoryRange, durable: Option<HistoryCursor>) -> RpcResult<()> {
        if range.journal_id.is_empty()
            || range.oldest.0 == 0
            || range.oldest.0 > range.high_water.0.saturating_add(1)
        {
            return Err(RpcError::new(-32004, "Invalid history range"));
        }
        let cursor = durable
            .or_else(|| {
                self.cursor
                    .clone()
                    .filter(|c| c.journal_id == range.journal_id)
            })
            .unwrap_or(HistoryCursor {
                journal_id: range.journal_id.clone(),
                position: DecimalU64(match self.start {
                    Start::Now if self.cursor.is_none() => range.high_water.0,
                    _ => 0,
                }),
            });
        if cursor.journal_id != range.journal_id || cursor.position > range.high_water {
            return Err(RpcError::new(
                -32004,
                "History progress exceeds guest range",
            ));
        }
        self.cursor = Some(cursor);
        self.through = range.high_water.0;
        self.page = None;
        Ok(())
    }
    pub fn read_request(&self) -> HistoryRead {
        HistoryRead {
            after: self.cursor.clone(),
            through: Some(DecimalU64(self.through)),
            limit: 256,
        }
    }
    pub fn caught_up(&self) -> bool {
        self.cursor
            .as_ref()
            .is_some_and(|cursor| cursor.position.0 == self.through)
    }
    pub fn page(&mut self, page: &HistoryPage) -> RpcResult<()> {
        self.check_page(
            &page.range,
            &page.next,
            page.records.iter().map(|r| &r.cursor),
            &page.missing,
        )
    }
    /// Validate a negotiated page before committing revisions and continuous progress.
    pub fn attribution_page(&mut self, page: &AttributionHistoryPage) -> RpcResult<()> {
        page.validate_revisions()?;
        self.check_page(
            &page.range,
            &page.next,
            page.records.iter().map(|r| &r.cursor),
            &page.missing,
        )
    }
    fn check_page<'a>(
        &mut self,
        range: &HistoryRange,
        next: &HistoryCursor,
        records: impl Iterator<Item = &'a HistoryCursor>,
        missing: &[HistoryMissing],
    ) -> RpcResult<()> {
        let invalid = || RpcError::new(-32004, "History page does not cover the requested range");
        let cursor = self.cursor.as_ref().ok_or_else(invalid)?;
        if range.journal_id != cursor.journal_id
            || next.journal_id != cursor.journal_id
            || next.position.0 > self.through
            || next.position < cursor.position
            || range.high_water < next.position
        {
            return Err(invalid());
        }
        let mut intervals = Vec::new();
        for record in records {
            if record.journal_id != cursor.journal_id {
                return Err(invalid());
            }
            intervals.push((record.position.0, record.position.0));
        }
        for missing in missing {
            if missing.first > missing.last || missing.reason != "retention_expired" {
                return Err(invalid());
            }
            intervals.push((missing.first.0, missing.last.0));
        }
        intervals.sort_unstable();
        let mut last = cursor.position.0;
        for (first, end) in intervals {
            if last.checked_add(1) != Some(first) || end > next.position.0 {
                return Err(invalid());
            }
            last = end;
        }
        if last != next.position.0 || (!self.caught_up() && last == cursor.position.0) {
            return Err(invalid());
        }
        self.page = Some(next.clone());
        Ok(())
    }
    /// Called after the application commits or outputs the accepted page.
    pub fn consumed(&mut self) -> RpcResult<HistoryAck> {
        let cursor = self
            .page
            .take()
            .ok_or_else(|| RpcError::new(-32004, "History page is pending"))?;
        self.cursor = Some(cursor.clone());
        Ok(HistoryAck { cursor })
    }
}
