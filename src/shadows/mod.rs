mod data_types;
mod error;
mod topics;

use mqttrust::{Mqtt, QoS};
use serde::{de::DeserializeOwned, Serialize};

pub use error::Error;

pub use shadow_derive as derive;

use topics::{Subscribe, Topic, Unsubscribe};

use crate::shadows::{data_types::{AcceptedResponse, DeltaResponse, ErrorResponse}, topics::Direction};

const MAX_TOPIC_LEN: usize = 128;
const MAX_PAYLOAD_SIZE: usize = 512;
const PARTIAL_REQUEST_OVERHEAD: usize = 64;

pub trait ShadowState: Serialize {
    const NAME: Option<&'static str>;

    // TODO: Move MAX_PAYLOAD_SIZE here once const_generics supports it
    // const MAX_PAYLOAD_SIZE: usize;

    type PartialState: Serialize + DeserializeOwned + Default + Clone;

    fn apply_patch(&mut self, opt: Self::PartialState);
}

pub struct Shadow<'a, S: ShadowState, M: Mqtt> {
    state: S,
    mqtt: &'a M,
}

impl<'a, S, M> Shadow<'a, S, M>
where
    S: ShadowState,
    M: Mqtt,
{
    pub fn new(state: S, mqtt: &'a M) -> Result<Self, Error> {
        let handler = Shadow { mqtt, state };

        handler.subscribe()?;
        Ok(handler)
    }

    /// Subscribes to all the topics required for keeping a shadow in sync
    fn subscribe(&self) -> Result<(), Error> {
        Subscribe::<5>::new()
            .topic(Topic::GetAccepted, QoS::AtLeastOnce)
            .topic(Topic::GetRejected, QoS::AtLeastOnce)
            .topic(Topic::UpdateAccepted, QoS::AtLeastOnce)
            .topic(Topic::UpdateRejected, QoS::AtLeastOnce)
            .topic(Topic::UpdateDelta, QoS::AtLeastOnce)
            .send(self.mqtt, S::NAME)?;

        Ok(())
    }

    /// Unsubscribes from all the topics required for keeping a shadow in sync
    fn unsubscribe(&self) -> Result<(), Error> {
        Unsubscribe::<5>::new()
            .topic(Topic::GetAccepted)
            .topic(Topic::GetRejected)
            .topic(Topic::UpdateAccepted)
            .topic(Topic::UpdateRejected)
            .topic(Topic::UpdateDelta)
            .send(self.mqtt, S::NAME)?;

        Ok(())
    }

    /// Internal helper function for applying a delta state to the actual shadow
    /// state, and update the cloud shadow.
    fn change_shadow_value(
        &mut self,
        delta: S::PartialState,
        update_desired: Option<bool>,
    ) -> Result<(), Error> {
        self.state.apply_patch(delta.clone());

        debug!(
            "[{:?}] Updating reported shadow value.",
            S::NAME.unwrap_or_else(|| "Classic")
        );

        if let Some(update_desired) = update_desired {
            let request = data_types::Request {
                state: data_types::State {
                    reported: Some(delta.clone()),
                    desired: update_desired.then(|| delta),
                },
                client_token: None,
                version: None,
            };

            let payload = serde_json_core::to_vec::<
                _,
                { MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD },
            >(&request)
            .map_err(|_| Error::Overflow)?;

            let update_topic =
                Topic::Update.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
            self.mqtt
                .publish(update_topic.as_str(), &payload, QoS::AtLeastOnce)?;
        }

        Ok(())
    }

    #[must_use]
    pub fn handle_message(&mut self, topic: &str, payload: &[u8]) -> Result<Option<&S>, Error> {
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
                match serde_json_core::from_slice::<AcceptedResponse<S::PartialState>>(payload) {
                    Ok((response, _)) => {
                        if let Some(delta) = response.state.delta {
                            self.change_shadow_value(delta, Some(false))?;
                        } else if let Some(reported) = response.state.reported {
                            self.change_shadow_value(reported, None)?;
                        }
                    }
                    _ => {}
                }

                return Ok(Some(self.get()));
            }
            Topic::GetRejected | Topic::UpdateRejected => {
                // Respond to the error message in the message body.
                match serde_json_core::from_slice::<ErrorResponse>(payload) {
                    Ok((error, _)) => {
                        if error.code == 404 && matches!(topic, Topic::GetRejected) {
                            debug!(
                                "[{:?}] Thing has no shadow document. Creating with defaults...",
                                S::NAME.unwrap_or_else(|| "Classic")
                            )
                        } else {
                            error!(
                                "{:?} request was rejected. code: {:?} message:'{:?}'",
                                if matches!(topic, Topic::GetRejected) {
                                    "Get"
                                } else {
                                    "Update"
                                },
                                error.code,
                                error.message
                            );
                        }
                    }
                    _ => {}
                }
            }
            Topic::UpdateDelta => {
                // Update the device's state to match the desired state in the
                // message body.
                debug!(
                    "[{:?}] Received shadow delta event.",
                    S::NAME.unwrap_or_else(|| "Classic")
                );

                match serde_json_core::from_slice::<DeltaResponse<S::PartialState>>(payload) {
                    Ok((delta, _)) => {
                        if let Some(delta) = delta.state {
                            debug!(
                                "[{:?}] Delta reports new desired value. Changing local value...",
                                S::NAME.unwrap_or_else(|| "Classic")
                            );
                            self.change_shadow_value(delta, Some(false))?;
                            return Ok(Some(self.get()));
                        }
                    }
                    _ => {}
                }
            }
            Topic::UpdateAccepted => {
                // Confirm the updated data in the message body matches the
                // device state.
                debug!(
                    "[{:?}] Finished updating reported shadow value.",
                    S::NAME.unwrap_or_else(|| "Classic")
                );
            }
            _ => {}
        }

        Ok(None)
    }

    pub fn get(&self) -> &S {
        &self.state
    }

    pub fn get_shadow(&self) -> Result<(), Error> {
        let get_topic = Topic::Get.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(get_topic.as_str(), b"", QoS::AtLeastOnce)?;
        Ok(())
    }

    /// Update the state of the shadow.
    ///
    /// This function will update the desired state of the shadow in the cloud,
    /// and depending on whether the state update is rejected or accepted, it
    /// will automatically update the local version after response
    pub fn update<F: FnOnce(&S, &mut S::PartialState)>(&mut self, f: F) -> Result<(), Error> {
        let mut desired = S::PartialState::default();
        f(&self.state, &mut desired);

        self.change_shadow_value(desired, Some(true))?;

        Ok(())
    }

    // pub fn delete(&mut self) -> Result<(), Error> {
    //     let delete_topic = Topic::Delete.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
    //     self.mqtt
    //         .publish(delete_topic.as_str(), b"", QoS::AtLeastOnce)?;
    //     Ok(())
    // }
}

