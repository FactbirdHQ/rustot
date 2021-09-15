use mqttrust::{Mqtt, QoS, SubscribeTopic};

use crate::jobs::JobError;

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
pub struct Subscribe<'a, const N: usize> {
    topics: heapless::Vec<(Topic<'a>, QoS), N>,
}

impl<'a, const N: usize> Subscribe<'a, N> {
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

        if self.topics.iter().any(|(t, _)| t == &topic) {
            return self;
        }

        let mut topics = self.topics;
        topics.push((topic, qos)).ok();

        Self { topics }
    }

    pub fn topics(
        self,
        client_id: &str,
    ) -> Result<heapless::Vec<(heapless::String<256>, QoS), N>, JobError> {
        assert!(client_id.len() <= MAX_THING_NAME_LEN);
        Ok(self
            .topics
            .iter()
            .map(|(topic, qos)| {
                (
                    JobTopic::from(topic).format::<256>(client_id).unwrap(),
                    *qos,
                )
            })
            .collect())
    }

    pub fn send<M: Mqtt>(self, mqtt: &M) -> Result<(), JobError> {
        let topic_paths = self.topics(mqtt.client_id())?;

        let topics: heapless::Vec<_, N> = topic_paths
            .iter()
            .map(|(s, qos)| SubscribeTopic {
                topic_path: s.as_str(),
                qos: *qos,
            })
            .collect();

        crate::rustot_log!(debug, "Subscribing!");

        for t in topics.chunks(5) {
            mqtt.subscribe(t)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mqttrust::{encoding::v4::decode_slice, Packet, QoS, SubscribeTopic};

    use super::*;

    use crate::test::MockMqtt;

    #[test]
    fn splits_subscribe_all() {
        let mqtt = &MockMqtt::new();

        Subscribe::<10>::new()
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
        let bytes = mqtt.tx.borrow_mut().pop_front().unwrap();
        let packet = decode_slice(bytes.as_slice()).unwrap();

        let topics = match packet {
            Some(Packet::Subscribe(ref s)) => s.topics().collect::<Vec<_>>(),
            _ => panic!(),
        };

        assert_eq!(
            topics,
            vec![
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/notify",
                    qos: QoS::AtLeastOnce
                },
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/notify-next",
                    qos: QoS::AtLeastOnce
                },
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/get/accepted",
                    qos: QoS::AtLeastOnce
                },
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/get/rejected",
                    qos: QoS::AtLeastOnce
                },
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/start-next/accepted",
                    qos: QoS::AtLeastOnce
                }
            ]
        );

        let bytes = mqtt.tx.borrow_mut().pop_front().unwrap();
        let packet = decode_slice(bytes.as_slice()).unwrap();

        let topics = match packet {
            Some(Packet::Subscribe(ref s)) => s.topics().collect::<Vec<_>>(),
            _ => panic!(),
        };

        assert_eq!(
            topics,
            vec![
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/start-next/rejected",
                    qos: QoS::AtLeastOnce
                },
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/test_job/get/accepted",
                    qos: QoS::AtLeastOnce
                },
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/test_job/get/rejected",
                    qos: QoS::AtLeastOnce
                },
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/test_job/update/accepted",
                    qos: QoS::AtLeastOnce
                },
                SubscribeTopic {
                    topic_path: "$aws/things/test_client/jobs/test_job/update/rejected",
                    qos: QoS::AtLeastOnce
                }
            ]
        );
    }
}
