use super::*;
use serde_json::{json, Number};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    String(String),
    Number(Number),
    Null,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RpcResponse {
    Success {
        jsonrpc: Version,
        id: RpcId,
        result: Value,
    },
    Error {
        jsonrpc: Version,
        id: RpcId,
        error: RpcError,
    },
}
impl<'de> Deserialize<'de> for RpcResponse {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(de)?;
        parse_response(&value).map_err(serde::de::Error::custom)
    }
}
fn parse_response(value: &Value) -> Result<RpcResponse, &'static str> {
    if value["jsonrpc"] != "2.0" {
        return Err("expected JSON-RPC 2.0");
    }
    let id = value.get("id").ok_or("expected response ID")?;
    let id: RpcId = serde_json::from_value(id.clone()).map_err(|_| "invalid response ID")?;
    match (value.get("result"), value.get("error")) {
        (Some(result), None) => Ok(RpcResponse::Success {
            jsonrpc: Version::V2,
            id,
            result: result.clone(),
        }),
        (None, Some(error)) => Ok(RpcResponse::Error {
            jsonrpc: Version::V2,
            id,
            error: serde_json::from_value(error.clone()).map_err(|_| "invalid error object")?,
        }),
        _ => Err("expected exactly one result or error"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Correlation(usize);

#[derive(Debug)]
pub struct RpcRequest {
    pub incoming: Incoming,
    pub id: Option<RpcId>,
    /// None denotes a notification. Execute it without completing a response slot.
    pub correlation: Option<Correlation>,
}
impl RpcRequest {
    pub fn is_notification(&self) -> bool {
        self.correlation.is_none()
    }
    pub fn dispatch(
        self,
        handler: &mut impl Handler,
    ) -> Result<(Option<Correlation>, RpcResult<Value>), serde_json::Error> {
        let result = match self.incoming.dispatch(handler)? {
            Some(Response::Success { result, .. }) => Ok(result),
            Some(Response::Error { error, .. }) => Err(error),
            None => Ok(Value::Null),
        };
        Ok((self.correlation, result))
    }
}

#[derive(Debug)]
pub enum PacketItem {
    Request(RpcRequest),
    Response(RpcResponse),
    /// A local error already queued for this packet's response.
    Rejected(RpcResponse),
}

#[derive(Debug)]
struct Slot {
    id: RpcId,
    response: Option<RpcResponse>,
}
/// Connection-local response aggregation; completion order preserves packet order.
#[derive(Debug)]
pub struct RpcBatch {
    array: bool,
    slots: Vec<Slot>,
    emitted: bool,
}
impl RpcBatch {
    fn slot(&mut self, id: RpcId, error: Option<RpcError>) -> Correlation {
        let correlation = Correlation(self.slots.len());
        let response = error.map(|error| RpcResponse::Error {
            jsonrpc: Version::V2,
            id: id.clone(),
            error,
        });
        self.slots.push(Slot { id, response });
        correlation
    }
    pub fn complete(
        &mut self,
        correlation: Correlation,
        result: RpcResult<Value>,
    ) -> RpcResult<()> {
        let slot = self
            .slots
            .get_mut(correlation.0)
            .ok_or_else(|| RpcError::new(-32603, "Unknown response slot"))?;
        if slot.response.is_some() || self.emitted {
            return Err(RpcError::new(-32603, "Response slot already completed"));
        }
        slot.response = Some(match result {
            Ok(result) => RpcResponse::Success {
                jsonrpc: Version::V2,
                id: slot.id.clone(),
                result,
            },
            Err(error) => RpcResponse::Error {
                jsonrpc: Version::V2,
                id: slot.id.clone(),
                error,
            },
        });
        Ok(())
    }
    pub fn is_complete(&self) -> bool {
        self.emitted || self.slots.iter().all(|slot| slot.response.is_some())
    }
    /// Returns one JSON value after all calls finish; notification packets produce None.
    pub fn take_response(&mut self) -> Option<Value> {
        if self.emitted || !self.is_complete() {
            return None;
        }
        self.emitted = true;
        if self.slots.is_empty() {
            return None;
        }
        let values: Vec<_> = self
            .slots
            .iter_mut()
            .map(|slot| serde_json::to_value(slot.response.take().unwrap()).unwrap())
            .collect();
        if self.array {
            Some(Value::Array(values))
        } else {
            values.into_iter().next()
        }
    }
}

#[derive(Debug)]
pub struct RpcPacket {
    pub items: Vec<PacketItem>,
    pub batch: RpcBatch,
}
impl RpcPacket {
    /// Parses one JSONL payload, including inbound responses and batch requests.
    pub fn decode(bytes: &[u8], receiver: Receiver) -> Self {
        let mut packet = Self {
            items: Vec::new(),
            batch: RpcBatch {
                array: false,
                slots: Vec::new(),
                emitted: false,
            },
        };
        let value = if bytes.len() <= MAX_FRAME_BYTES {
            serde_json::from_slice::<Value>(bytes).ok()
        } else {
            None
        };
        match value {
            None => packet.reject(RpcId::Null, RpcError::new(-32700, "Parse error")),
            Some(Value::Array(values)) if !values.is_empty() => {
                packet.batch.array = true;
                for value in values {
                    packet.item(value, receiver);
                }
            }
            Some(value) => packet.item(value, receiver),
        }
        packet
    }
    fn reject(&mut self, id: RpcId, error: RpcError) {
        self.items.push(PacketItem::Rejected(RpcResponse::Error {
            jsonrpc: Version::V2,
            id: id.clone(),
            error: error.clone(),
        }));
        self.batch.slot(id, Some(error));
    }
    fn invalid(&mut self) {
        self.reject(RpcId::Null, RpcError::new(-32600, "Invalid Request"));
    }
    fn item(&mut self, value: Value, receiver: Receiver) {
        let Some(object) = value.as_object() else {
            self.invalid();
            return;
        };
        if !object.contains_key("method")
            && (object.contains_key("result") || object.contains_key("error"))
        {
            match parse_response(&value) {
                Ok(response) => self.items.push(PacketItem::Response(response)),
                Err(_) => self.invalid(),
            }
            return;
        }
        if value["jsonrpc"] != "2.0"
            || !value["method"].is_string()
            || object.contains_key("result")
            || object.contains_key("error")
        {
            self.invalid();
            return;
        }
        let id = match object.get("id") {
            None => None,
            Some(value) => match serde_json::from_value::<RpcId>(value.clone()) {
                Ok(id) => Some(id),
                Err(_) => {
                    self.invalid();
                    return;
                }
            },
        };
        let name = value["method"].as_str().unwrap();
        let method = Method::from_name(name)
            .filter(|method| method.receiver().is_none_or(|role| role == receiver));
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
        let decoded = match method {
            None => Err(RpcError::new(-32601, "Method not found")),
            Some(method) if params.is_object() => {
                let mut wire = json!({"jsonrpc":"2.0", "method":name, "params":params});
                if method.is_call() {
                    wire["id"] = match &id {
                        Some(RpcId::String(id)) => json!(id),
                        _ => json!("packet"),
                    };
                }
                decode(&wire.to_string()).map_err(|_| RpcError::new(-32602, "Invalid params"))
            }
            Some(_) => Err(RpcError::new(-32602, "Invalid params")),
        };
        match decoded {
            Ok(incoming) => {
                let correlation = id.clone().map(|id| self.batch.slot(id, None));
                self.items.push(PacketItem::Request(RpcRequest {
                    incoming,
                    id,
                    correlation,
                }));
            }
            Err(error) => {
                if let Some(id) = id {
                    self.reject(id, error);
                }
            }
        }
    }
}
