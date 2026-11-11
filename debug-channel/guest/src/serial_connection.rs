use crate::state::serial;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use venera_debug_protocol::{v1::*, Envelope, Message};

const CAPACITY: usize = 64;

#[derive(Default)]
pub struct Connection {
    framer: JsonlFramer,
    peer: PeerProtocol,
    next_id: u64,
    endpoint_id: String,
    discovery_id: String,
    batches: BTreeMap<u64, RpcBatch>,
    reports: BTreeMap<String, (u64, Option<Correlation>)>,
    wrong_role: bool,
}

#[derive(Default)]
pub struct Received {
    pub output: Vec<Value>,
    pub reports: Vec<String>,
    pub parse_errors: u64,
}

pub fn line(value: &Value) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn discovery() -> RpcResult<Discovery> {
    serde_json::from_str(include_str!("../../protocol/v1/guest.openrpc.json"))
        .map_err(|_| RpcError::new(-32603, "Guest discovery document is unavailable"))
}

fn result(envelope: &Envelope) -> RpcResult<Value> {
    let bytes =
        serial::encode(envelope).map_err(|error| RpcError::new(-32603, error.to_string()))?;
    match serde_json::from_slice::<RpcResponse>(&bytes)
        .map_err(|error| RpcError::new(-32603, error.to_string()))?
    {
        RpcResponse::Success { result, .. } => Ok(result),
        RpcResponse::Error { error, .. } => Err(error),
    }
}

impl Connection {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn disconnect(&mut self) {
        self.peer.disconnect();
        self.framer.clear();
        self.batches.clear();
        self.reports.clear();
        self.endpoint_id.clear();
        self.discovery_id.clear();
        self.wrong_role = false;
    }

    pub fn connect(&mut self) -> [Value; 2] {
        self.disconnect();
        self.peer.connect();
        self.endpoint_id = format!("guest-endpoint-{}", self.id());
        self.discovery_id = format!("guest-discovery-{}", self.id());
        [
            json!({"jsonrpc":"2.0","id":self.endpoint_id,"method":"endpoint.info","params":{}}),
            json!({"jsonrpc":"2.0","id":self.discovery_id,"method":"rpc.discover","params":{}}),
        ]
    }

    fn accept(&mut self, response: RpcResponse) {
        if let RpcResponse::Success {
            id: RpcId::String(id),
            result,
            ..
        } = response
        {
            if id == self.endpoint_id && !self.endpoint_id.is_empty() {
                if let Ok(info) = serde_json::from_value::<EndpointInfo>(result) {
                    self.wrong_role = info.role != EndpointRole::Host;
                    let _ = self.peer.accept_endpoint(info);
                }
                self.endpoint_id.clear();
            } else if id == self.discovery_id && !self.discovery_id.is_empty() {
                if let Ok(document) = serde_json::from_value::<Discovery>(result) {
                    let _ = self.peer.accept_discovery(document);
                }
                self.discovery_id.clear();
            }
        }
    }

    fn ready(&self) -> RpcResult<()> {
        if self.wrong_role {
            return Err(RpcError::new(-32003, "incompatible_protocol"));
        }
        // The peer is a host receiver; endpoint.info is its shared readiness capability.
        self.peer.require(Method::EndpointInfo)
    }

