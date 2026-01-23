use serde::Serialize;

use crate::jobs::JobTopic;
use crate::mqtt::{PayloadError, ToPayload};

use super::{JobError, MAX_CLIENT_TOKEN_LEN, MAX_JOB_ID_LEN, MAX_THING_NAME_LEN};

/// Gets detailed information about a job execution.
///
/// You can set the jobId to $next to return the next pending job execution for
/// a thing (status IN_PROGRESS or QUEUED).
///
/// Topic: $aws/things/{thingName}/jobs/{jobId}/get
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DescribeJobExecutionRequest<'a> {
    /// Optional. A number that identifies a particular job execution on a
    /// particular device. If not specified, the latest job execution is
    /// returned.
    #[serde(rename = "executionNumber")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_number: Option<i64>,
    /// Optional. When set to true, the response contains the job document. The
    /// default is false.
    #[serde(rename = "includeJobDocument")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_job_document: Option<bool>,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

#[derive(Default)]
pub struct Describe<'a> {
    job_id: Option<&'a str>,
    client_token: Option<&'a str>,
    include_job_document: bool,
    execution_number: Option<i64>,
}

impl<'a> Describe<'a> {
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

    pub fn job_id(self, job_id: &'a str) -> Self {
        assert!(job_id.len() < MAX_JOB_ID_LEN);

        Self {
            job_id: Some(job_id),
            ..self
        }
    }

    pub fn include_job_document(self) -> Self {
        Self {
            include_job_document: true,
            ..self
        }
    }

    pub fn execution_number(self, execution_number: i64) -> Self {
        Self {
            execution_number: Some(execution_number),
            ..self
        }
    }

    pub fn topic(
        &self,
        client_id: &str,
    ) -> Result<heapless::String<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 22 }>, JobError> {
        self.job_id
            .map(JobTopic::Get)
            .unwrap_or(JobTopic::GetNext)
            .format(client_id)
    }
}

impl ToPayload for Describe<'_> {
    fn max_size(&self) -> usize {
        256
    }

    fn encode(&self, buf: &mut [u8]) -> Result<usize, PayloadError> {
        serde_json_core::to_slice(
            &DescribeJobExecutionRequest {
                execution_number: self.execution_number,
                include_job_document: self.include_job_document.then_some(true),
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
        let req = DescribeJobExecutionRequest {
            execution_number: Some(1),
            include_job_document: Some(true),
            client_token: Some("test_client:token"),
        };
        assert_eq!(
            to_string::<_, 512>(&req).unwrap().as_str(),
            r#"{"executionNumber":1,"includeJobDocument":true,"clientToken":"test_client:token"}"#
        );
    }

    #[test]
    fn topic_and_payload() {
        let describe = Describe::new()
            .include_job_document()
            .execution_number(1)
            .client_token("test_client:token");

        let topic = describe.topic("test_client").unwrap();

        let mut buf = [0u8; 256];
        let len = describe.encode(&mut buf).unwrap();

        assert_eq!(
            &buf[..len],
            br#"{"executionNumber":1,"includeJobDocument":true,"clientToken":"test_client:token"}"#
        );

        assert_eq!(topic.as_str(), "$aws/things/test_client/jobs/$next/get");
    }

    #[test]
    fn topic_job_id() {
        let describe = Describe::new()
            .include_job_document()
            .execution_number(1)
            .job_id("test_job_id")
            .client_token("test_client:token");

        let topic = describe.topic("test_client").unwrap();

        let mut buf = [0u8; 256];
        let len = describe.encode(&mut buf).unwrap();

        assert_eq!(
            &buf[..len],
            br#"{"executionNumber":1,"includeJobDocument":true,"clientToken":"test_client:token"}"#
        );

        assert_eq!(
            topic.as_str(),
            "$aws/things/test_client/jobs/test_job_id/get"
        );
    }
}
