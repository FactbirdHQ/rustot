use mqttrust::Mqtt;

use crate::jobs::JobTopic;

use super::{
    subscribe::Topic,
    JobError, {MAX_JOB_ID_LEN, MAX_THING_NAME_LEN},
};

#[derive(Default)]
pub struct Unsubscribe<'a, const N: usize> {
    topics: heapless::Vec<Topic<'a>, N>,
}

impl<'a, const N: usize> Unsubscribe<'a, N> {
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

        if self.topics.iter().any(|t| t == &topic) {
            return self;
        }

        let mut topics = self.topics;
        topics.push(topic).ok();
        Self { topics }
    }

    pub fn topics(
        self,
        client_id: &str,
    ) -> Result<heapless::Vec<heapless::String<256>, N>, JobError> {
        assert!(client_id.len() <= MAX_THING_NAME_LEN);

        self.topics
            .iter()
            .map(|topic| JobTopic::from(topic).format(client_id))
            .collect()
    }

    pub fn send<M: Mqtt>(self, mqtt: &M) -> Result<(), JobError> {
        let topic_paths = self.topics(mqtt.client_id())?;
        let topics: heapless::Vec<_, N> = topic_paths.iter().map(|s| s.as_str()).collect();

        for t in topics.chunks(5) {
            mqtt.unsubscribe(t)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mqttrust::{encoding::v4::decode_slice, Packet};

    use super::*;
    use crate::test::MockMqtt;

    #[test]
    fn splits_unsubscribe_all() {
        let mqtt = &MockMqtt::new();

        Unsubscribe::<10>::new()
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
        let bytes = mqtt.tx.borrow_mut().pop_front().unwrap();

        let packet = decode_slice(bytes.as_slice()).unwrap();
        let topics = match packet {
            Some(Packet::Unsubscribe(ref s)) => s.topics().collect::<Vec<_>>(),
            _ => panic!(),
        };

        assert_eq!(
            topics,
            vec![
                "$aws/things/test_client/jobs/notify",
                "$aws/things/test_client/jobs/notify-next",
                "$aws/things/test_client/jobs/get/accepted",
                "$aws/things/test_client/jobs/get/rejected",
                "$aws/things/test_client/jobs/start-next/accepted",
            ]
        );

        let bytes = mqtt.tx.borrow_mut().pop_front().unwrap();
        let packet = decode_slice(bytes.as_slice()).unwrap();
        let topics = match packet {
            Some(Packet::Unsubscribe(ref s)) => s.topics().collect::<Vec<_>>(),
            _ => panic!(),
        };
        assert_eq!(
            topics,
            vec![
                "$aws/things/test_client/jobs/start-next/rejected",
                "$aws/things/test_client/jobs/test_job/get/accepted",
                "$aws/things/test_client/jobs/test_job/get/rejected",
                "$aws/things/test_client/jobs/test_job/update/accepted",
                "$aws/things/test_client/jobs/test_job/update/rejected"
            ]
        );
    }
}
