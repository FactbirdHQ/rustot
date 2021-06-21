pub mod data_types;
pub mod describe;
pub mod subscribe;
pub mod unsubscribe;
pub mod update;

use core::fmt::Write;
use mqttrust::{Mqtt, QoS};

use self::{
    data_types::JobStatus, describe::Describe, subscribe::Subscribe, unsubscribe::Unsubscribe,
    update::Update,
};
use crate::jobs::data_types::{
    GetPendingJobExecutionsRequest, StartNextPendingJobExecutionRequest, MAX_CLIENT_TOKEN_LEN,
    MAX_THING_NAME_LEN,
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
    pub fn format<const L: usize>(&self, client_id: &str) -> Result<heapless::String<L>, ()> {
        let mut topic_path = heapless::String::new();
        match self {
            Topic::Notify => {
                topic_path.write_fmt(format_args!("$aws/things/{}/jobs/notify", client_id))
            }
            Topic::NotifyNext => {
                topic_path.write_fmt(format_args!("$aws/things/{}/jobs/notify-next", client_id))
            }
            Topic::GetAccepted => {
                topic_path.write_fmt(format_args!("$aws/things/{}/jobs/get/accepted", client_id))
            }
            Topic::GetRejected => {
                topic_path.write_fmt(format_args!("$aws/things/{}/jobs/get/rejected", client_id))
            }
            Topic::StartNextAccepted => topic_path.write_fmt(format_args!(
                "$aws/things/{}/jobs/start-next/accepted",
                client_id
            )),
            Topic::StartNextRejected => topic_path.write_fmt(format_args!(
                "$aws/things/{}/jobs/start-next/rejected",
                client_id
            )),
            Topic::DescribeAccepted(job_id) => topic_path.write_fmt(format_args!(
                "$aws/things/{}/jobs/{}/get/accepted",
                client_id, job_id
            )),
            Topic::DescribeRejected(job_id) => topic_path.write_fmt(format_args!(
                "$aws/things/{}/jobs/{}/get/rejected",
                client_id, job_id
            )),
            Topic::UpdateAccepted(job_id) => topic_path.write_fmt(format_args!(
                "$aws/things/{}/jobs/{}/update/accepted",
                client_id, job_id
            )),
            Topic::UpdateRejected(job_id) => topic_path.write_fmt(format_args!(
                "$aws/things/{}/jobs/{}/update/rejected",
                client_id, job_id
            )),
        }
        .map_err(drop)?;

        Ok(topic_path)
    }
}

pub struct Jobs;

impl Jobs {
    pub fn get_pending<M: Mqtt>(mqtt: &M) -> Result<(), ()> {
        let mut topic = heapless::String::<{ MAX_THING_NAME_LEN + 21 }>::new();

        topic
            .write_fmt(format_args!("$aws/things/{}/jobs/get", mqtt.client_id()))
            .map_err(drop)?;

        let buf = &mut [0u8; MAX_CLIENT_TOKEN_LEN];
        let len =
            serde_json_core::to_slice(&GetPendingJobExecutionsRequest { client_token: None }, buf)
                .map_err(drop)?;

        mqtt.publish(topic.as_str(), &buf[..len], QoS::AtLeastOnce)
            .map_err(drop)?;

        Ok(())
    }

    pub fn start_next<M: Mqtt>(mqtt: &M) -> Result<(), ()> {
        let mut topic = heapless::String::<{ MAX_THING_NAME_LEN + 28 }>::new();

        topic
            .write_fmt(format_args!(
                "$aws/things/{}/jobs/start-next",
                mqtt.client_id()
            ))
            .map_err(drop)?;

        let buf = &mut [0u8; MAX_CLIENT_TOKEN_LEN];
        let len = serde_json_core::to_slice(
            &StartNextPendingJobExecutionRequest {
                step_timeout_in_minutes: None,
                client_token: None,
            },
            buf,
        )
        .map_err(drop)?;

        mqtt.publish(topic.as_str(), &buf[..len], QoS::AtLeastOnce)
            .map_err(drop)?;

        Ok(())
    }

    pub fn describe<'a>() -> Describe<'a> {
        Describe::new()
    }

    pub fn update(job_id: &str, status: JobStatus) -> Update {
        Update::new(job_id, status)
    }

    pub fn subscribe<'a>() -> Subscribe<'a> {
        Subscribe::new()
    }

    pub fn unsubscribe<'a>() -> Unsubscribe<'a> {
        Unsubscribe::new()
    }
}

#[cfg(test)]
mod tests {
    use mqttrust::{SubscribeRequest, SubscribeTopic, UnsubscribeRequest};

    use super::*;

    use crate::test::{MockMqtt, MqttRequest};

    #[test]
    fn splits_subscribe_all() {
        let mqtt = &MockMqtt::new();

        Jobs::subscribe()
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

    #[test]
    fn splits_unsubscribe_all() {
        let mqtt = &MockMqtt::new();

        Jobs::unsubscribe()
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
