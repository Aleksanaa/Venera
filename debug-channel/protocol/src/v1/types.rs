use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EndpointRole {
    Host,
    Guest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EndpointInfo {
    pub role: EndpointRole,
    pub software_version: String,
    #[serde(deserialize_with = "stable_version")]
    #[cfg_attr(feature = "schema", schemars(schema_with = "version_schema"))]
    pub protocol_version: String,
}
impl EndpointInfo {
    pub fn new(role: EndpointRole, software_version: impl Into<String>) -> Self {
        Self {
            role,
            software_version: software_version.into(),
            protocol_version: super::PROTOCOL_VERSION.into(),
        }
    }
}

pub(super) fn valid_stable_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|c| c.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}
fn stable_version<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let version = String::deserialize(de)?;
    if valid_stable_version(&version) {
        Ok(version)
    } else {
        Err(D::Error::custom(
            "expected a stable three-part protocol version",
        ))
    }
}
#[cfg(feature = "schema")]
fn version_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"string", "pattern":"^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$(?![\\s\\S])"})
}

/// A canonical decimal spelling of a complete unsigned 64-bit value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct DecimalU64(pub u64);
impl fmt::Display for DecimalU64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for DecimalU64 {
    type Err = &'static str;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.is_empty()
            || !text.bytes().all(|c| c.is_ascii_digit())
            || (text.len() > 1 && text.starts_with('0'))
        {
            return Err("expected a canonical unsigned decimal string");
        }
        text.parse::<u64>()
            .map(Self)
            .map_err(|_| "decimal value exceeds u64")
    }
}
impl Serialize for DecimalU64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
impl<'de> Deserialize<'de> for DecimalU64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for DecimalU64 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DecimalU64".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Equal-length alternatives are generated from the same bound used by parsing.
        let bound = u64::MAX.to_string();
        let mut alternatives = vec!["0".to_owned(), "[1-9][0-9]{0,18}".to_owned()];
        for (index, digit) in bound.bytes().enumerate() {
            let minimum = if index == 0 { b'1' } else { b'0' };
            if digit > minimum {
                alternatives.push(format!(
                    "{}[{}-{}][0-9]{{{}}}",
                    &bound[..index],
                    minimum as char,
                    (digit - 1) as char,
                    bound.len() - index - 1
                ));
            }
        }
        alternatives.push(bound);
        schemars::json_schema!({"type": "string", "pattern": format!("^({})$(?![\\s\\S])", alternatives.join("|")), "maxLength": 20})
    }
}

#[cfg(feature = "schema")]
fn u32_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "integer", "minimum": 0, "maximum": u32::MAX})
}
#[cfg(feature = "schema")]
fn optional_u32_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = u32_schema(generator);
    schema.insert("type".into(), serde_json::json!(["integer", "null"]));
    schema
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Empty {}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Filter {
    pub components: Vec<String>,
    pub levels: Vec<String>,
    pub targets: Vec<Target>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    ProcessInstance {
        #[cfg_attr(feature = "schema", schemars(schema_with = "u32_schema"))]
        pid: u32,
        start_time_filetime: DecimalU64,
        include_descendants: bool,
    },
    ExePath {
        path: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FilterSet {
    pub filter: Filter,
    #[serde(default)]
    pub duration_ms: Option<DecimalU64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Status {
    pub forwarding_enabled: bool,
    pub revision: DecimalU64,
    pub effective_filter: Filter,
    pub expires_at: Option<String>,
    pub health: Value,
    pub loss: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProcessIdentity {
    #[cfg_attr(feature = "schema", schemars(schema_with = "u32_schema"))]
    pub pid: u32,
    pub start_time_filetime: Option<DecimalU64>,
    pub exe_path: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LogRecord {
    pub stream_id: String,
    pub seq: DecimalU64,
    pub observed_at: String,
    pub source_time: Option<String>,
    pub source: String,
    pub component: String,
    pub level: String,
    pub raw_level: String,
    #[cfg_attr(feature = "schema", schemars(schema_with = "optional_u32_schema"))]
    #[serde(default)]
    pub session_id: Option<u32>,
    pub process: Option<ProcessIdentity>,
    #[cfg_attr(feature = "schema", schemars(schema_with = "optional_u32_schema"))]
    #[serde(default)]
    pub tid: Option<u32>,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Gap {
    pub stream_id: String,
    pub reason: String,
    pub count: Option<DecimalU64>,
    pub first_seq: Option<DecimalU64>,
    pub last_seq: Option<DecimalU64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SystemInfoRow {
    pub key: String,
    pub value: String,
    pub bad: bool,
    pub warning: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SystemInfoSection {
    pub title: String,
    pub rows: Vec<SystemInfoRow>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SystemInfo {
    pub collected_at: String,
    #[cfg_attr(feature = "schema", schemars(schema_with = "optional_u32_schema"))]
    pub session_id: Option<u32>,
    pub sections: Vec<SystemInfoSection>,
}

/// The discovery document returned by the connected receiver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Discovery {
    pub openrpc: String,
    pub info: DiscoveryInfo,
    pub methods: Vec<MethodDescriptor>,
    pub components: DiscoveryComponents,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DiscoveryInfo {
    pub title: String,
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DiscoveryComponents {
    pub schemas: std::collections::BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MethodDescriptor {
    pub name: String,
    #[serde(rename = "paramStructure")]
    pub param_structure: ParamStructure,
    pub params: Vec<ContentDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ContentDescriptor>,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ParamStructure {
    #[serde(rename = "by-name")]
    ByName,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContentDescriptor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    pub schema: Value,
}
