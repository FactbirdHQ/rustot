mod data_types;
mod error;
mod topics;

use core::convert::TryFrom;

pub use error::Error;
use topics::{Direction, Subscribe, Topic, Unsubscribe};

use mqttrust::{Mqtt, QoS};
use serde::{de::DeserializeOwned, Serialize};

use crate::shadows::{data_types::ErrorResponse, error::ShadowError};

const MAX_TOPIC_LEN: usize = 128;
const MAX_PAYLOAD_SIZE: usize = 512;

pub trait ShadowState: Serialize {
    const NAME: Option<&'static str>;

    // TODO: Move MAX_PAYLOAD_SIZE here once const_generics supports it
    // const MAX_PAYLOAD_SIZE: usize;

    type PartialState: Serialize + DeserializeOwned + Default;

    fn apply_options(&mut self, opt: Self::PartialState);
}

#[derive(Debug, PartialEq, Eq)]
pub enum CallbackEvent {
    GetAccepted,
    GetRejected(error::ShadowError),
    UpdateRejected(error::ShadowError),
    DeleteAccepted,
    DeleteRejected(error::ShadowError),
}

#[derive(Debug, Default)]
pub struct Builder<S: ShadowState> {
    state: S,
}

impl<S> Builder<S>
where
    S: ShadowState,
{
    pub fn new(state: S) -> Self {
        Builder { state }
    }

    pub fn with_callback<'a, M: Mqtt, const C: usize>(
        self,
        mqtt: &'a M,
        callback_producer: heapless::spsc::Producer<'a, CallbackEvent, C>,
    ) -> Result<Shadow<'a, S, M, C>, Error> {
        let handler = Shadow {
            mqtt,
            state: self.state,
            in_sync: false,
            callback_producer: Some(callback_producer),
        };

        handler.init()?;
        Ok(handler)
    }

    pub fn build<'a, M: Mqtt>(self, mqtt: &'a M) -> Result<Shadow<'a, S, M, 0>, Error> {
        let handler = Shadow {
            mqtt,
            state: self.state,
            in_sync: false,
            callback_producer: None,
        };

        handler.init()?;
        Ok(handler)
    }
}

pub struct Shadow<'a, S: ShadowState, M: Mqtt, const C: usize> {
    state: S,
    mqtt: &'a M,
    callback_producer: Option<heapless::spsc::Producer<'a, CallbackEvent, C>>,
    in_sync: bool,
}

