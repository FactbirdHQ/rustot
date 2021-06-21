use mqttrust::Mqtt;

use super::{
    data_types::{MAX_JOB_ID_LEN, MAX_THING_NAME_LEN},
    Topic,
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

    pub fn topics(self, client_id: &str) -> Result<heapless::Vec<heapless::String<256>, 10>, ()> {
        assert!(client_id.len() <= MAX_THING_NAME_LEN);

        self.topics
            .iter()
            .map(|topic| topic.format(client_id))
            .collect()
    }

    pub fn send<M: Mqtt>(self, mqtt: &M) -> Result<(), ()> {
        let topics = self.topics(mqtt.client_id())?;

        for t in topics.chunks(5) {
            mqtt.unsubscribe_many(heapless::Vec::from_slice(t).map_err(drop)?)
                .map_err(drop)?;
        }

        Ok(())
    }
}
