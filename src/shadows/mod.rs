mod data_types;
mod error;
mod topics;

use core::convert::TryFrom;

pub use error::Error;
use topics::{Direction, Subscribe, Topic, Unsubscribe};

use mqttrust::{Mqtt, QoS};
use serde::{de::DeserializeOwned, Serialize};

use crate::shadows::{data_types::ErrorResponse, error::ShadowError};

pub trait ShadowState: Serialize {
    const NAME: Option<&'static str>;

    type PartialState: Serialize + DeserializeOwned + Default;

    fn apply_options(&mut self, opt: Self::PartialState);
}

pub trait ShadowCallback {
    fn notify(&mut self, event: CallbackEvent);
}

/// Dummy implementation for the case when no `CallbackHandler` is used.
impl ShadowCallback for () {
    fn notify(&mut self, _event: CallbackEvent) {}
}

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

    pub fn with_callback<'a, M: Mqtt, C: ShadowCallback>(
        self,
        mqtt: &'a M,
        callback_handler: C,
    ) -> Result<Shadow<'a, S, M, C>, Error> {
        let handler = Shadow {
            mqtt,
            state: self.state,
            in_sync: false,
            callback_handler: Some(callback_handler),
        };

        handler.subscribe()?;

        let get_topic = Topic::Get.format::<128>(mqtt.client_id(), S::NAME)?;
        handler
            .mqtt
            .publish(get_topic.as_str(), b"", QoS::AtLeastOnce)?;

        Ok(handler)
    }

    pub fn build<'a, M: Mqtt>(self, mqtt: &'a M) -> Result<Shadow<'a, S, M, ()>, Error> {
        let handler = Shadow {
            mqtt,
            state: self.state,
            in_sync: false,
            callback_handler: None,
        };

        handler.subscribe()?;

        let get_topic = Topic::Get.format::<128>(mqtt.client_id(), S::NAME)?;
        handler
            .mqtt
            .publish(get_topic.as_str(), b"", QoS::AtLeastOnce)?;

        Ok(handler)
    }
}

pub struct Shadow<'a, S: ShadowState, M: Mqtt, C: ShadowCallback> {
    state: S,
    mqtt: &'a M,
    callback_handler: Option<C>,
    in_sync: bool,
}

impl<'a, S, M, C> Shadow<'a, S, M, C>
where
    S: ShadowState,
    M: Mqtt,
    C: ShadowCallback,
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
                // The actions necessary to process
                // the state document in the
                // message body.

                if let Some(ref mut handler) = self.callback_handler {
                    handler.notify(CallbackEvent::GetAccepted);
                }
            }
            Topic::GetRejected => {
                // Respond to the error message in
                // the message body.

                if let Some(ref mut handler) = self.callback_handler {
                    let (error, _) = serde_json_core::from_slice::<ErrorResponse>(payload)
                        .map_err(|_| Error::InvalidPayload)?;
                    let get_rejected_err =
                        ShadowError::try_from(error).map_err(|_| Error::InvalidPayload)?;

                    handler.notify(CallbackEvent::GetRejected(get_rejected_err));
                }
            }
            Topic::UpdateDelta => {
                // Update the device's state to
                // match the desired state in the
                // message body.
            }
            Topic::UpdateAccepted => {
                // Confirm the updated data in
                // the message body matches the
                // device state.

                // TODO: What is the difference between `UpdateAccepted` & `UpdateDocuments`?
            }
            Topic::UpdateDocuments => {
                // Confirm the updated state in
                // the message body matches the
                // device's state.

                let (opt, _) =
                    serde_json_core::from_slice(payload).map_err(|_| Error::InvalidPayload)?;
                self.state.apply_options(opt);
                self.in_sync = true;
            }
            Topic::UpdateRejected => {
                // Respond to the error message in
                // the message body.

                if let Some(ref mut handler) = self.callback_handler {
                    let (error, _) = serde_json_core::from_slice::<ErrorResponse>(payload)
                        .map_err(|_| Error::InvalidPayload)?;
                    let update_rejected_err =
                        ShadowError::try_from(error).map_err(|_| Error::InvalidPayload)?;

                    handler.notify(CallbackEvent::UpdateRejected(update_rejected_err));
                }
            }
            Topic::DeleteAccepted => {
                // The actions necessary to
                // accommodate the deleted
                // shadow, such as stop publishing
                // updates.
                if let Some(ref mut handler) = self.callback_handler {
                    handler.notify(CallbackEvent::DeleteAccepted);
                }
            }
            Topic::DeleteRejected => {
                // Respond to the error message in
                // the message body.

                if let Some(ref mut handler) = self.callback_handler {
                    let (error, _) = serde_json_core::from_slice::<ErrorResponse>(payload)
                        .map_err(|_| Error::InvalidPayload)?;
                    let delete_rejected_err =
                        ShadowError::try_from(error).map_err(|_| Error::InvalidPayload)?;

                    handler.notify(CallbackEvent::DeleteRejected(delete_rejected_err));
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
    /// will automatically
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

        let payload =
            serde_json_core::to_vec::<_, 512>(&reported_state).map_err(|_| Error::Overflow)?;

        // send update
        let update_topic = Topic::Update.format::<128>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(update_topic.as_str(), &payload, QoS::AtLeastOnce)?;

        Ok(())
    }

    pub fn delete(&mut self) -> Result<(), Error> {
        let delete_topic = Topic::Delete.format::<128>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(delete_topic.as_str(), b"", QoS::AtLeastOnce)?;
        Ok(())
    }
}

impl<'a, S, M, C> Drop for Shadow<'a, S, M, C>
where
    S: ShadowState,
    M: Mqtt,
    C: ShadowCallback,
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

    #[derive(Debug, OptionalStruct, Default, Serialize)]
    #[optional_derive(Default, Deserialize, Serialize)]
    pub struct Config {
        pub id: u8,
    }

    impl ShadowState for Config {
        const NAME: Option<&'static str> = Some("dev_config");

        type PartialState = OptionalConfig;

        fn apply_options(&mut self, opt: Self::PartialState) {
            self.apply_options(opt);
        }
    }

    pub struct CallbackHandler;

    impl ShadowCallback for CallbackHandler {
        fn notify(&mut self, event: CallbackEvent) {
            match event {
                CallbackEvent::GetAccepted => {}
                CallbackEvent::DeleteAccepted => {}
                CallbackEvent::GetRejected(_e) => {
                    // Nothing to do here, but perhaps retry?
                }
                CallbackEvent::UpdateRejected(_e) => {
                    // Nothing to do here, but perhaps retry?
                }
                CallbackEvent::DeleteRejected(_e) => {
                    // Nothing to do here, but perhaps retry?
                }
            }
        }
    }

    #[test]
    fn test() {
        let mqtt = &MockMqtt::new();
        let config = Config::default();

        let mut config_handler = Builder::new(config).build(mqtt).unwrap();
        let _config_default_handler: Shadow<Config, _, _> = Builder::default()
            .with_callback(mqtt, CallbackHandler)
            .unwrap();

        config_handler
            .update(|_, desired| desired.id = Some(7))
            .unwrap();
    }
}
