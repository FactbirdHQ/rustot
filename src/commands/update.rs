// Used as a method call (`req.serialize(...)`) only on the cbor encoding branch.
#[cfg_attr(not(feature = "commands_cbor"), allow(unused_imports))]
use serde::Serialize;

use crate::commands::data_types::{
    CommandStatus, MAX_REASON_CODE_LEN, MAX_REASON_DESCRIPTION_LEN, StatusReason,
    UpdateCommandExecutionRequest,
};
use crate::mqtt::{MaxJsonSize, PayloadError, ToPayload};

/// JSON-framing overhead for [`UpdateCommandExecutionRequest`]: executionId
/// (≤64), statusReason (≤1024 + ≤64 + framing), status enum, and the JSON
/// object itself with all its keys.
const UPDATE_FRAMING_OVERHEAD: usize = 1280;

/// Type-state builder for an `UpdateCommandExecution` payload.
///
/// The generic `R` is the type of the optional `result` field — it evolves on
/// `result(...)` to accept any user-provided [`MaxJsonSize`] type, mirroring
/// `crate::jobs::update::Update<'a, S>`. The encode buffer is sized from
/// `R::MAX_JSON_SIZE` plus framing overhead.
pub struct Update<'a, R: MaxJsonSize = ()> {
    status: CommandStatus,
    execution_id: Option<&'a str>,
    status_reason: Option<StatusReason<'a>>,
    result: Option<&'a R>,
}

impl<'a> Update<'a, ()> {
    /// Create a new update with the given status.
    pub fn new(status: CommandStatus) -> Self {
        Self {
            status,
            execution_id: None,
            status_reason: None,
            result: None,
        }
    }
}

impl<'a, R: MaxJsonSize> Update<'a, R> {
    /// Echo the executionId in the payload body. Optional — AWS already knows
    /// the id from the topic path.
    pub fn execution_id(mut self, id: &'a str) -> Self {
        self.execution_id = Some(id);
        self
    }

    /// Set the `statusReason` field.
    ///
    /// AWS limits: `code` ≤ 64 chars matching `[A-Z0-9_-]+`; `description` ≤ 1024 chars.
    pub fn status_reason(mut self, code: &'a str, description: Option<&'a str>) -> Self {
        debug_assert!(code.len() <= MAX_REASON_CODE_LEN);
        debug_assert!(description.map(|d| d.len()).unwrap_or(0) <= MAX_REASON_DESCRIPTION_LEN);
        self.status_reason = Some(StatusReason {
            reason_code: code,
            reason_description: description,
        });
        self
    }

    /// Attach a `result`. Type-state: the returned builder's `R` becomes `T`.
    /// `T` must impl [`MaxJsonSize`] so the encode buffer can be sized to fit.
    pub fn result<T: MaxJsonSize>(self, result: &'a T) -> Update<'a, T> {
        Update {
            status: self.status,
            execution_id: self.execution_id,
            status_reason: self.status_reason,
            result: Some(result),
        }
    }
}

impl<R: MaxJsonSize> ToPayload for Update<'_, R> {
    fn max_size(&self) -> usize {
        R::MAX_JSON_SIZE + UPDATE_FRAMING_OVERHEAD
    }

