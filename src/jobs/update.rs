use mqttrust::{Mqtt, QoS};
use serde::Serialize;

use crate::jobs::{
    data_types::JobStatus, JobTopic, StatusDetails, MAX_CLIENT_TOKEN_LEN, MAX_JOB_ID_LEN,
    MAX_THING_NAME_LEN,
};

use super::JobError;

/// Updates the status of a job execution. You can optionally create a step
/// timer by setting a value for the stepTimeoutInMinutes property. If you don't
/// update the value of this property by running UpdateJobExecution again, the
/// job execution times out when the step timer expires.
///
/// Topic: $aws/things/{thingName}/jobs/{jobId}/update
#[derive(Debug, PartialEq, Serialize)]
pub struct UpdateJobExecutionRequest<'a> {
    /// Optional. A number that identifies a particular job execution on a
    /// particular device.
    #[serde(rename = "executionNumber")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_number: Option<i64>,
    /// Optional. The expected current version of the job execution. Each time
    /// you update the job execution, its version is incremented. If the version
    /// of the job execution stored in Jobs does not match, the update is
    /// rejected with a VersionMismatch error, and an ErrorResponse that
    /// contains the current job execution status data is returned. (This makes
    /// it unnecessary to perform a separate DescribeJobExecution request in
    /// order to obtain the job execution status data.)
    #[serde(rename = "expectedVersion")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<i64>,
    /// Optional. When set to true, the response contains the job document. The
    /// default is false.
    #[serde(rename = "includeJobDocument")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_job_document: Option<bool>,
    /// Optional. When included and set to true, the response contains the
    /// JobExecutionState data. The default is false.
    #[serde(rename = "includeJobExecutionState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_job_execution_state: Option<bool>,
    /// The new status for the job execution (IN_PROGRESS, FAILED, SUCCESS, or
    /// REJECTED). This must be specified on every update.
    #[serde(rename = "status")]
    pub status: JobStatus,
    // /  Optional. A collection of name/value pairs that describe the status of
    // the job execution. If not specified, the statusDetails are unchanged.
    #[serde(rename = "statusDetails")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_details: Option<&'a StatusDetails>,
    // Specifies the amount of time this device has to finish execution of this
    // job. If the job execution status is not set to a terminal state before
    // this timer expires, or before the timer is reset (by again calling
    // <code>UpdateJobExecution</code>, setting the status to
    // <code>IN_PROGRESS</code> and specifying a new timeout value in this
    // field) the job execution status will be automatically set to
    // <code>TIMED_OUT</code>. Note that setting or resetting this timeout has
    // no effect on that job execution timeout which may have been specified
    // when the job was created (<code>CreateJob</code> using field
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

pub struct Update<'a> {
    job_id: &'a str,
    status: JobStatus,
    client_token: Option<&'a str>,
    status_details: Option<&'a StatusDetails>,
    include_job_document: bool,
    execution_number: Option<i64>,
    include_job_execution_state: bool,
    expected_version: Option<i64>,
    step_timeout_in_minutes: Option<i64>,
}

impl<'a> Update<'a> {
    pub fn new(job_id: &'a str, status: JobStatus) -> Self {
        assert!(job_id.len() < MAX_JOB_ID_LEN);

        Self {
            job_id,
            status,
            status_details: None,
            include_job_document: false,
            execution_number: None,
            include_job_execution_state: false,
            expected_version: None,
            client_token: None,
            step_timeout_in_minutes: None,
        }
    }

    pub fn client_token(self, client_token: &'a str) -> Self {
        assert!(client_token.len() < MAX_CLIENT_TOKEN_LEN);

        Self {
            client_token: Some(client_token),
            ..self
        }
    }

    pub fn status_details(self, status_details: &'a StatusDetails) -> Self {
        Self {
            status_details: Some(status_details),
            ..self
        }
    }

    pub fn include_job_document(self) -> Self {
        Self {
            include_job_document: true,
            ..self
        }
    }

    pub fn include_job_execution_state(self) -> Self {
        Self {
            include_job_execution_state: true,
            ..self
        }
    }

    pub fn execution_number(self, execution_number: i64) -> Self {
        Self {
            execution_number: Some(execution_number),
            ..self
        }
    }

    pub fn expected_version(self, expected_version: i64) -> Self {
        Self {
            expected_version: Some(expected_version),
            ..self
        }
    }

    pub fn step_timeout_in_minutes(self, step_timeout_in_minutes: i64) -> Self {
        Self {
            step_timeout_in_minutes: Some(step_timeout_in_minutes),
            ..self
        }
    }

    pub fn topic_payload(
        self,
        client_id: &str,
    ) -> Result<
        (
            heapless::String<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>,
            heapless::Vec<u8, 512>,
        ),
        JobError,
    > {
        let payload = serde_json_core::to_vec(&UpdateJobExecutionRequest {
            execution_number: self.execution_number,
            include_job_document: self.include_job_document.then(|| true),
            expected_version: self.expected_version,
            include_job_execution_state: self.include_job_execution_state.then(|| true),
            status: self.status,
            status_details: self.status_details,
            step_timeout_in_minutes: self.step_timeout_in_minutes,
            client_token: self.client_token,
        })
        .map_err(|_|JobError::Encoding)?;

        Ok((JobTopic::Update(self.job_id).format(client_id)?, payload))
    }

    pub fn send<M: Mqtt>(self, mqtt: &M, qos: QoS) -> Result<(), JobError> {
        let (topic, payload) = self.topic_payload(mqtt.client_id())?;

        mqtt.publish(topic.as_str(), &payload, qos)?;

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json_core::to_string;

    #[test]
    fn serialize_requests() {
        let req = UpdateJobExecutionRequest {
            client_token: Some("test_client:token_update"),
            step_timeout_in_minutes: Some(50),
            execution_number: Some(5),
            expected_version: Some(2),
            include_job_document: Some(true),
            include_job_execution_state: Some(true),
            status_details: None,
            status: JobStatus::Failed,
        };
        assert_eq!(
            &to_string::<_, 512>(&req).unwrap(),
            r#"{"executionNumber":5,"expectedVersion":2,"includeJobDocument":true,"includeJobExecutionState":true,"status":"FAILED","stepTimeoutInMinutes":50,"clientToken":"test_client:token_update"}"#
        );
    }

    #[test]
    fn topic_payload() {
        let (topic, payload) = Update::new("test_job_id", JobStatus::Failed)
            .client_token("test_client:token_update")
            .step_timeout_in_minutes(50)
            .execution_number(5)
            .expected_version(2)
            .include_job_document()
            .include_job_execution_state()
            .topic_payload("test_client")
            .unwrap();

        assert_eq!(payload, br#"{"executionNumber":5,"expectedVersion":2,"includeJobDocument":true,"includeJobExecutionState":true,"status":"FAILED","stepTimeoutInMinutes":50,"clientToken":"test_client:token_update"}"#);

        assert_eq!(
            topic.as_str(),
            "$aws/things/test_client/jobs/test_job_id/update"
        );
    }
}