impl<'a, S, M> Drop for Shadow<'a, S, M>
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
    use crate as rustot;
    use crate::test::MockMqtt;
    use serde::Deserialize;
    use derive::ShadowState;

    #[derive(Debug, Default, Serialize, ShadowState, PartialEq)]
    pub struct SerdeRename {
        #[serde(rename = "SomeRenamedField")]
        some_renamed_field: u8,
    }

    #[derive(Debug, Default, Serialize, ShadowState, PartialEq)]
    #[serde(rename_all = "UPPERCASE")]
    pub struct SerdeRenameAll {
        test: u8,
    }

    #[derive(Debug, Default, Serialize, ShadowState, PartialEq)]
    #[shadow("config")]
    pub struct Config {
        id: u8,
    }

    #[test]
    fn shadow_name() {
        assert_eq!(<Config as ShadowState>::NAME, Some("config"))
    }

    #[test]
    fn serde_rename() {
        const RENAMED_STATE_FIELD: &[u8] = b"{\"SomeRenamedField\":  100}";
        const RENAMED_STATE_ALL: &[u8] = b"{\"TEST\":  100}";

        let (state, _) = serde_json_core::from_slice::<<SerdeRename as ShadowState>::PartialState>(
            RENAMED_STATE_FIELD,
        )
        .unwrap();

        assert!(state.some_renamed_field.is_some());

        let (state, _) =
            serde_json_core::from_slice::<<SerdeRenameAll as ShadowState>::PartialState>(
                RENAMED_STATE_ALL,
            )
            .unwrap();

        assert!(state.test.is_some());
    }

    #[test]
    fn handles_additional_fields() {
        const JSON_PATCH: &[u8] =
            b"{\"state\": {\"id\": 100, \"extra_field\": 123}, \"timestamp\": 12345}";

        let mqtt = &MockMqtt::new();

        let config = Config::default();
        let mut config_shadow = Shadow::new(config, mqtt).unwrap();

        mqtt.tx.borrow_mut().clear();

        let updated = config_shadow
            .handle_message(
                &format!(
                    "$aws/things/{}/shadow/{}update/delta",
                    mqtt.client_id(),
                    <Config as ShadowState>::NAME
                        .map_or_else(|| "".to_string(), |n| format!("name/{}/", n))
                ),
                JSON_PATCH,
            )
            .expect("handle additional fields in received delta");

        assert_eq!(mqtt.tx.borrow_mut().len(), 1);

        assert_eq!(updated, Some(&Config { id: 100 }));
    }

    #[test]
    fn initialization_packets() {
        let mqtt = &MockMqtt::new();

        let config = Config::default();
        let mut config_shadow = Shadow::new(config, mqtt).unwrap();

        config_shadow.get_shadow().unwrap();

        // Check that we have 2 subscribe packets + 1 publish packet (get request)
        assert_eq!(mqtt.tx.borrow_mut().len(), 2);
        mqtt.tx.borrow_mut().clear();

        config_shadow
            .update(|_current, desired| desired.id = Some(7))
            .unwrap();

        // Check that we have 1 publish packet (update request)
        assert_eq!(mqtt.tx.borrow_mut().len(), 1);
    }
}