    pub fn receive(
        &mut self,
        bytes: &[u8],
        mut control: impl FnMut(Message) -> Option<Envelope>,
    ) -> Received {
        let mut received = Received::default();
        for frame in self.framer.push(bytes) {
            let frame = match frame {
                Ok(frame) => frame,
                Err(error) => {
                    received.parse_errors += 1;
                    received.output.push(json!(RpcResponse::Error {
                        jsonrpc: Version::V2,
                        id: RpcId::Null,
                        error,
                    }));
                    continue;
                }
            };
            let RpcPacket { items, mut batch } = RpcPacket::decode(frame.bytes(), Receiver::Guest);
            let batch_id = self.id();
            let mut pending = false;
            for item in items {
                let request = match item {
                    PacketItem::Response(response) => {
                        self.accept(response);
                        continue;
                    }
                    PacketItem::Rejected(_) => {
                        received.parse_errors += 1;
                        continue;
                    }
                    PacketItem::Request(request) => request,
                };
                let correlation = request.correlation;
                let value = match request.incoming {
                    Incoming::EndpointInfo { .. } => Ok(json!(EndpointInfo::new(
                        EndpointRole::Guest,
                        env!("CARGO_PKG_VERSION"),
                    ))),
                    Incoming::Discover { .. } => discovery().and_then(|doc| {
                        serde_json::to_value(doc).map_err(|e| RpcError::new(-32603, e.to_string()))
                    }),
                    incoming => {
                        let allowed = self.ready();
                        if let Err(error) = allowed {
                            Err(error)
                        } else if self.reports.len() >= CAPACITY {
                            Err(RpcError::new(
                                -32000,
                                "Control queue is busy; retry the request",
                            ))
                        } else {
                            let internal_id = format!("guest-report-{}", self.id());
                            let message = match incoming {
                                Incoming::StatusGet { .. } => Message::StatusGet {
                                    request_id: internal_id,
                                },
                                Incoming::FilterSet { params, .. } => Message::FilterSet {
                                    request_id: internal_id,
                                    filter: serial::filter_legacy(params.filter),
                                    duration_ms: params.duration_ms.map(|ms| ms.0),
                                },
                                Incoming::FilterReset { .. } => Message::FilterReset {
                                    request_id: internal_id,
                                },
                                Incoming::SystemInfoGet { .. } => {
                                    if !pending && self.batches.len() >= CAPACITY {
                                        if let Some(slot) = correlation {
                                            let _ = batch.complete(
                                                slot,
                                                Err(RpcError::new(
                                                    -32000,
                                                    "Control queue is busy; retry the request",
                                                )),
                                            );
                                        }
                                        continue;
                                    }
                                    self.reports
                                        .insert(internal_id.clone(), (batch_id, correlation));
                                    received.reports.push(internal_id);
                                    pending = true;
                                    continue;
                                }
                                _ => continue,
                            };
                            control(message).as_ref().map(result).unwrap_or_else(|| {
                                Err(RpcError::new(-32603, "Control result is unavailable"))
                            })
                        }
                    }
                };
                if let Some(slot) = correlation {
                    let _ = batch.complete(slot, value);
                }
            }
            if pending {
                self.batches.insert(batch_id, batch);
            } else if let Some(value) = batch.take_response() {
                received.output.push(value);
            }
        }
        received
    }