impl<'a, S, M, const C: usize> Shadow<'a, S, M, C>
where
    S: ShadowState,
    M: Mqtt,
{
    /// Subscribes to all the topics required for keeping a shadow in sync
    fn subscribe(&self) -> Result<(), Error> {
        Subscribe::<8>::new()
            .topic(Topic::DeleteAccepted, QoS::AtLeastOnce)
            .topic(Topic::DeleteRejected, QoS::AtLeastOnce)
            .topic(Topic::GetAccepted, QoS::AtLeastOnce)
            .topic(Topic::GetRejected, QoS::AtLeastOnce)
            .topic(Topic::UpdateAccepted, QoS::AtLeastOnce)
            .topic(Topic::UpdateRejected, QoS::AtLeastOnce)
            .topic(Topic::UpdateDelta, QoS::AtLeastOnce)
            .topic(Topic::UpdateDocuments, QoS::AtLeastOnce)
            .send(self.mqtt, S::NAME)?;

        Ok(())
    }

    /// Unsubscribes from all the topics required for keeping a shadow in sync
    fn unsubscribe(&self) -> Result<(), Error> {
        Unsubscribe::<8>::new()
            .topic(Topic::DeleteAccepted)
            .topic(Topic::DeleteRejected)
            .topic(Topic::GetAccepted)
            .topic(Topic::GetRejected)
            .topic(Topic::UpdateAccepted)
            .topic(Topic::UpdateRejected)
            .topic(Topic::UpdateDelta)
            .topic(Topic::UpdateDocuments)
            .send(self.mqtt, S::NAME)?;

        Ok(())
    }

    /// Initialize the shadow state by subscribing to necessary topics &
    /// publishing a GET request to initiate sync with the cloud
    fn init(&self) -> Result<(), Error> {
        self.subscribe()?;

        let get_topic = Topic::Get.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(get_topic.as_str(), b"", QoS::AtLeastOnce)?;
        Ok(())
    }

    #[must_use]
    pub fn handle_message(&mut self, topic: &str, payload: &[u8]) -> Result<(), Error> {
        let (topic, thing_name, shadow_name) =
            Topic::from_str(topic).ok_or(Error::WrongShadowName)?;

        assert_eq!(thing_name, self.mqtt.client_id());
        assert_eq!(topic.direction(), Direction::Incoming);

        if shadow_name != S::NAME {
            return Err(Error::WrongShadowName);
        }

        match topic {
            Topic::GetAccepted => {
                // The actions necessary to process the state document in the
                // message body.

                if let Some(ref mut producer) = self.callback_producer {
                    producer
                        .enqueue(CallbackEvent::GetAccepted)
                        .map_err(|_| Error::Overflow)?;
                }
            }
            Topic::GetRejected => {
                // Respond to the error message in the message body.

                if let Some(ref mut producer) = self.callback_producer {
                    let (error, _) = serde_json_core::from_slice::<ErrorResponse>(payload)
                        .map_err(|_| Error::InvalidPayload)?;
                    let get_rejected_err =
                        ShadowError::try_from(error).map_err(|_| Error::InvalidPayload)?;

                    producer
                        .enqueue(CallbackEvent::GetRejected(get_rejected_err))
                        .map_err(|_| Error::Overflow)?;
                }
            }
            Topic::UpdateDelta => {
                // Update the device's state to match the desired state in the
                // message body.
            }
            Topic::UpdateAccepted => {
                // Confirm the updated data in the message body matches the
                // device state.

                // TODO: What is the difference between `UpdateAccepted` & `UpdateDocuments`?
            }
            Topic::UpdateDocuments => {
                // Confirm the updated state in the message body matches the
                // device's state.

                let (opt, _) =
                    serde_json_core::from_slice(payload).map_err(|_| Error::InvalidPayload)?;
                self.state.apply_options(opt);
                self.in_sync = true;
            }
            Topic::UpdateRejected => {
                // Respond to the error message in the message body.

                if let Some(ref mut producer) = self.callback_producer {
                    let (error, _) = serde_json_core::from_slice::<ErrorResponse>(payload)
                        .map_err(|_| Error::InvalidPayload)?;
                    let update_rejected_err =
                        ShadowError::try_from(error).map_err(|_| Error::InvalidPayload)?;

                    producer
                        .enqueue(CallbackEvent::UpdateRejected(update_rejected_err))
                        .map_err(|_| Error::Overflow)?;
                }
            }
            Topic::DeleteAccepted => {
                // The actions necessary to accommodate the deleted shadow, such
                // as stop publishing updates.
                if let Some(ref mut producer) = self.callback_producer {
                    producer
                        .enqueue(CallbackEvent::DeleteAccepted)
                        .map_err(|_| Error::Overflow)?;
                }
            }
            Topic::DeleteRejected => {
                // Respond to the error message in the message body.

                if let Some(ref mut producer) = self.callback_producer {
                    let (error, _) = serde_json_core::from_slice::<ErrorResponse>(payload)
                        .map_err(|_| Error::InvalidPayload)?;
                    let delete_rejected_err =
                        ShadowError::try_from(error).map_err(|_| Error::InvalidPayload)?;

                    producer
                        .enqueue(CallbackEvent::DeleteRejected(delete_rejected_err))
                        .map_err(|_| Error::Overflow)?;
                }
            }
            _ => {
                // Not possible due to above assert on topic direction!
            }
        }

        Ok(())
    }

    // TODO: Does this make sense as `nb`? Test it out in actual application
    pub fn get(&self) -> nb::Result<&S, core::convert::Infallible> {
        if self.in_sync {
            Ok(&self.state)
        } else {
            Err(nb::Error::WouldBlock)
        }
    }

    /// Update the state of the shadow.
    ///
    /// This function will update the desired state of the shadow in the cloud,
    /// and depending on whether the state update is rejected or accepted, it
    /// will automatically update the local version after response
    pub fn update<F: FnOnce(&S, &mut S::PartialState)>(&mut self, f: F) -> Result<(), Error> {
        let mut desired = S::PartialState::default();
        f(&self.state, &mut desired);

        self.in_sync = false;

        // TODO:
        let reported_state = data_types::AcceptedResponse {
            state: data_types::Desired { desired },
            timestamp: 0,
            client_token: None,
            version: None,
        };

        let payload = serde_json_core::to_vec::<_, MAX_PAYLOAD_SIZE>(&reported_state)
            .map_err(|_| Error::Overflow)?;

        // send update
        let update_topic = Topic::Update.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(update_topic.as_str(), &payload, QoS::AtLeastOnce)?;

        Ok(())
    }

    pub fn delete(&mut self) -> Result<(), Error> {
        let delete_topic = Topic::Delete.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(delete_topic.as_str(), b"", QoS::AtLeastOnce)?;
        Ok(())
    }
}

