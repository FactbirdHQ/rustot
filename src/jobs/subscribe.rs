use mqttrust::{Mqtt, QoS, SubscribeTopic};

use crate::{jobs::JobError, rustot_log};

use super::{
    JobTopic, {MAX_JOB_ID_LEN, MAX_THING_NAME_LEN},
};

#[derive(Debug, Clone, PartialEq)]
pub enum Topic<'a> {
    Notify,
    NotifyNext,
    GetAccepted,
    GetRejected,
    StartNextAccepted,
    StartNextRejected,
    DescribeAccepted(&'a str),
    DescribeRejected(&'a str),
    UpdateAccepted(&'a str),
    UpdateRejected(&'a str),
}

impl<'a> Topic<'a> {
    pub fn from_str(s: &'a str) -> Option<Self> {
        let tt = s.splitn(8, '/').collect::<heapless::Vec<&str, 8>>();
        Some(match (tt.get(0), tt.get(1), tt.get(2), tt.get(3)) {
            (Some(&"$aws"), Some(&"things"), _, Some(&"jobs")) => {
                // This is a job topic! Figure out which
                match (tt.get(4), tt.get(5), tt.get(6), tt.get(7)) {
                    (Some(&"notify-next"), None, None, None) => Topic::NotifyNext,
                    (Some(&"notify"), None, None, None) => Topic::Notify,
                    (Some(&"get"), Some(&"accepted"), None, None) => Topic::GetAccepted,
                    (Some(&"get"), Some(&"rejected"), None, None) => Topic::GetRejected,
                    (Some(&"start-next"), Some(&"accepted"), None, None) => {
                        Topic::StartNextAccepted
                    }
                    (Some(&"start-next"), Some(&"rejected"), None, None) => {
                        Topic::StartNextRejected
                    }
                    (Some(job_id), Some(&"update"), Some(&"accepted"), None) => {
                        Topic::UpdateAccepted(job_id)
                    }
                    (Some(job_id), Some(&"update"), Some(&"rejected"), None) => {
                        Topic::UpdateRejected(job_id)
                    }
                    (Some(job_id), Some(&"get"), Some(&"accepted"), None) => {
                        Topic::DescribeAccepted(job_id)
                    }
                    (Some(job_id), Some(&"get"), Some(&"rejected"), None) => {
                        Topic::DescribeRejected(job_id)
                    }
                    _ => return None,
                }
            }
            _ => return None,
        })
    }
}

impl<'a> From<&Topic<'a>> for JobTopic<'a> {
    fn from(t: &Topic<'a>) -> Self {
        match t {
            Topic::Notify => Self::Notify,
            Topic::NotifyNext => Self::NotifyNext,
            Topic::GetAccepted => Self::GetAccepted,
            Topic::GetRejected => Self::GetRejected,
            Topic::StartNextAccepted => Self::StartNextAccepted,
            Topic::StartNextRejected => Self::StartNextRejected,
            Topic::DescribeAccepted(job_id) => Self::DescribeAccepted(job_id),
            Topic::DescribeRejected(job_id) => Self::DescribeRejected(job_id),
            Topic::UpdateAccepted(job_id) => Self::UpdateAccepted(job_id),
            Topic::UpdateRejected(job_id) => Self::UpdateRejected(job_id),
        }
    }
}

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

    pub fn topics(self, client_id: &str) -> Result<heapless::Vec<SubscribeTopic, 10>, JobError> {
        assert!(client_id.len() <= MAX_THING_NAME_LEN);

        self.topics
            .iter()
            .map(|(topic, qos)| {
                Ok(SubscribeTopic {
                    topic_path: JobTopic::from(topic).format(client_id)?,
                    qos: qos.clone(),
                })
            })
            .collect()
    }

    pub fn send<M: Mqtt>(self, mqtt: &M) -> Result<(), JobError> {
        let topics = self.topics(mqtt.client_id())?;

        rustot_log!(debug, "Subscribing to: {:?}", topics);

        for t in topics.chunks(5) {
            mqtt.subscribe_many(heapless::Vec::from_slice(t).map_err(|_|JobError::Overflow)?)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mqttrust::{QoS, SubscribeRequest, SubscribeTopic};

    use super::*;

    use crate::test::{MockMqtt, MqttRequest};

    #[test]
    fn splits_subscribe_all() {
        let mqtt = &MockMqtt::new();

        Subscribe::new()
            .topic(Topic::Notify, QoS::AtLeastOnce)
            .topic(Topic::NotifyNext, QoS::AtLeastOnce)
            .topic(Topic::GetAccepted, QoS::AtLeastOnce)
            .topic(Topic::GetRejected, QoS::AtLeastOnce)
            .topic(Topic::StartNextAccepted, QoS::AtLeastOnce)
            .topic(Topic::StartNextRejected, QoS::AtLeastOnce)
            .topic(Topic::DescribeAccepted("test_job"), QoS::AtLeastOnce)
            .topic(Topic::DescribeRejected("test_job"), QoS::AtLeastOnce)
            .topic(Topic::UpdateAccepted("test_job"), QoS::AtLeastOnce)
            .topic(Topic::UpdateRejected("test_job"), QoS::AtLeastOnce)
            .send(mqtt)
            .unwrap();

        assert_eq!(mqtt.tx.borrow_mut().len(), 2);
        assert_eq!(
            mqtt.tx.borrow_mut().pop_front(),
            Some(MqttRequest::Subscribe(SubscribeRequest {
                topics: heapless::Vec::from_slice(&[
                    SubscribeTopic {
                        topic_path: heapless::String::from("$aws/things/test_client/jobs/notify"),
                        qos: QoS::AtLeastOnce
                    },
                    SubscribeTopic {
                        topic_path: heapless::String::from(
                            "$aws/things/test_client/jobs/notify-next"
                        ),
                        qos: QoS::AtLeastOnce
                    },
                    SubscribeTopic {
                        topic_path: heapless::String::from(
                            "$aws/things/test_client/jobs/get/accepted"
                        ),
                        qos: QoS::AtLeastOnce
                    },
                    SubscribeTopic {
                        topic_path: heapless::String::from(
                            "$aws/things/test_client/jobs/get/rejected"
                        ),
                        qos: QoS::AtLeastOnce
                    },
                    SubscribeTopic {
                        topic_path: heapless::String::from(
                            "$aws/things/test_client/jobs/start-next/accepted"
                        ),
                        qos: QoS::AtLeastOnce
                    }
                ])
                .unwrap()
            }))
        );
        assert_eq!(
            mqtt.tx.borrow_mut().pop_front(),
            Some(MqttRequest::Subscribe(SubscribeRequest {
                topics: heapless::Vec::from_slice(&[
                    SubscribeTopic {
                        topic_path: heapless::String::from(
                            "$aws/things/test_client/jobs/start-next/rejected"
                        ),
                        qos: QoS::AtLeastOnce
                    },
                    SubscribeTopic {
                        topic_path: heapless::String::from(
                            "$aws/things/test_client/jobs/test_job/get/accepted"
                        ),
                        qos: QoS::AtLeastOnce
                    },
                    SubscribeTopic {
                        topic_path: heapless::String::from(
                            "$aws/things/test_client/jobs/test_job/get/rejected"
                        ),
                        qos: QoS::AtLeastOnce
                    },
                    SubscribeTopic {
                        topic_path: heapless::String::from(
                            "$aws/things/test_client/jobs/test_job/update/accepted"
                        ),
                        qos: QoS::AtLeastOnce
                    },
                    SubscribeTopic {
                        topic_path: heapless::String::from(
                            "$aws/things/test_client/jobs/test_job/update/rejected"
                        ),
                        qos: QoS::AtLeastOnce
                    }
                ])
                .unwrap()
            }))
        );
    }
}
