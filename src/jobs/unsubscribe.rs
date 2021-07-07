use mqttrust::Mqtt;

use crate::jobs::JobTopic;

use super::{
    subscribe::Topic,
    JobError, {MAX_JOB_ID_LEN, MAX_THING_NAME_LEN},
};

#[derive(Default)]
pub struct Unsubscribe<'a> {
    topics: heapless::Vec<Topic<'a>, 10>,
}

impl<'a> Unsubscribe<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn topic(self, topic: Topic<'a>) -> Self {
        match topic {
            Topic::DescribeAccepted(job_id) => assert!(job_id.len() <= MAX_JOB_ID_LEN),
            Topic::DescribeRejected(job_id) => assert!(job_id.len() <= MAX_JOB_ID_LEN),
            Topic::UpdateAccepted(job_id) => assert!(job_id.len() <= MAX_JOB_ID_LEN),
            Topic::UpdateRejected(job_id) => assert!(job_id.len() <= MAX_JOB_ID_LEN),
            _ => {}
        }

        if self.topics.iter().find(|&t| t == &topic).is_some() {
            return self;
        }

        let mut topics = self.topics;
        topics.push(topic).ok();
        Self { topics }
    }

    pub fn topics(
        self,
        client_id: &str,
    ) -> Result<heapless::Vec<heapless::String<256>, 10>, JobError> {
        assert!(client_id.len() <= MAX_THING_NAME_LEN);

        self.topics
            .iter()
            .map(|topic| JobTopic::from(topic).format(client_id))
            .collect()
    }

    pub fn send<M: Mqtt>(self, mqtt: &M) -> Result<(), JobError> {
        let topics = self.topics(mqtt.client_id())?;

        for t in topics.chunks(5) {
            mqtt.unsubscribe_many(heapless::Vec::from_slice(t).map_err(|_| JobError::Overflow)?)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::{MockMqtt, MqttRequest};
    use mqttrust::UnsubscribeRequest;

    #[test]
    fn splits_unsubscribe_all() {
        let mqtt = &MockMqtt::new();

        Unsubscribe::new()
            .topic(Topic::Notify)
            .topic(Topic::NotifyNext)
            .topic(Topic::GetAccepted)
            .topic(Topic::GetRejected)
            .topic(Topic::StartNextAccepted)
            .topic(Topic::StartNextRejected)
            .topic(Topic::DescribeAccepted("test_job"))
            .topic(Topic::DescribeRejected("test_job"))
            .topic(Topic::UpdateAccepted("test_job"))
            .topic(Topic::UpdateRejected("test_job"))
            .send(mqtt)
            .unwrap();

        assert_eq!(mqtt.tx.borrow_mut().len(), 2);
        assert_eq!(
            mqtt.tx.borrow_mut().pop_front(),
            Some(MqttRequest::Unsubscribe(UnsubscribeRequest {
                topics: heapless::Vec::from_slice(&[
                    heapless::String::from("$aws/things/test_client/jobs/notify"),
                    heapless::String::from("$aws/things/test_client/jobs/notify-next"),
                    heapless::String::from("$aws/things/test_client/jobs/get/accepted"),
                    heapless::String::from("$aws/things/test_client/jobs/get/rejected"),
                    heapless::String::from("$aws/things/test_client/jobs/start-next/accepted"),
                ])
                .unwrap()
            }))
        );
        assert_eq!(
            mqtt.tx.borrow_mut().pop_front(),
            Some(MqttRequest::Unsubscribe(UnsubscribeRequest {
                topics: heapless::Vec::from_slice(&[
                    heapless::String::from("$aws/things/test_client/jobs/start-next/rejected"),
                    heapless::String::from("$aws/things/test_client/jobs/test_job/get/accepted"),
                    heapless::String::from("$aws/things/test_client/jobs/test_job/get/rejected"),
                    heapless::String::from("$aws/things/test_client/jobs/test_job/update/accepted"),
                    heapless::String::from("$aws/things/test_client/jobs/test_job/update/rejected")
                ])
                .unwrap()
            }))
        );
    }
}