impl<'a, S, M, const C: usize> Drop for Shadow<'a, S, M, C>
where
    S: ShadowState,
    M: Mqtt,
{
    fn drop(&mut self) {
        self.unsubscribe().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::MockMqtt;
    use optional_struct::OptionalStruct;
    use serde::Deserialize;

    // ########## START DERIVE(ShadowState) ##############

    #[derive(Debug, OptionalStruct, Default, Serialize)]
    #[optional_derive(Default, Deserialize, Serialize)]
    pub struct Config {
        pub id: u8,
    }

    impl ShadowState for Config {
        const NAME: Option<&'static str> = Some("dev_config");
        // const MAX_PAYLOAD_SIZE: usize = 512;

        type PartialState = OptionalConfig;

        fn apply_options(&mut self, opt: Self::PartialState) {
            self.apply_options(opt);
        }
    }

    // ########## END DERIVE(ShadowState) ##############

    const MAX_CALLBACK_ITEMS: usize = 10;

    #[test]
    fn default_construction_callback() {
        let mqtt = &MockMqtt::new();
        let mut queue = heapless::spsc::Queue::<CallbackEvent, MAX_CALLBACK_ITEMS>::new();
        let (producer, mut consumer) = queue.split();

        let mut config_default_handler = Builder::<Config>::default()
            .with_callback(mqtt, producer)
            .unwrap();

        let (name_prefix, shadow_name) = Config::NAME.map(|n| ("/name/", n)).unwrap_or_default();

        // Check that we have 2 subscribe packets + 1 publish packet (get request)
        assert_eq!(mqtt.tx.borrow_mut().len(), 3);
        mqtt.tx.borrow_mut().clear();

        config_default_handler
            .update(|_current, desired| desired.id = Some(7))
            .unwrap();

        config_default_handler
            .handle_message(
                &format!(
                    "$aws/things/{}/shadow{}{}/update/documents",
                    mqtt.client_id(),
                    name_prefix,
                    shadow_name
                ),
                &serde_json_core::to_vec::<_, MAX_PAYLOAD_SIZE>(&OptionalConfig::default())
                    .unwrap(),
            )
            .unwrap();

        assert_eq!(consumer.dequeue(), None);
    }

    #[test]
    fn test() {
        let mqtt = &MockMqtt::new();

        let config = Config::default();
        let mut config_handler = Builder::new(config).build(mqtt).unwrap();

        // Check that we have 2 subscribe packets + 1 publish packet (get request)
        assert_eq!(mqtt.tx.borrow_mut().len(), 3);
        mqtt.tx.borrow_mut().clear();

        config_handler
            .update(|_current, desired| desired.id = Some(7))
            .unwrap();

        // Check that we have 1 publish packet (update request)
        assert_eq!(mqtt.tx.borrow_mut().len(), 1);
    }
}