    fn encode(&self, buf: &mut [u8]) -> Result<usize, PayloadError> {
        let req = UpdateCommandExecutionRequest {
            execution_id: self.execution_id,
            status: self.status,
            status_reason: self.status_reason.clone(),
            result: self.result,
        };

        #[cfg(feature = "commands_cbor")]
        {
            let mut serializer =
                minicbor_serde::Serializer::new(minicbor::encode::write::Cursor::new(&mut *buf));
            req.serialize(&mut serializer)
                .map_err(|_| PayloadError::EncodingFailed)?;
            Ok(serializer.into_encoder().writer().position())
        }

        #[cfg(not(feature = "commands_cbor"))]
        {
            serde_json_core::to_slice(&req, buf).map_err(|_| PayloadError::BufferSize)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[cfg(not(feature = "commands_cbor"))]
    use crate::commands::data_types::{CommandResultEntry, ResultMap};

    #[cfg(not(feature = "commands_cbor"))]
    fn encode<R: MaxJsonSize>(u: &Update<'_, R>) -> heapless::String<2048> {
        let mut buf = [0u8; 2048];
        let len = u.encode(&mut buf).unwrap();
        let s = core::str::from_utf8(&buf[..len]).unwrap();
        heapless::String::try_from(s).unwrap()
    }

    #[cfg(not(feature = "commands_cbor"))]
    #[test]
    fn json_in_progress_minimal() {
        let upd: Update<'_, ()> = Update::new(CommandStatus::InProgress).execution_id("e1");
        let s = encode(&upd);
        assert_eq!(s.as_str(), r#"{"executionId":"e1","status":"IN_PROGRESS"}"#);
    }

    #[cfg(not(feature = "commands_cbor"))]
    #[test]
    fn json_failed_with_status_reason() {
        let upd: Update<'_, ()> = Update::new(CommandStatus::Failed)
            .execution_id("e1")
            .status_reason("E_BAD", Some("car battery low"));
        let s = encode(&upd);
        assert_eq!(
            s.as_str(),
            r#"{"executionId":"e1","status":"FAILED","statusReason":{"reasonCode":"E_BAD","reasonDescription":"car battery low"}}"#
        );
    }

    #[cfg(not(feature = "commands_cbor"))]
    #[test]
    fn json_succeeded_with_result_map() {
        let mut map = ResultMap::new();
        map.insert("out", CommandResultEntry::String { s: "ok" })
            .unwrap();
        let upd = Update::new(CommandStatus::Succeeded)
            .execution_id("e1")
            .result(&map);
        let s = encode(&upd);
        assert_eq!(
            s.as_str(),
            r#"{"executionId":"e1","status":"SUCCEEDED","result":{"out":{"s":"ok"}}}"#
        );
    }

    #[cfg(not(feature = "commands_cbor"))]
    #[test]
    fn json_succeeded_with_bool_and_bin_results() {
        let mut map = ResultMap::new();
        map.insert("done", CommandResultEntry::Bool { b: true })
            .unwrap();
        map.insert("blob", CommandResultEntry::Bin { bin: "AAA=" })
            .unwrap();
        let upd = Update::new(CommandStatus::Succeeded).result(&map);
        let s = encode(&upd);
        // LinearMap preserves insertion order.
        assert_eq!(
            s.as_str(),
            r#"{"status":"SUCCEEDED","result":{"done":{"b":true},"blob":{"bin":"AAA="}}}"#
        );
    }

    #[cfg(feature = "commands_cbor")]
    #[test]
    fn cbor_round_trip_succeeded() {
        use serde::Deserialize;

        // Slim view used only to decode and confirm the CBOR branch produced
        // the documented field shape. We deliberately avoid making the full
        // `UpdateCommandExecutionRequest` `Deserialize`, since its `result`
        // field is `Option<&R>` (a non-trivial Deserialize bound for users).
        #[derive(Deserialize)]
        struct DecodedRequest<'a> {
            #[serde(rename = "executionId")]
            execution_id: Option<&'a str>,
            status: CommandStatus,
        }

        let upd: Update<'_, ()> = Update::new(CommandStatus::Succeeded).execution_id("e1");
        let mut buf = [0u8; 2048];
        let len = upd.encode(&mut buf).unwrap();

        let mut de = minicbor_serde::Deserializer::new(&buf[..len]);
        let decoded = DecodedRequest::deserialize(&mut de).unwrap();
        assert_eq!(decoded.execution_id, Some("e1"));
        assert_eq!(decoded.status, CommandStatus::Succeeded);
    }
}
