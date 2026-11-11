use super::*;

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// Metadata for the current connection. Reconnect starts a new negotiation.
#[derive(Clone, Debug, Default)]
pub struct PeerProtocol {
    endpoint: Option<EndpointInfo>,
    discovery: Option<Discovery>,
    incompatible: bool,
}
impl PeerProtocol {
    pub fn connect(&mut self) {
        *self = Self::default();
    }
    pub fn disconnect(&mut self) {
        *self = Self::default();
    }
    pub fn endpoint(&self) -> Option<&EndpointInfo> {
        self.endpoint.as_ref()
    }
    pub fn discovery(&self) -> Option<&Discovery> {
        self.discovery.as_ref()
    }
    pub fn accept_endpoint(&mut self, endpoint: EndpointInfo) -> RpcResult<()> {
        self.endpoint = Some(endpoint);
        self.validate()
    }
    pub fn accept_discovery(&mut self, discovery: Discovery) -> RpcResult<()> {
        self.discovery = Some(discovery);
        self.validate()
    }
    fn validate(&mut self) -> RpcResult<()> {
        self.incompatible = self.endpoint.as_ref().is_some_and(|info| {
            !valid_stable_version(&info.protocol_version)
                || info.protocol_version.split('.').next() != Some("1")
        }) || self.discovery.as_ref().is_some_and(|doc| {
            !valid_stable_version(&doc.info.version)
                || doc.info.version.split('.').next() != Some("1")
                || self
                    .endpoint
                    .as_ref()
                    .is_some_and(|info| info.protocol_version != doc.info.version)
        });
        if self.incompatible {
            Err(RpcError::new(-32003, "incompatible_protocol"))
        } else {
            Ok(())
        }
    }
    pub fn require(&self, method: Method) -> RpcResult<()> {
        if self.incompatible {
            return Err(RpcError::new(-32003, "incompatible_protocol"));
        }
        let (Some(info), Some(doc)) = (&self.endpoint, &self.discovery) else {
            return Err(RpcError::new(-32001, "not_ready"));
        };
        let receiver = match info.role {
            EndpointRole::Host => Receiver::Host,
            EndpointRole::Guest => Receiver::Guest,
        };
        if method.receiver().is_some_and(|role| role != receiver)
            || !doc.methods.iter().any(|entry| entry.name == method.name())
        {
            return Err(RpcError::new(-32002, "unsupported"));
        }
        Ok(())
    }
}
