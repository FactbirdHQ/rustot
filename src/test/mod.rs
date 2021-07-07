use std::{cell::RefCell, collections::VecDeque};

use mqttrust::{Mqtt, PublishRequest, QoS, SubscribeRequest, UnsubscribeRequest};

#[derive(Debug, PartialEq)]
pub enum MqttRequest {
    Publish(OwnedPublishRequest),
    Subscribe(SubscribeRequest),
    Unsubscribe(UnsubscribeRequest),
    Disconnect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnedPublishRequest {
    pub dup: bool,
    pub qos: QoS,
    pub retain: bool,
    pub topic_name: String,
    pub payload: Vec<u8>,
}

impl<'a> From<PublishRequest<'a>> for OwnedPublishRequest {
    fn from(v: PublishRequest<'a>) -> Self {
        Self {
            dup: v.dup,
            qos: v.qos,
            retain: v.retain,
            topic_name: String::from(v.topic_name),
            payload: Vec::from(v.payload),
        }
    }
}

///
/// Mock Mqtt client used for unit tests. Implements `mqttrust::Mqtt` trait.
///
pub struct MockMqtt {
    pub tx: RefCell<VecDeque<MqttRequest>>,
    publish_fail: bool,
}

impl MockMqtt {
    pub fn new() -> Self {
        Self {
            tx: RefCell::new(VecDeque::new()),
            publish_fail: false,
        }
    }

    pub fn publish_fail(&mut self) {
        self.publish_fail = true;
    }
}

impl Mqtt for MockMqtt {
    fn send(&self, request: mqttrust::Request) -> Result<(), mqttrust::MqttError> {
        let req = match request {
            mqttrust::Request::Publish(p) => {
                if self.publish_fail {
                    return Err(mqttrust::MqttError::Full);
                }
                MqttRequest::Publish(p.into())
            }
            mqttrust::Request::Subscribe(s) => MqttRequest::Subscribe(s),
            mqttrust::Request::Unsubscribe(u) => MqttRequest::Unsubscribe(u),
            mqttrust::Request::Disconnect => MqttRequest::Disconnect,
        };

        self.tx.borrow_mut().push_back(req);

        Ok(())
    }

    fn client_id(&self) -> &str {
        "test_client"
    }
}
