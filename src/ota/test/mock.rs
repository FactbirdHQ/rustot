use std::{cell::RefCell, collections::VecDeque};

use mqttrust::{Mqtt, PublishRequest, QoS, SubscribeRequest, UnsubscribeRequest};

use crate::ota::{encoding::FileContext, pal::{ImageState, OtaPal, OtaPalError, PalImageState, Version}};

///
/// Mock timer used for unit tests. Implements `embedded_hal::timer::CountDown`
/// & `embedded_hal::timer::Cancel` traits.
///
pub struct MockTimer {
    pub is_started: bool,
}
impl MockTimer {
    pub fn new() -> Self {
        Self { is_started: false }
    }
}
impl embedded_hal::timer::CountDown for MockTimer {
    type Error = ();

    type Time = u32;

    fn try_start<T>(&mut self, count: T) -> Result<(), Self::Error>
    where
        T: Into<Self::Time>,
    {
        self.is_started = true;
        Ok(())
    }

    fn try_wait(&mut self) -> nb::Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_hal::timer::Cancel for MockTimer {
    fn try_cancel(&mut self) -> Result<(), Self::Error> {
        self.is_started = false;
        Ok(())
    }
}

///
/// Mock Platform abstration layer used for unit tests. Implements `OtaPal`
/// trait.
///
pub struct MockPal {}

impl OtaPal for MockPal {
    type Error = ();

    fn abort(&mut self, file: &FileContext) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn create_file_for_rx(&mut self, file: &FileContext) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn get_platform_image_state(&self) -> Result<PalImageState, OtaPalError<Self::Error>> {
        Ok(PalImageState::Valid)
    }

    fn set_platform_image_state(
        &mut self,
        image_state: ImageState,
    ) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn reset_device(&mut self) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn close_file(&mut self, file: &FileContext) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn write_block(
        &mut self,
        file: &FileContext,
        block_offset: usize,
        block_payload: &[u8],
    ) -> Result<usize, OtaPalError<Self::Error>> {
        Ok(block_payload.len())
    }

    fn get_active_firmware_version(&self) -> Result<Version, OtaPalError<Self::Error>> {
        Ok(Version::default())
    }
}

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
            publish_fail: false
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
            },
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
