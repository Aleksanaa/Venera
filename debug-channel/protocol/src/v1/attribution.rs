use super::*;

/// The existing gap envelope carries a zero-count cursor marker in compatibility pages.
pub const HISTORY_ATTRIBUTION_MARKER: &str = "attribution_revision";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HistoryAttribution {
    pub process: ProcessIdentity,
    pub ancestors: Vec<ProcessIdentity>,
}

impl HistoryAttribution {
    pub fn apply_to(&self, record: &mut LogRecord) {
        record.process = Some(self.process.clone());
        record.ancestors.clone_from(&self.ancestors);
    }

    pub fn is_resolved(&self) -> bool {
        std::iter::once(&self.process)
            .chain(&self.ancestors)
            .all(|identity| {
                identity
                    .start_time_filetime
                    .as_deref()
                    .is_some_and(|time| time.parse::<DecimalU64>().is_ok_and(|time| time.0 != 0))
            })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttributionHistoryEvent {
    Log {
        record: LogRecord,
    },
    Gap {
        gap: Gap,
    },
    AttributionRevision {
        target: HistoryCursor,
        attribution: HistoryAttribution,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AttributionHistoryRecord {
    pub cursor: HistoryCursor,
    pub boot_id: String,
    pub recorded_at_ms: DecimalU64,
    pub startup: bool,
    pub event: AttributionHistoryEvent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AttributionHistoryPage {
    pub range: HistoryRange,
    pub records: Vec<AttributionHistoryRecord>,
    pub missing: Vec<HistoryMissing>,
    pub next: HistoryCursor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryEncoding {
    #[default]
    Compatibility,
    Attribution,
}

impl HistoryEncoding {
    /// Select the history encoding for a request using this connection's handshake.
    pub fn negotiate(local: &EndpointInfo, peer: &PeerProtocol) -> Self {
        let advertised = |info: &EndpointInfo| {
            info.capabilities
                .iter()
                .any(|cap| cap == HISTORY_ATTRIBUTION_CAPABILITY)
        };
        let has_capable_peer = peer
            .endpoint()
            .is_some_and(|remote| local.role != remote.role && advertised(remote));
        let required_method = match local.role {
            EndpointRole::Guest => Method::EndpointInfo,
            EndpointRole::Host => Method::HistoryRead,
        };
        if advertised(local) && has_capable_peer && peer.require(required_method).is_ok() {
            Self::Attribution
        } else {
            Self::Compatibility
        }
    }

    /// Encode a page in the negotiated format. The compatibility resolver supplies
    /// the current attribution for a retained log cursor from the server's read view.
    /// `Ok(None)` preserves the archived attribution. Measure the frame after encoding.
    pub fn encode_page(
        self,
        page: AttributionHistoryPage,
        resolve: impl FnMut(&HistoryCursor) -> RpcResult<Option<HistoryAttribution>>,
    ) -> RpcResult<Value> {
        page.validate_revisions()?;
        let value = match self {
            Self::Attribution => serde_json::to_value(page),
            Self::Compatibility => serde_json::to_value(page.into_compatibility(resolve)?),
        };
        value.map_err(|error| RpcError::new(-32603, error.to_string()))
    }
}

impl AttributionHistoryPage {
    pub fn validate_revisions(&self) -> RpcResult<()> {
        for entry in &self.records {
            if let AttributionHistoryEvent::AttributionRevision {
                target,
                attribution,
            } = &entry.event
            {
                if target.journal_id.is_empty()
                    || target.journal_id != self.range.journal_id
                    || target.journal_id != entry.cursor.journal_id
                    || target.position.0 == 0
                    || target.position >= entry.cursor.position
                    || !attribution.is_resolved()
                {
                    return Err(RpcError::new(
                        -32004,
                        "Invalid history attribution revision",
                    ));
                }
            }
        }
        Ok(())
    }

    fn into_compatibility(
        self,
        mut resolve: impl FnMut(&HistoryCursor) -> RpcResult<Option<HistoryAttribution>>,
    ) -> RpcResult<HistoryPage> {
        let mut records = Vec::with_capacity(self.records.len());
        for entry in self.records {
            let event = match entry.event {
                AttributionHistoryEvent::Log { mut record } => {
                    if let Some(attribution) = resolve(&entry.cursor)? {
                        if !attribution.is_resolved() {
                            return Err(RpcError::new(
                                -32603,
                                "Invalid history attribution snapshot",
                            ));
                        }
                        attribution.apply_to(&mut record);
                    }
                    HistoryEvent::Log { record }
                }
                AttributionHistoryEvent::Gap { gap } => HistoryEvent::Gap { gap },
                AttributionHistoryEvent::AttributionRevision { .. } => HistoryEvent::Gap {
                    gap: Gap {
                        stream_id: self.range.journal_id.clone(),
                        reason: HISTORY_ATTRIBUTION_MARKER.into(),
                        count: Some(DecimalU64(0)),
                        first_seq: None,
                        last_seq: None,
                    },
                },
            };
            records.push(HistoryRecord {
                cursor: entry.cursor,
                boot_id: entry.boot_id,
                recorded_at_ms: entry.recorded_at_ms,
                startup: entry.startup,
                event,
            });
        }
        Ok(HistoryPage {
            range: self.range,
            records,
            missing: self.missing,
            next: self.next,
        })
    }
}

impl Gap {
    pub fn is_history_attribution_marker(&self) -> bool {
        self.reason == HISTORY_ATTRIBUTION_MARKER
            && self.count == Some(DecimalU64(0))
            && self.first_seq.is_none()
            && self.last_seq.is_none()
    }
}
