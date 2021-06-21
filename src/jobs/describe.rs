use core::fmt::Write;

use mqttrust::{Mqtt, QoS};

use crate::jobs::data_types::DescribeJobExecutionRequest;

use super::data_types::{MAX_CLIENT_TOKEN_LEN, MAX_JOB_ID_LEN, MAX_THING_NAME_LEN};


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

    pub fn topic_payload(
        self,
        client_id: &str,
    ) -> Result<
        (
            heapless::String<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 22 }>,
            heapless::Vec<u8, { MAX_CLIENT_TOKEN_LEN + 2 }>,
        ),
        (),
    > {
        let mut topic_path =
            heapless::String::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 22 }>::new();

        let id = self.job_id.unwrap_or("$next");

        topic_path
            .write_fmt(format_args!("$aws/things/{}/jobs/{}/get", client_id, id))
            .map_err(drop)?;

        let payload = serde_json_core::to_vec(&DescribeJobExecutionRequest {
            execution_number: self.execution_number,
            include_job_document: self.include_job_document.then(|| true),
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
