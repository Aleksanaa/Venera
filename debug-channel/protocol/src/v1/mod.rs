//! Venera 1.0.0 typed messages and method registration.
mod types;
pub use types::*;
mod packet;
pub use packet::*;
mod peer;
pub use peer::*;
mod framer;
pub use framer::*;
pub const PROTOCOL_VERSION: &str = "1.0.0";
#[cfg(feature = "schema")]
pub mod schema;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Version {
    #[serde(rename = "2.0")]
    V2,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}
pub type RpcResult<T> = Result<T, RpcError>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Response<T> {
    Success {
        jsonrpc: Version,
        id: String,
        result: T,
    },
    Error {
        jsonrpc: Version,
        id: String,
        error: RpcError,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receiver {
    Guest,
    Host,
}

/// One declaration connects decoding, typed handlers, dispatch and discovery.
macro_rules! methods {
    (
        calls { $( $call:ident, $handler:ident, $name:literal, $role:ident, $params:ty => $result:ty; )* }
        notifications { $( $event:ident, $event_handler:ident, $event_name:literal, $event_params:ty; )* }
    ) => {
        #[derive(Clone, Debug, PartialEq)]
        pub enum Incoming {
            $( $call { id: String, params: $params }, )*
            $( $event($event_params), )*
        }
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Method {
            $( $call, )*
            $( $event, )*
        }
        impl Method {
            pub fn name(self) -> &'static str {
                match self { $( Self::$call => $name, )* $( Self::$event => $event_name, )* }
            }
            pub fn receiver(self) -> Option<Receiver> {
                match self { $( Self::$call => methods!(@role $role), )* $( Self::$event => Some(Receiver::Host), )* }
            }
            pub fn from_name(name: &str) -> Option<Self> {
                match name { $( $name => Some(Self::$call), )* $( $event_name => Some(Self::$event), )* _ => None }
            }
            pub fn is_call(self) -> bool {
                match self { $( Self::$call => true, )* $( Self::$event => false, )* }
            }
        }
        pub trait Handler {
            $( fn $handler(&mut self, params: $params) -> RpcResult<$result>; )*
            $( fn $event_handler(&mut self, params: $event_params); )*
        }
        impl Incoming {
            pub fn method(&self) -> &'static str {
                match self { $( Self::$call { .. } => $name, )* $( Self::$event(_) => $event_name, )* }
            }
            pub fn receiver(&self) -> Option<Receiver> {
                match self {
                    $( Self::$call { .. } => methods!(@role $role), )*
                    $( Self::$event(_) => Some(Receiver::Host), )*
                }
            }
            pub fn dispatch(self, handler: &mut impl Handler) -> Result<Option<Response<Value>>, serde_json::Error> {
                match self {
                    $( Self::$call { id, params } => Ok(Some(match handler.$handler(params) {
                        Ok(result) => Response::Success { jsonrpc: Version::V2, id, result: serde_json::to_value(result)? },
                        Err(error) => Response::Error { jsonrpc: Version::V2, id, error },
                    })), )*
                    $( Self::$event(params) => { handler.$event_handler(params); Ok(None) }, )*
                }
            }
        }
        pub fn decode(text: &str) -> Result<Incoming, String> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Wire { jsonrpc: Version, #[serde(default, deserialize_with = "request_id")] id: Option<String>, method: String, params: Value }
            let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
            let _ = wire.jsonrpc;
            if !wire.params.is_object() { return Err("expected named parameters".into()); }
            match wire.method.as_str() {
                $( $name => Ok(Incoming::$call {
                    id: wire.id.ok_or("expected a string request ID")?,
                    params: serde_json::from_value(wire.params).map_err(|e| e.to_string())?,
                }), )*
                $( $event_name => {
                    if wire.id.is_some() { return Err("expected a notification".into()); }
                    Ok(Incoming::$event(serde_json::from_value(wire.params).map_err(|e| e.to_string())?))
                }, )*
                _ => Err("unknown method".into()),
            }
        }
        #[cfg(feature = "schema")]
        fn declarations(receiver: Receiver, input: &mut schemars::SchemaGenerator, output: &mut schemars::SchemaGenerator) -> Vec<schema::Method> {
            let mut methods = Vec::new();
            $( if methods!(@role $role).is_none_or(|role| role == receiver) {
                methods.push(schema::Method { name: $name, params: input.subschema_for::<$params>().to_value(), result: Some(output.subschema_for::<$result>().to_value()) });
            } )*
            if receiver == Receiver::Host {
                $( methods.push(schema::Method { name: $event_name, params: input.subschema_for::<$event_params>().to_value(), result: None }); )*
            }
            methods
        }
    };
    (@role Guest) => { Some(Receiver::Guest) };
    (@role Both) => { None::<Receiver> };
}

fn request_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

methods! {
    calls {
        EndpointInfo, endpoint_info, "endpoint.info", Both, Empty => EndpointInfo;
        StatusGet, status_get, "status.get", Guest, Empty => Status;
        FilterSet, filter_set, "filter.set", Guest, FilterSet => Status;
        FilterReset, filter_reset, "filter.reset", Guest, Empty => Status;
        SystemInfoGet, system_info_get, "system.info.get", Guest, Empty => SystemInfo;
        Discover, discover, "rpc.discover", Both, Empty => Discovery;
    }
    notifications {
        StatusChanged, status_changed, "status.changed", Status;
        Log, log, "log", LogRecord;
        Gap, gap, "gap", Gap;
    }
}
