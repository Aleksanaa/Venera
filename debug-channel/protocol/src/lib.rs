use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAJOR: u32 = 1;
pub const DEFAULT_MAX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(deserialize_with = "integer::u32")]
    pub major: u32,
    #[serde(flatten)]
    pub body: Message,
}
impl Envelope {
    pub fn new(body: Message) -> Self {
        Self { major: MAJOR, body }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    #[serde(rename = "hello")]
    Hello {
        role: String,
        software_version: String,
        capabilities: Vec<String>,
    },
    #[serde(rename = "status.get")]
    StatusGet { request_id: String },
    #[serde(rename = "status")]
    Status {
        #[serde(flatten)]
        status: Status,
    },
    #[serde(rename = "filter.set")]
    FilterSet {
        request_id: String,
        filter: Filter,
        #[serde(default, deserialize_with = "integer::optional_u64")]
        duration_ms: Option<u64>,
    },
    #[serde(rename = "filter.reset")]
    FilterReset { request_id: String },
    #[serde(rename = "request.result")]
    RequestResult {
        #[serde(flatten)]
        result: RequestResult,
    },
    #[serde(rename = "log")]
    Log {
        #[serde(flatten)]
        record: LogRecord,
    },
    #[serde(rename = "gap")]
    Gap {
        stream_id: String,
        reason: String,
        #[serde(default, deserialize_with = "integer::optional_u64")]
        count: Option<u64>,
        #[serde(default, deserialize_with = "integer::optional_u64")]
        first_seq: Option<u64>,
        #[serde(default, deserialize_with = "integer::optional_u64")]
        last_seq: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    pub stream_id: String,
    #[serde(deserialize_with = "integer::u64")]
    pub seq: u64,
    pub observed_at: String,
    pub source_time: Option<String>,
    pub source: String,
    pub component: String,
    pub level: String,
    pub raw_level: String,
    #[serde(default, deserialize_with = "integer::optional_u32")]
    pub session_id: Option<u32>,
    pub process: Option<ProcessIdentity>,
    #[serde(default, deserialize_with = "integer::optional_u32")]
    pub tid: Option<u32>,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    #[serde(deserialize_with = "integer::u32")]
    pub pid: u32,
    pub start_time_filetime: Option<String>,
    pub exe_path: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub components: Vec<String>,
    pub levels: Vec<String>,
    pub targets: Vec<Target>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    ProcessInstance {
        #[serde(deserialize_with = "integer::u32")]
        pid: u32,
        start_time_filetime: String,
        include_descendants: bool,
    },
    ExePath {
        path: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub forwarding_enabled: bool,
    #[serde(deserialize_with = "integer::u64")]
    pub revision: u64,
    pub effective_filter: Filter,
    pub expires_at: Option<String>,
    pub health: Value,
    pub loss: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestError {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestResult {
    pub request_id: String,
    pub operation: String,
    pub ok: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "integer::optional_u64"
    )]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_filter: Option<Filter>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarding_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RequestError>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DecodeError {
    Utf8,
    Json(String),
    Major(u64),
    UnknownType(String),
    TooLong,
}

mod integer {
    use serde::{de::Error, Deserialize, Deserializer};
    use serde_json::Value;

    pub(super) fn number(value: &Value) -> Option<u64> {
        value.as_u64().or_else(|| {
            let number = value.as_f64()?;
            // 2^64 is the first floating-point value beyond u64's range.
            ((0.0..18_446_744_073_709_551_616.0).contains(&number) && number.fract() == 0.0)
                .then_some(number as u64)
        })
    }

    pub fn u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        number(&Value::deserialize(deserializer)?)
            .ok_or_else(|| D::Error::custom("expected an integer in the u64 range"))
    }

    pub fn u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
        u64(deserializer)?
            .try_into()
            .map_err(|_| D::Error::custom("expected an integer in the u32 range"))
    }

    pub fn optional_u64<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        Option::<Value>::deserialize(deserializer)?
            .map(|value| {
                number(&value)
                    .ok_or_else(|| D::Error::custom("expected an integer in the u64 range"))
            })
            .transpose()
    }

    pub fn optional_u32<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u32>, D::Error> {
        optional_u64(deserializer)?
            .map(|number| {
                number
                    .try_into()
                    .map_err(|_| D::Error::custom("expected an integer in the u32 range"))
            })
            .transpose()
    }
}

pub fn decode(line: &[u8]) -> Result<Envelope, DecodeError> {
    let text = std::str::from_utf8(line).map_err(|_| DecodeError::Utf8)?;
    let value: Value = serde_json::from_str(text).map_err(|e| DecodeError::Json(e.to_string()))?;
    let major = value
        .get("major")
        .and_then(integer::number)
        .ok_or_else(|| DecodeError::Json("major must be an unsigned integer".into()))?;
    if major != u64::from(MAJOR) {
        return Err(DecodeError::Major(major));
    }
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| DecodeError::Json("type must be a string".into()))?;
    if ![
        "hello",
        "status.get",
        "status",
        "filter.set",
        "filter.reset",
        "request.result",
        "log",
        "gap",
    ]
    .contains(&kind)
    {
        return Err(DecodeError::UnknownType(kind.into()));
    }
    serde_json::from_value(value).map_err(|e| DecodeError::Json(e.to_string()))
}
pub fn encode(message: &Envelope) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(message)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub struct Framer {
    buffer: Vec<u8>,
    max_bytes: usize,
    dropping: bool,
}
impl Framer {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_bytes,
            dropping: false,
        }
    }
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.dropping = false;
    }
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Envelope, DecodeError>> {
        self.push_raw(bytes)
            .into_iter()
            .map(|item| item.map(|(message, _)| message))
            .collect()
    }
    pub fn push_raw(&mut self, bytes: &[u8]) -> Vec<Result<(Envelope, Value), DecodeError>> {
        let mut result = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                if self.dropping {
                    result.push(Err(DecodeError::TooLong));
                } else if !self.buffer.is_empty() {
                    result.push(decode(&self.buffer).map(|message| {
                        (
                            message,
                            serde_json::from_slice(&self.buffer).expect("decoded JSON"),
                        )
                    }));
                }
                self.reset();
            } else if !self.dropping {
                if self.buffer.len() == self.max_bytes {
                    self.buffer.clear();
                    self.dropping = true;
                } else {
                    self.buffer.push(byte);
                }
            }
        }
        result
    }
}

