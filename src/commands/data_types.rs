use serde::{Deserialize, Serialize};

/// AWS schema: `reasonCode` matches `[A-Z0-9_-]+` and is at most 64 characters.
pub const MAX_REASON_CODE_LEN: usize = 64;
/// AWS schema: `reasonDescription` is at most 1024 characters.
pub const MAX_REASON_DESCRIPTION_LEN: usize = 1024;
/// Maximum number of entries in a default `ResultMap`.
pub const MAX_RESULT_ENTRIES: usize = 8;

/// Status values a device may publish for a command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum CommandStatus {
    #[serde(rename = "IN_PROGRESS")]
    InProgress,
    #[serde(rename = "SUCCEEDED")]
    Succeeded,
    #[serde(rename = "FAILED")]
    Failed,
    #[serde(rename = "REJECTED")]
    Rejected,
    #[serde(rename = "TIMED_OUT")]
    TimedOut,
}

impl CommandStatus {
    /// Whether this status is terminal (cloud will not accept further updates
    /// once a terminal status is recorded).
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Rejected | Self::TimedOut
        )
    }
}

/// AWS schema: `{ "reasonCode": "<...>", "reasonDescription": "<...>" }`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct StatusReason<'a> {
    #[serde(rename = "reasonCode")]
    pub reason_code: &'a str,
    #[serde(rename = "reasonDescription")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_description: Option<&'a str>,
}

/// One entry in a command execution `result` map.
///
/// AWS encodes the type as the field name itself: `s` (string), `b` (boolean),
/// or `bin` (binary). With `serde(untagged)` the active variant is selected
/// from whichever field is present.
///
/// **CBOR caveat (v1)**: AWS encodes `bin` as raw bytes in CBOR but as a
/// base64 string in JSON. This v1 representation uses `&str` in both cases —
/// for JSON the caller is responsible for base64-encoding binary data. For
/// CBOR, raw bytes are out of scope; if needed, deserialize the request and
/// build a custom `Serialize` type that emits a CBOR byte string.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CommandResultEntry<'a> {
    String { s: &'a str },
    Bool { b: bool },
    Bin { bin: &'a str },
}

/// Default-sized result map type for ergonomic use.
pub type ResultMap<'a> = heapless::LinearMap<&'a str, CommandResultEntry<'a>, MAX_RESULT_ENTRIES>;

// Sized for the typical Commands result: up to MAX_RESULT_ENTRIES (10) keys
// (≤64 chars each) holding short strings, booleans, or base64 blobs (≤512
// chars). 4 KB covers the worst case with framing while staying well under
// AWS IoT Commands' per-update size cap.
impl crate::mqtt::MaxJsonSize for ResultMap<'_> {
    const MAX_JSON_SIZE: usize = 4096;
}

/// Outbound payload sent to
/// `$aws/commands/things/{thing}/executions/{id}/response/{format}`.
#[derive(Debug, PartialEq, Serialize)]
pub struct UpdateCommandExecutionRequest<'a, R: Serialize = ()> {
    /// Optional. The execution id is already encoded in the topic; AWS allows
    /// echoing it in the body.
    #[serde(rename = "executionId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<&'a str>,

    pub status: CommandStatus,

    #[serde(rename = "statusReason")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<StatusReason<'a>>,

    /// Caller-supplied result. Generic so users may provide a `ResultMap<'a>`,
    /// a custom struct, or any value that matches AWS's `result` schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<&'a R>,
}

/// A parsed inbound command request.
///
/// `execution_id` is owned (`heapless::String`) to release the topic-name
/// borrow before the payload is decoded — this disambiguates the immutable
/// borrow of [`MqttMessage::topic_name`] from the mutable borrow of
/// [`MqttMessage::payload_mut`] used by the JSON deserializer.
///
/// [`MqttMessage::topic_name`]: crate::mqtt::MqttMessage::topic_name
/// [`MqttMessage::payload_mut`]: crate::mqtt::MqttMessage::payload_mut
#[derive(Debug, PartialEq)]
pub struct CommandRequest<P> {
    pub execution_id: heapless::String<{ super::topics::MAX_EXECUTION_ID_LEN }>,
    pub payload: P,
}

/// Cloud rejection payload (received on `.../response/rejected/{format}`).
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ErrorResponse<'a> {
    #[serde(rename = "executionId")]
    pub execution_id: Option<&'a str>,
    pub status: Option<&'a str>,
    pub message: Option<&'a str>,
    pub timestamp: Option<i64>,
    #[serde(rename = "errorCode")]
    pub error_code: Option<u16>,
}
