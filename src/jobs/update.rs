use core::fmt::Write;

use mqttrust::{Mqtt, QoS};

use crate::jobs::data_types::{MAX_CLIENT_TOKEN_LEN, MAX_JOB_ID_LEN, UpdateJobExecutionRequest};

use super::data_types::{JobStatus, MAX_THING_NAME_LEN, StatusDetails};


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

    pub fn execution_number(self, execution_number: i64) -> Self {
        Self {
            execution_number: Some(execution_number),
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
        (),
    > {
        let mut topic_path =
            heapless::String::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>::new();
        topic_path
            .write_fmt(format_args!(
                "$aws/things/{}/jobs/{}/update",
                client_id, self.job_id
            ))
            .map_err(drop)?;

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
        .map_err(drop)?;

        Ok((topic_path, payload))
    }

    pub fn send<M: Mqtt>(self, mqtt: &M, qos: QoS) -> Result<(), ()> {
        let (topic, payload) = self.topic_payload(mqtt.client_id())?;

        mqtt.publish(topic.as_str(), &payload, qos).map_err(drop)?;

        Ok(())
    }
}