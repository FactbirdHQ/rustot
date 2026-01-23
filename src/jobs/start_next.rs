use serde::Serialize;

use crate::jobs::JobTopic;
use crate::mqtt::{PayloadError, ToPayload};

use super::{JobError, MAX_CLIENT_TOKEN_LEN, MAX_THING_NAME_LEN};

/// Gets and starts the next pending job execution for a thing (status
/// IN_PROGRESS or QUEUED).
///
/// Any job executions with status IN_PROGRESS are returned first.
///
/// Job executions are returned in the order in which they were created.
///
/// If the next pending job execution is QUEUED, its state is changed to
/// IN_PROGRESS and the job execution's status details are set as specified.
///
/// If the next pending job execution is already IN_PROGRESS, its status details
/// are not changed.
///
/// If no job executions are pending, the response does not include the
/// execution field.
///
/// You can optionally create a step timer by setting a value for the
/// stepTimeoutInMinutes property. If you don't update the value of this
/// property by running UpdateJobExecution, the job execution times out when the
/// step timer expires.
///
/// Topic: $aws/things/{thingName}/jobs/start-next
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartNextPendingJobExecutionRequest<'a> {
    // / A collection of name/value pairs that describe the status of the job
    // execution. If not specified, the statusDetails are unchanged.
    // #[serde(rename = "statusDetails")] #[serde(skip_serializing_if =
    // "Option::is_none")] pub status_details:
    // Option<::std::collections::HashMap<String, String>>, Specifies the amount
    // of time this device has to finish execution of this job. If the job
    // execution status is not set to a terminal state before this timer
    // expires, or before the timer is reset (by calling
    // <code>UpdateJobExecution</code>, setting the status to
    // <code>IN_PROGRESS</code> and specifying a new timeout value in field
    // <code>stepTimeoutInMinutes</code>) the job execution status will be
    // automatically set to <code>TIMED_OUT</code>. Note that setting this
    // timeout has no effect on that job execution timeout which may have been
    // specified when the job was created (<code>CreateJob</code> using field
    // <code>timeoutConfig</code>).
    #[serde(rename = "stepTimeoutInMinutes")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_timeout_in_minutes: Option<i64>,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

#[derive(Default)]
pub struct StartNext<'a> {
    client_token: Option<&'a str>,
    step_timeout_in_minutes: Option<i64>,
}

impl<'a> StartNext<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn client_token(self, client_token: &'a str) -> Self {
        assert!(client_token.len() < MAX_CLIENT_TOKEN_LEN);

        Self {
            client_token: Some(client_token),
            ..self
        }
    }

    pub fn step_timeout_in_minutes(self, step_timeout_in_minutes: i64) -> Self {
        Self {
            step_timeout_in_minutes: Some(step_timeout_in_minutes),
            ..self
        }
    }

    pub fn topic(
        &self,
        client_id: &str,
    ) -> Result<heapless::String<{ MAX_THING_NAME_LEN + 28 }>, JobError> {
        JobTopic::StartNext.format(client_id)
    }
}

impl ToPayload for StartNext<'_> {
    fn max_size(&self) -> usize {
        256
    }

    fn encode(&self, buf: &mut [u8]) -> Result<usize, PayloadError> {
        serde_json_core::to_slice(
            &StartNextPendingJobExecutionRequest {
                step_timeout_in_minutes: self.step_timeout_in_minutes,
                client_token: self.client_token,
            },
            buf,
        )
        .map_err(|_| PayloadError::BufferSize)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json_core::to_string;

    #[test]
    fn serialize_requests() {
        let req = StartNextPendingJobExecutionRequest {
            client_token: Some("test_client:token_next_pending"),
            step_timeout_in_minutes: Some(50),
        };
        assert_eq!(
            &to_string::<_, 512>(&req).unwrap(),
            r#"{"stepTimeoutInMinutes":50,"clientToken":"test_client:token_next_pending"}"#
        );
        let req_none = StartNextPendingJobExecutionRequest {
            client_token: Some("test_client:token_next_pending"),
            step_timeout_in_minutes: None,
        };
        assert_eq!(
            &to_string::<_, 512>(&req_none).unwrap(),
            r#"{"clientToken":"test_client:token_next_pending"}"#
        );
    }

    #[test]
    fn topic_and_payload() {
        let start_next = StartNext::new()
            .client_token("test_client:token_next_pending")
            .step_timeout_in_minutes(43);

        let topic = start_next.topic("test_client").unwrap();

        let mut buf = [0u8; 256];
        let len = start_next.encode(&mut buf).unwrap();

        assert_eq!(
            &buf[..len],
            br#"{"stepTimeoutInMinutes":43,"clientToken":"test_client:token_next_pending"}"#
        );

        assert_eq!(topic.as_str(), "$aws/things/test_client/jobs/start-next");
    }
}
