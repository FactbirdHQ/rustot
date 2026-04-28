//! # AWS IoT Commands (Remote Commands)
//!
//! Cloud-to-device, one-shot instruction service. Unlike Jobs, command
//! executions have no queueing or retry semantics — the device receives a
//! request and returns a single terminal response.
//!
//! See: <https://docs.aws.amazon.com/iot/latest/developerguide/iot-remote-command.html>
//!
//! ## Workflow
//!
//! 1. Subscribe to `$aws/commands/things/{ThingName}/executions/+/request/{format}`.
//! 2. On message: parse `executionId` from the topic, deserialize the payload.
//! 3. Optionally publish `IN_PROGRESS` to `.../executions/{id}/response/{format}`.
//! 4. Publish a terminal status (`SUCCEEDED` / `FAILED` / `REJECTED`) on the
//!    same response topic, optionally with `statusReason` and `result`.
//! 5. Optionally subscribe to `.../response/accepted/{format}` and
//!    `.../response/rejected/{format}` for cloud confirmation.
//!
//! Default cloud-side timeout is 10 seconds, max 12 hours. Devices may publish
//! a terminal status to override a cloud-issued `TIMED_OUT`.
//!
//! ## Payload format
//!
//! With the `commands_cbor` feature enabled, payloads use CBOR; otherwise
//! JSON. This mirrors `provision_cbor` in `crate::provisioning`. Only one
//! format may be active per build.
//!
//! ## Caveats
//!
//! - **Binary results in CBOR**: AWS encodes the `bin` result variant as a raw
//!   CBOR byte string, but [`CommandResultEntry::Bin`] currently exposes
//!   `&str`. JSON callers should pass base64-encoded strings; CBOR callers who
//!   need byte-string results should construct a custom `Serialize` type
//!   instead of using [`ResultMap`].
//! - The unregistered `$aws/commands/clients/{ClientId}/...` form is not
//!   supported; this module targets registered Things only.

pub mod data_types;
pub mod error;
pub mod stream;
pub mod topics;
pub mod update;

pub use data_types::{
    CommandRequest, CommandResultEntry, CommandStatus, ErrorResponse, MAX_REASON_CODE_LEN,
    MAX_REASON_DESCRIPTION_LEN, MAX_RESULT_ENTRIES, ResultMap, StatusReason,
    UpdateCommandExecutionRequest,
};
pub use error::CommandError;
pub use stream::{CommandAgent, parse_command_request};
pub use topics::{CommandTopic, MAX_COMMAND_TOPIC_LEN, MAX_EXECUTION_ID_LEN, Topic};
pub use update::Update;

/// AWS IoT thing-name length limit, re-exported from `crate::jobs`.
pub use crate::jobs::MAX_THING_NAME_LEN;

/// Factory mirroring `crate::jobs::Jobs`.
pub struct Commands;

impl Commands {
    /// Start a new `UpdateCommandExecution` builder.
    pub fn update<'a>(status: CommandStatus) -> Update<'a, ()> {
        Update::new(status)
    }
}