/// `ancestors` contains the observed ancestry admitted by the active rule's lifecycle.
pub fn matches_filter(filter: &Filter, record: &LogRecord, ancestors: &[ProcessIdentity]) -> bool {
    (filter.components.is_empty() || filter.components.contains(&record.component))
        && (filter.levels.is_empty() || filter.levels.contains(&record.level))
        && (filter.targets.is_empty()
            || record.process.as_ref().is_some_and(|p| {
                filter.targets.iter().any(|target| match target {
                    Target::ExePath { path } => p
                        .exe_path
                        .as_ref()
                        .is_some_and(|actual| same_path(actual, path)),
                    Target::ProcessInstance {
                        pid,
                        start_time_filetime,
                        include_descendants,
                    } => {
                        let same = |p: &ProcessIdentity| {
                            p.pid == *pid
                                && p.start_time_filetime.as_ref() == Some(start_time_filetime)
                        };
                        same(p) || (*include_descendants && ancestors.iter().any(same))
                    }
                })
            }))
}

#[cfg(not(windows))]
fn same_path(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

#[cfg(windows)]
fn same_path(left: &str, right: &str) -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn CompareStringOrdinal(
            left: *const u16,
            left_len: i32,
            right: *const u16,
            right_len: i32,
            ignore_case: i32,
        ) -> i32;
    }
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    let (Ok(left_len), Ok(right_len)) = (i32::try_from(left.len()), i32::try_from(right.len()))
    else {
        return false;
    };
    // Both slices remain alive for the call, and the explicit lengths cover their initialized UTF-16 units.
    unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) == 2 }
}