    pub fn report(&mut self, envelope: &Envelope) -> Option<Value> {
        let Message::SystemInfoResult { result: report } = &envelope.body else {
            return None;
        };
        let (batch_id, correlation) = self.reports.remove(&report.request_id)?;
        let batch = self.batches.get_mut(&batch_id)?;
        if let Some(slot) = correlation {
            let _ = batch.complete(slot, result(envelope));
        }
        if !self.reports.values().any(|(id, _)| *id == batch_id) {
            let value = batch.take_response();
            self.batches.remove(&batch_id);
            value
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    fn receive(connection: &mut Connection, state: &mut State, value: Value) -> Received {
        connection.receive(&line(&value).unwrap(), |message| {
            state.control(message, None)
        })
    }

    fn negotiate(connection: &mut Connection, state: &mut State, version: &str) {
        let [endpoint, discover] = connection.connect();
        let host: Value =
            serde_json::from_str(include_str!("../../protocol/v1/host.openrpc.json")).unwrap();
        let received = receive(
            connection,
            state,
            json!([
                {"jsonrpc":"2.0","id":endpoint["id"],"result":{"role":"host","software_version":"host-7","protocol_version":version}},
                {"jsonrpc":"2.0","id":discover["id"],"result":host}
            ]),
        );
        assert_eq!(received.output, Vec::<Value>::new());
    }

    fn report(id: String) -> Envelope {
        Envelope::new(Message::SystemInfoResult {
            result: venera_debug_protocol::SystemInfoResult {
                request_id: id,
                ok: true,
                collected_at: "2026-11-11T00:00:00Z".into(),
                session_id: Some(2),
                sections: vec![],
                error: None,
            },
        })
    }

    #[test]
    fn endpoint_and_discovery_are_available_at_connection_start() {
        let mut connection = Connection::default();
        let mut state = State::new("run".into(), true);
        let output = receive(
            &mut connection,
            &mut state,
            json!({
                "jsonrpc":"2.0","id":17,"method":"endpoint.info"
            }),
        )
        .output;
        assert_eq!(
            output,
            vec![json!({"jsonrpc":"2.0","id":17,"result":{
                "role":"guest","software_version":env!("CARGO_PKG_VERSION"),"protocol_version":"1.0.0"
            }})]
        );
        let output = receive(
            &mut connection,
            &mut state,
            json!({
                "jsonrpc":"2.0","id":null,"method":"rpc.discover"
            }),
        )
        .output;
        assert_eq!(output[0]["id"], Value::Null);
        assert_eq!(output[0]["result"]["openrpc"], "1.4.1");
        assert_eq!(
            output[0]["result"]["info"],
            json!({"title":"Venera guest","version":"1.0.0"})
        );
        assert_eq!(output[0]["result"]["methods"][0]["name"], "endpoint.info");
    }

    #[test]
    fn negotiation_controls_execution_and_reconnect_resets_readiness() {
        let mut connection = Connection::default();
        let mut state = State::new("run".into(), true);
        let reset = json!({"jsonrpc":"2.0","id":"reset","method":"filter.reset"});
        let output = receive(&mut connection, &mut state, reset.clone()).output;
        assert_eq!(
            output,
            vec![
                json!({"jsonrpc":"2.0","id":"reset","error":{"code":-32001,"message":"not_ready"}})
            ]
        );
        assert_eq!(state.current().revision, 0);
        negotiate(&mut connection, &mut state, "2.0.0");
        let output = receive(&mut connection, &mut state, reset.clone()).output;
        assert_eq!(
            output[0]["error"],
            json!({"code":-32003,"message":"incompatible_protocol"})
        );
        assert_eq!(state.current().revision, 0);
        negotiate(&mut connection, &mut state, "1.0.0");
        let output = receive(&mut connection, &mut state, reset.clone()).output;
        assert_eq!(output[0]["result"]["revision"], "1");
        assert_eq!(state.current().revision, 1);
        connection.disconnect();
        let output = receive(&mut connection, &mut state, reset).output;
        assert_eq!(output[0]["error"]["message"], "not_ready");
        assert_eq!(state.current().revision, 1);
    }

    #[test]
    fn calls_echo_numeric_null_and_string_ids_and_notifications_execute_silently() {
        let mut connection = Connection::default();
        let mut state = State::new("run".into(), true);
        negotiate(&mut connection, &mut state, "1.0.0");
        for id in [json!(42), Value::Null, json!("status-7")] {
            let output = receive(
                &mut connection,
                &mut state,
                json!({
                    "jsonrpc":"2.0","id":id,"method":"status.get"
                }),
            )
            .output;
            assert_eq!(
                output,
                vec![json!({"jsonrpc":"2.0","id":id,"result":{
                    "forwarding_enabled":true,"revision":"0","effective_filter":{"components":[],"levels":[],"targets":[]},
                    "expires_at":null,"health":{"driver":"probing","controller":"probing","port":"waiting","host":"waiting","dbwin":"starting","etw":"starting","sessions":[]},
                    "loss":{"queue_dropped":"0","parse_errors":"0"}
                }})]
            );
        }
        let received = receive(
            &mut connection,
            &mut state,
            json!([
                {"jsonrpc":"2.0","method":"filter.reset"},
                {"jsonrpc":"2.0","method":"missing"},
                {"jsonrpc":"2.0","method":"filter.set","params":{"filter":{}}}
            ]),
        );
        assert_eq!(state.current().revision, 1);
        assert_eq!(received.output, Vec::<Value>::new());
        assert_eq!(received.reports, Vec::<String>::new());
    }

    #[test]
    fn mixed_batch_waits_for_report_and_aggregates_in_input_order() {
        let mut connection = Connection::default();
        let mut state = State::new("run".into(), true);
        negotiate(&mut connection, &mut state, "1.0.0");
        let received = receive(
            &mut connection,
            &mut state,
            json!([
                {"jsonrpc":"2.0","id":null,"method":"system.info.get"},
                {"jsonrpc":"2.0","id":7,"method":"status.get"},
                {"jsonrpc":"2.0","id":"bad","method":"filter.set","params":{"filter":{}}},
                {"jsonrpc":"2.0","method":"missing"}
            ]),
        );
        assert_eq!(received.output, Vec::<Value>::new());
        assert_eq!(received.reports.len(), 1);
        let value = connection
            .report(&report(received.reports[0].clone()))
            .unwrap();
        assert_eq!(
            value[0],
            json!({"jsonrpc":"2.0","id":null,"result":{
                "collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[]
            }})
        );
        assert_eq!(value[1]["id"], 7);
        assert_eq!(value[1]["result"]["revision"], "0");
        assert_eq!(
            value[2],
            json!({"jsonrpc":"2.0","id":"bad","error":{"code":-32602,"message":"Invalid params"}})
        );
        assert_eq!(value.as_array().unwrap().len(), 3);
        assert_eq!(
            connection.report(&report(received.reports[0].clone())),
            None
        );
    }

    #[test]
    fn asynchronous_notifications_and_stale_reports_produce_no_response() {
        let mut connection = Connection::default();
        let mut state = State::new("run".into(), true);
        negotiate(&mut connection, &mut state, "1.0.0");
        let notification = receive(
            &mut connection,
            &mut state,
            json!({"jsonrpc":"2.0","method":"system.info.get"}),
        );
        assert_eq!(notification.output, Vec::<Value>::new());
        assert_eq!(notification.reports.len(), 1);
        assert_eq!(
            connection.report(&report(notification.reports[0].clone())),
            None
        );
        let pending = receive(
            &mut connection,
            &mut state,
            json!({"jsonrpc":"2.0","id":"old","method":"system.info.get"}),
        );
        negotiate(&mut connection, &mut state, "1.0.0");
        let next = receive(
            &mut connection,
            &mut state,
            json!({"jsonrpc":"2.0","id":"new","method":"system.info.get"}),
        );
        assert_eq!(connection.report(&report(pending.reports[0].clone())), None);
        assert_eq!(
            connection.report(&report(next.reports[0].clone())).unwrap(),
            json!({
                "jsonrpc":"2.0","id":"new","result":{"collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[]}
            })
        );
    }

    #[test]
    fn framing_bounds_partial_lines_and_recovers_after_parse_errors() {
        let mut connection = Connection::default();
        let received = connection.receive(&vec![b'x'; MAX_FRAME_BYTES + 100], |_| None);
        assert_eq!(connection.framer.buffered_len(), 0);
        assert_eq!(received.parse_errors, 1);
        assert_eq!(
            received.output,
            vec![
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Frame exceeds 1 MiB"}})
            ]
        );
        let received = connection.receive(b"tail\n{\n", |_| None);
        assert_eq!(received.parse_errors, 1);
        assert_eq!(
            received.output,
            vec![
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
            ]
        );
        let mut state = State::new("run".into(), true);
        let output = receive(
            &mut connection,
            &mut state,
            json!({"jsonrpc":"2.0","id":1,"method":"endpoint.info"}),
        )
        .output;
        assert_eq!(output[0]["result"]["role"], "guest");
        assert_eq!(output[0]["id"], 1);
    }

    #[test]
    fn empty_batch_and_unknown_call_return_standard_errors() {
        let mut connection = Connection::default();
        let mut state = State::new("run".into(), true);
        assert_eq!(
            receive(&mut connection, &mut state, json!([])).output,
            vec![json!({
                "jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}
            })]
        );
        assert_eq!(
            receive(
                &mut connection,
                &mut state,
                json!({"jsonrpc":"2.0","id":8,"method":"missing"})
            )
            .output,
            vec![json!({
                "jsonrpc":"2.0","id":8,"error":{"code":-32601,"message":"Method not found"}
            })]
        );
    }

    #[test]
    fn peer_role_and_discovered_capabilities_gate_controls() {
        let mut connection = Connection::default();
        let mut state = State::new("run".into(), true);
        let [endpoint, discover] = connection.connect();
        let mut host: Value =
            serde_json::from_str(include_str!("../../protocol/v1/host.openrpc.json")).unwrap();
        host["methods"] = json!([]);
        receive(
            &mut connection,
            &mut state,
            json!([
                {"jsonrpc":"2.0","id":endpoint["id"],"result":{"role":"host","software_version":"host-7","protocol_version":"1.0.0"}},
                {"jsonrpc":"2.0","id":discover["id"],"result":host}
            ]),
        );
        let reset = json!({"jsonrpc":"2.0","id":19,"method":"filter.reset"});
        let output = receive(&mut connection, &mut state, reset.clone()).output;
        assert_eq!(
            output,
            vec![json!({"jsonrpc":"2.0","id":19,"error":{"code":-32002,"message":"unsupported"}})]
        );
        assert_eq!(state.current().revision, 0);

        let [endpoint, discover] = connection.connect();
        let host: Value =
            serde_json::from_str(include_str!("../../protocol/v1/host.openrpc.json")).unwrap();
        receive(
            &mut connection,
            &mut state,
            json!([
                {"jsonrpc":"2.0","id":endpoint["id"],"result":{"role":"guest","software_version":"guest-7","protocol_version":"1.0.0"}},
                {"jsonrpc":"2.0","id":discover["id"],"result":host}
            ]),
        );
        let output = receive(&mut connection, &mut state, reset).output;
        assert_eq!(
            output,
            vec![
                json!({"jsonrpc":"2.0","id":19,"error":{"code":-32003,"message":"incompatible_protocol"}})
            ]
        );
        assert_eq!(state.current().revision, 0);
    }

    #[test]
    fn report_capacity_returns_busy_with_original_id_and_recovers_after_completion() {
        let mut connection = Connection::default();
        let mut state = State::new("run".into(), true);
        negotiate(&mut connection, &mut state, "1.0.0");
        let mut reports = Vec::new();
        for id in 0..64 {
            let received = receive(
                &mut connection,
                &mut state,
                json!({"jsonrpc":"2.0","id":id,"method":"system.info.get"}),
            );
            assert_eq!(received.output, Vec::<Value>::new());
            reports.extend(received.reports);
        }
        assert_eq!(reports.len(), 64);
        let received = receive(
            &mut connection,
            &mut state,
            json!({"jsonrpc":"2.0","id":null,"method":"system.info.get"}),
        );
        assert_eq!(
            received.output,
            vec![
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":"Control queue is busy; retry the request"}})
            ]
        );
        assert_eq!(
            connection.report(&report(reports.remove(0))).unwrap()["id"],
            0
        );
        let next = receive(
            &mut connection,
            &mut state,
            json!({"jsonrpc":"2.0","id":"next","method":"system.info.get"}),
        );
        assert_eq!(next.reports.len(), 1);
        assert_eq!(
            connection.report(&report(next.reports[0].clone())).unwrap()["id"],
            "next"
        );
    }

    #[test]
    fn filter_calls_convert_decimal_fields_and_return_the_effective_snapshot() {
        let mut connection = Connection::default();
        let mut state = State::new("run".into(), true);
        negotiate(&mut connection, &mut state, "1.0.0");
        let received = receive(
            &mut connection,
            &mut state,
            json!({
                "jsonrpc":"2.0","id":null,"method":"filter.set","params":{
                    "filter":{"components":["venus"],"levels":["info"],"targets":[{
                        "kind":"process_instance","pid":4294967295u32,"start_time_filetime":"18446744073709551615","include_descendants":true
                    }]},"duration_ms":"9"
                }
            }),
        );
        assert_eq!(
            received.output[0]["result"]["effective_filter"],
            json!({
                "components":["venus"],"levels":["info"],"targets":[{"kind":"process_instance","pid":4294967295u32,"start_time_filetime":"18446744073709551615","include_descendants":true}]
            })
        );
        assert_eq!(received.output[0]["result"]["revision"], "1");
        assert_eq!(state.current().revision, 1);
    }
}
