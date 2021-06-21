use mqttrust::{Mqtt, QoS, SubscribeTopic};

use super::{
    data_types::{MAX_JOB_ID_LEN, MAX_THING_NAME_LEN},
    Topic,
};

#[derive(Default)]
pub struct Subscribe<'a> {
    topics: heapless::Vec<(Topic<'a>, QoS), 10>,
}

impl<'a> Subscribe<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn topic(self, topic: Topic<'a>, qos: QoS) -> Self {
        match topic {
            Topic::DescribeAccepted(job_id) => assert!(job_id.len() <= MAX_JOB_ID_LEN),
            Topic::DescribeRejected(job_id) => assert!(job_id.len() <= MAX_JOB_ID_LEN),
            Topic::UpdateAccepted(job_id) => assert!(job_id.len() <= MAX_JOB_ID_LEN),
            Topic::UpdateRejected(job_id) => assert!(job_id.len() <= MAX_JOB_ID_LEN),
            _ => {}
        }

        if self.topics.iter().find(|(t, _)| t == &topic).is_some() {
            return self;
        }

        let mut topics = self.topics;
        topics.push((topic, qos)).ok();
        Self { topics }
    }

    pub fn topics(self, client_id: &str) -> Result<heapless::Vec<SubscribeTopic, 10>, ()> {
        assert!(client_id.len() <= MAX_THING_NAME_LEN);

        self.topics
            .iter()
            .map(|(topic, qos)| {
                Ok(SubscribeTopic {
                    topic_path: topic.format(client_id)?,
                    qos: qos.clone(),
                })
            })
            .collect()
    }

    pub fn send<M: Mqtt>(self, mqtt: &M) -> Result<(), ()> {
        let topics = self.topics(mqtt.client_id())?;

        for t in topics.chunks(5) {
            mqtt.subscribe_many(heapless::Vec::from_slice(t).map_err(drop)?)
                .map_err(drop)?;
        }

        Ok(())
    }
}
