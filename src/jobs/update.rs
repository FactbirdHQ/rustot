use serde::Serialize;

use crate::jobs::{data_types::JobStatus, MAX_CLIENT_TOKEN_LEN};
use crate::mqtt::{PayloadError, ToPayload};

/// Updates the status of a job execution. You can optionally create a step
/// timer by setting a value for the stepTimeoutInMinutes property. If you don't
/// update the value of this property by running UpdateJobExecution again, the
/// job execution times out when the step timer expires.
///
/// Topic: $aws/things/{thingName}/jobs/{jobId}/update
///
/// The type parameter `S` allows any serializable type for status_details,
/// enabling different consumers to provide different status structures.
#[derive(Debug, PartialEq, Serialize)]
pub struct UpdateJobExecutionRequest<'a, S: Serialize = ()> {
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
    /// Optional. A collection of name/value pairs that describe the status of
    /// the job execution. If not specified, the statusDetails are unchanged.
    #[serde(rename = "statusDetails")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_details: Option<&'a S>,
    /// Specifies the amount of time this device has to finish execution of this
    /// job. If the job execution status is not set to a terminal state before
    /// this timer expires, or before the timer is reset (by again calling
    /// UpdateJobExecution, setting the status to IN_PROGRESS and specifying a
    /// new timeout value in this field) the job execution status will be
    /// automatically set to TIMED_OUT. Note that setting or resetting this
    /// timeout has no effect on that job execution timeout which may have been
    /// specified when the job was created (CreateJob using field timeoutConfig).
    #[serde(rename = "stepTimeoutInMinutes")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_timeout_in_minutes: Option<i64>,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

/// Builder for job execution update requests.
///
/// The type parameter `S` represents the status details type. Use
/// [`status_details`](Self::status_details) to set the status details and
/// change the type parameter.
pub struct Update<'a, S: Serialize = ()> {
    status: JobStatus,
    client_token: Option<&'a str>,
    status_details: Option<&'a S>,
    include_job_document: bool,
    execution_number: Option<i64>,
    include_job_execution_state: bool,
    expected_version: Option<i64>,
    step_timeout_in_minutes: Option<i64>,
}

impl<'a> Update<'a, ()> {
    /// Create a new Update builder with the given job status.
    pub fn new(status: JobStatus) -> Self {
        Self {
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
}

impl<'a, S: Serialize> Update<'a, S> {
    /// Set the client token for request correlation.
    pub fn client_token(self, client_token: &'a str) -> Self {
        assert!(client_token.len() < MAX_CLIENT_TOKEN_LEN);

        Self {
            client_token: Some(client_token),
            ..self
        }
    }

    /// Set the status details.
    ///
    /// This method accepts any type that implements `Serialize`, allowing
    /// different consumers to provide different status detail structures.
    pub fn status_details<T: Serialize>(self, status_details: &'a T) -> Update<'a, T> {
        Update {
            status: self.status,
            client_token: self.client_token,
            status_details: Some(status_details),
            include_job_document: self.include_job_document,
            execution_number: self.execution_number,
            include_job_execution_state: self.include_job_execution_state,
            expected_version: self.expected_version,
            step_timeout_in_minutes: self.step_timeout_in_minutes,
        }
    }

    /// Include the job document in the response.
    pub fn include_job_document(self) -> Self {
        Self {
            include_job_document: true,
            ..self
        }
    }

    /// Include the job execution state in the response.
    pub fn include_job_execution_state(self) -> Self {
        Self {
            include_job_execution_state: true,
            ..self
        }
    }

    /// Set the execution number.
    pub fn execution_number(self, execution_number: i64) -> Self {
        Self {
            execution_number: Some(execution_number),
            ..self
        }
    }

    /// Set the expected version for optimistic locking.
    pub fn expected_version(self, expected_version: i64) -> Self {
        Self {
            expected_version: Some(expected_version),
            ..self
        }
    }

    /// Set the step timeout in minutes.
    pub fn step_timeout_in_minutes(self, step_timeout_in_minutes: i64) -> Self {
        Self {
            step_timeout_in_minutes: Some(step_timeout_in_minutes),
            ..self
        }
    }
}

impl<S: Serialize> ToPayload for Update<'_, S> {
    fn max_size(&self) -> usize {
        512
    }

    fn encode(&self, buf: &mut [u8]) -> Result<usize, PayloadError> {
        serde_json_core::to_slice(
            &UpdateJobExecutionRequest {
                execution_number: self.execution_number,
                include_job_document: self.include_job_document.then_some(true),
                expected_version: self.expected_version,
                include_job_execution_state: self.include_job_execution_state.then_some(true),
                status: self.status,
                status_details: self.status_details,
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
    use crate::jobs::JobTopic;

    use super::*;
    use serde_json_core::to_string;

    #[test]
    fn serialize_requests() {
        let req: UpdateJobExecutionRequest<'_, ()> = UpdateJobExecutionRequest {
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
    fn topic_and_payload() {
        let topic = JobTopic::Update("test_job_id")
            .format::<64>("test_client")
            .unwrap();

        let update = Update::new(JobStatus::Failed)
            .client_token("test_client:token_update")
            .step_timeout_in_minutes(50)
            .execution_number(5)
            .expected_version(2)
            .include_job_document()
            .include_job_execution_state();

        let mut buf = [0u8; 512];
        let len = update.encode(&mut buf).unwrap();

        assert_eq!(&buf[..len], br#"{"executionNumber":5,"expectedVersion":2,"includeJobDocument":true,"includeJobExecutionState":true,"status":"FAILED","stepTimeoutInMinutes":50,"clientToken":"test_client:token_update"}"#);

        assert_eq!(
            topic.as_str(),
            "$aws/things/test_client/jobs/test_job_id/update"
        );
    }

    #[test]
    fn serialize_with_status_details() {
        // Test with a custom status details struct
        #[derive(Serialize)]
        struct TestStatus {
            self_test: &'static str,
            progress: &'static str,
        }

        let status = TestStatus {
            self_test: "receiving",
            progress: "10/100",
        };

        let update = Update::new(JobStatus::InProgress)
            .client_token("test_client")
            .status_details(&status);

        let mut buf = [0u8; 512];
        let len = update.encode(&mut buf).unwrap();

        let json = core::str::from_utf8(&buf[..len]).unwrap();
        assert!(json.contains(r#""self_test":"receiving""#));
        assert!(json.contains(r#""progress":"10/100""#));
        assert!(json.contains(r#""status":"IN_PROGRESS""#));
    }
}
