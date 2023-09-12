pub mod dao;
pub mod data_types;
mod error;
mod shadow_diff;
pub mod topics;

use core::marker::PhantomData;

use mqttrust::{Mqtt, QoS};

pub use data_types::Patch;
pub use error::Error;
use serde::de::DeserializeOwned;
pub use shadow_derive as derive;
pub use shadow_diff::ShadowPatch;

use data_types::{AcceptedResponse, DeltaResponse, ErrorResponse};
use topics::{Direction, Subscribe, Topic, Unsubscribe};

use self::dao::ShadowDAO;

const MAX_TOPIC_LEN: usize = 128;
const PARTIAL_REQUEST_OVERHEAD: usize = 64;
const CLASSIC_SHADOW: &str = "Classic";

pub trait ShadowState: ShadowPatch {
    const NAME: Option<&'static str>;

    const MAX_PAYLOAD_SIZE: usize = 512;
}

struct ShadowHandler<'a, M: Mqtt, S: ShadowState>
where
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    mqtt: &'a M,
    _shadow: PhantomData<S>,
}

impl<'a, M: Mqtt, S: ShadowState> ShadowHandler<'a, M, S>
where
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    /// Subscribes to all the topics required for keeping a shadow in sync
    pub fn subscribe(&self) -> Result<(), Error> {
        Subscribe::<7>::new()
            .topic(Topic::GetAccepted, QoS::AtLeastOnce)
            .topic(Topic::GetRejected, QoS::AtLeastOnce)
            .topic(Topic::DeleteAccepted, QoS::AtLeastOnce)
            .topic(Topic::DeleteRejected, QoS::AtLeastOnce)
            .topic(Topic::UpdateAccepted, QoS::AtLeastOnce)
            .topic(Topic::UpdateRejected, QoS::AtLeastOnce)
            .topic(Topic::UpdateDelta, QoS::AtLeastOnce)
            .send(self.mqtt, S::NAME)?;

        Ok(())
    }

    /// Unsubscribes from all the topics required for keeping a shadow in sync
    pub fn unsubscribe(&self) -> Result<(), Error> {
        Unsubscribe::<7>::new()
            .topic(Topic::GetAccepted)
            .topic(Topic::GetRejected)
            .topic(Topic::DeleteAccepted)
            .topic(Topic::DeleteRejected)
            .topic(Topic::UpdateAccepted)
            .topic(Topic::UpdateRejected)
            .topic(Topic::UpdateDelta)
            .send(self.mqtt, S::NAME)?;

        Ok(())
    }

    /// Helper function to check whether a topic name is relevant for this
    /// particular shadow.
    pub fn should_handle_topic(&mut self, topic: &str) -> bool {
        if let Some((_, thing_name, shadow_name)) = Topic::from_str(topic) {
            return thing_name == self.mqtt.client_id() && shadow_name == S::NAME;
        }
        false
    }

    /// Internal helper function for applying a delta state to the actual shadow
    /// state, and update the cloud shadow.
    fn change_shadow_value(
        &mut self,
        state: &mut S,
        delta: Option<S::PatchState>,
        update_desired: Option<bool>,
    ) -> Result<(), Error> {
        if let Some(ref delta) = delta {
            state.apply_patch(delta.clone());
        }

        debug!(
            "[{:?}] Updating reported shadow value. Update_desired: {:?}",
            S::NAME.unwrap_or_else(|| CLASSIC_SHADOW),
            update_desired
        );

        if let Some(update_desired) = update_desired {
            let desired = if update_desired { Some(&state) } else { None };

            let request = data_types::Request {
                state: data_types::State {
                    reported: Some(&state),
                    desired,
                },
                client_token: None,
                version: None,
            };

            let payload = serde_json_core::to_vec::<
                _,
                { S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD },
            >(&request)
            .map_err(|_| Error::Overflow)?;

            let update_topic =
                Topic::Update.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
            self.mqtt
                .publish(update_topic.as_str(), &payload, QoS::AtLeastOnce)?;
        }

        Ok(())
    }

    /// Initiate a `GetShadow` request, updating the local state from the cloud.
    pub fn get_shadow(&self) -> Result<(), Error> {
        let get_topic = Topic::Get.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(get_topic.as_str(), b"", QoS::AtLeastOnce)?;
        Ok(())
    }

    pub fn delete_shadow(&mut self) -> Result<(), Error> {
        let delete_topic = Topic::Delete.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(delete_topic.as_str(), b"", QoS::AtLeastOnce)?;
        Ok(())
    }
}

pub struct PersistedShadow<'a, S: ShadowState + DeserializeOwned, M: Mqtt, D: ShadowDAO<S>>
where
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    handler: ShadowHandler<'a, M, S>,
    pub(crate) dao: D,
}

impl<'a, S, M, D> PersistedShadow<'a, S, M, D>
where
    S: ShadowState + DeserializeOwned,
    M: Mqtt,
    D: ShadowDAO<S>,
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    /// Instantiate a new shadow that will be automatically persisted to NVM
    /// based on the passed `DAO`.
    pub fn new(
        initial_state: S,
        mqtt: &'a M,
        mut dao: D,
        auto_subscribe: bool,
    ) -> Result<Self, Error> {
        if dao.read().is_err() {
            dao.write(&initial_state)?;
        }

        let handler = ShadowHandler {
            mqtt,
            _shadow: PhantomData,
        };
        if auto_subscribe {
            handler.subscribe()?;
        }
        Ok(Self { handler, dao })
    }

    /// Subscribes to all the topics required for keeping a shadow in sync
    pub fn subscribe(&self) -> Result<(), Error> {
        self.handler.subscribe()
    }

    /// Unsubscribes from all the topics required for keeping a shadow in sync
    pub fn unsubscribe(&self) -> Result<(), Error> {
        self.handler.unsubscribe()
    }

    /// Helper function to check whether a topic name is relevant for this
    /// particular shadow.
    pub fn should_handle_topic(&mut self, topic: &str) -> bool {
        self.handler.should_handle_topic(topic)
    }

    /// Handle incomming publish messages from the cloud on any topics relevant
    /// for this particular shadow.
    ///
    /// This function needs to be fed all relevant incoming MQTT payloads in
    /// order for the shadow manager to work.
    #[must_use]
    pub fn handle_message(
        &mut self,
        topic: &str,
        payload: &[u8],
    ) -> Result<(S, Option<S::PatchState>), Error> {
        let (topic, thing_name, shadow_name) =
            Topic::from_str(topic).ok_or(Error::WrongShadowName)?;

        assert_eq!(thing_name, self.handler.mqtt.client_id());
        assert_eq!(topic.direction(), Direction::Incoming);

        if shadow_name != S::NAME {
            return Err(Error::WrongShadowName);
        }

        let mut state = self.dao.read()?;

        let delta = match topic {
            Topic::GetAccepted => {
                // The actions necessary to process the state document in the
                // message body.
                serde_json_core::from_slice::<AcceptedResponse<S::PatchState>>(payload)
                    .map_err(|_| Error::InvalidPayload)
                    .and_then(|(response, _)| {
                        if let Some(_) = response.state.delta {
                            debug!(
                                "[{:?}] Received delta state",
                                S::NAME.unwrap_or_else(|| CLASSIC_SHADOW)
                            );
                            self.handler.change_shadow_value(
                                &mut state,
                                response.state.delta.clone(),
                                Some(false),
                            )?;
                        } else if let Some(_) = response.state.reported {
                            self.handler.change_shadow_value(
                                &mut state,
                                response.state.reported,
                                None,
                            )?;
                        }
                        Ok(response.state.delta)
                    })?
            }
            Topic::GetRejected | Topic::UpdateRejected => {
                // Respond to the error message in the message body.
                if let Ok((error, _)) = serde_json_core::from_slice::<ErrorResponse>(payload) {
                    if error.code == 404 && matches!(topic, Topic::GetRejected) {
                        debug!(
                            "[{:?}] Thing has no shadow document. Creating with defaults...",
                            S::NAME.unwrap_or_else(|| CLASSIC_SHADOW)
                        );
                        self.report_shadow()?;
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
                None
            }
            Topic::UpdateDelta => {
                // Update the device's state to match the desired state in the
                // message body.
                debug!(
                    "[{:?}] Received shadow delta event.",
                    S::NAME.unwrap_or_else(|| CLASSIC_SHADOW),
                );

                serde_json_core::from_slice::<DeltaResponse<S::PatchState>>(payload)
                    .map_err(|_| Error::InvalidPayload)
                    .and_then(|(delta, _)| {
                        if let Some(_) = delta.state {
                            debug!(
                                "[{:?}] Delta reports new desired value. Changing local value...",
                                S::NAME.unwrap_or_else(|| CLASSIC_SHADOW),
                            );
                        }
                        self.handler.change_shadow_value(
                            &mut state,
                            delta.state.clone(),
                            Some(false),
                        )?;
                        Ok(delta.state)
                    })?
            }
            Topic::UpdateAccepted => {
                // Confirm the updated data in the message body matches the
                // device state.

                debug!(
                    "[{:?}] Finished updating reported shadow value.",
                    S::NAME.unwrap_or_else(|| CLASSIC_SHADOW)
                );

                None
            }
            _ => None,
        };

        // Something has changed as part of handling a message. Persist it
        // to NVM storage.
        if delta.is_some() {
            self.dao.write(&state)?;
        }

        Ok((state, delta))
    }

    /// Get an immutable reference to the internal local state.
    pub fn try_get(&mut self) -> Result<S, Error> {
        self.dao.read()
    }

    /// Initiate a `GetShadow` request, updating the local state from the cloud.
    pub fn get_shadow(&self) -> Result<(), Error> {
        self.handler.get_shadow()
    }

    /// Initiate an `UpdateShadow` request, reporting the local state to the cloud.
    pub fn report_shadow(&mut self) -> Result<(), Error> {
        let mut state = self.dao.read()?;
        self.handler
            .change_shadow_value(&mut state, None, Some(false))?;
        Ok(())
    }

    /// Update the state of the shadow.
    ///
    /// This function will update the desired state of the shadow in the cloud,
    /// and depending on whether the state update is rejected or accepted, it
    /// will automatically update the local version after response
    pub fn update<F: FnOnce(&S, &mut S::PatchState)>(&mut self, f: F) -> Result<(), Error> {
        let mut desired = S::PatchState::default();
        let mut state = self.dao.read()?;
        f(&state, &mut desired);

        self.handler
            .change_shadow_value(&mut state, Some(desired), Some(false))?;
        self.dao.write(&state)?;

        Ok(())
    }

    pub fn delete_shadow(&mut self) -> Result<(), Error> {
        self.handler.delete_shadow()
    }
}

pub struct Shadow<'a, S: ShadowState, M: Mqtt>
where
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    state: S,
    handler: ShadowHandler<'a, M, S>,
}

impl<'a, S, M> Shadow<'a, S, M>
where
    S: ShadowState,
    M: Mqtt,
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    /// Instantiate a new non-persisted shadow
    pub fn new(state: S, mqtt: &'a M, auto_subscribe: bool) -> Result<Self, Error> {
        let handler = ShadowHandler {
            mqtt,
            _shadow: PhantomData,
        };
        if auto_subscribe {
            handler.subscribe()?;
        }
        Ok(Self { handler, state })
    }

    /// Subscribes to all the topics required for keeping a shadow in sync
    pub fn subscribe(&self) -> Result<(), Error> {
        self.handler.subscribe()
    }

    /// Unsubscribes from all the topics required for keeping a shadow in sync
    pub fn unsubscribe(&self) -> Result<(), Error> {
        self.handler.unsubscribe()
    }

    /// Handle incomming publish messages from the cloud on any topics relevant
    /// for this particular shadow.
    ///
    /// This function needs to be fed all relevant incoming MQTT payloads in
    /// order for the shadow manager to work.
    #[must_use]
    pub fn handle_message(
        &mut self,
        topic: &str,
        payload: &[u8],
    ) -> Result<(&S, Option<S::PatchState>), Error> {
        let (topic, thing_name, shadow_name) =
            Topic::from_str(topic).ok_or(Error::WrongShadowName)?;

        assert_eq!(thing_name, self.handler.mqtt.client_id());
        assert_eq!(topic.direction(), Direction::Incoming);

        if shadow_name != S::NAME {
            return Err(Error::WrongShadowName);
        }

        let delta = match topic {
            Topic::GetAccepted => {
                // The actions necessary to process the state document in the
                // message body.
                serde_json_core::from_slice::<AcceptedResponse<S::PatchState>>(payload)
                    .map_err(|_| Error::InvalidPayload)
                    .and_then(|(response, _)| {
                        if let Some(_) = response.state.delta {
                            debug!(
                                "[{:?}] Received delta state",
                                S::NAME.unwrap_or_else(|| CLASSIC_SHADOW)
                            );
                            self.handler.change_shadow_value(
                                &mut self.state,
                                response.state.delta.clone(),
                                Some(false),
                            )?;
                        } else if let Some(_) = response.state.reported {
                            self.handler.change_shadow_value(
                                &mut self.state,
                                response.state.reported,
                                None,
                            )?;
                        }
                        Ok(response.state.delta)
                    })?
            }
            Topic::GetRejected | Topic::UpdateRejected => {
                // Respond to the error message in the message body.
                if let Ok((error, _)) = serde_json_core::from_slice::<ErrorResponse>(payload) {
                    if error.code == 404 && matches!(topic, Topic::GetRejected) {
                        debug!(
                            "[{:?}] Thing has no shadow document. Creating with defaults...",
                            S::NAME.unwrap_or_else(|| CLASSIC_SHADOW)
                        );
                        self.report_shadow()?;
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
                None
            }
            Topic::UpdateDelta => {
                // Update the device's state to match the desired state in the
                // message body.
                debug!(
                    "[{:?}] Received shadow delta event.",
                    S::NAME.unwrap_or_else(|| CLASSIC_SHADOW),
                );

                serde_json_core::from_slice::<DeltaResponse<S::PatchState>>(payload)
                    .map_err(|_| Error::InvalidPayload)
                    .and_then(|(delta, _)| {
                        if let Some(_) = delta.state {
                            debug!(
                                "[{:?}] Delta reports new desired value. Changing local value...",
                                S::NAME.unwrap_or_else(|| CLASSIC_SHADOW),
                            );
                        }
                        self.handler.change_shadow_value(
                            &mut self.state,
                            delta.state.clone(),
                            Some(false),
                        )?;
                        Ok(delta.state)
                    })?
            }
            Topic::UpdateAccepted => {
                // Confirm the updated data in the message body matches the
                // device state.

                debug!(
                    "[{:?}] Finished updating reported shadow value.",
                    S::NAME.unwrap_or_else(|| CLASSIC_SHADOW)
                );

                None
            }
            _ => None,
        };

        Ok((self.get(), delta))
    }

    /// Get an immutable reference to the internal local state.
    pub fn get(&self) -> &S {
        &self.state
    }

    /// Initiate an `UpdateShadow` request, reporting the local state to the cloud.
    pub fn report_shadow(&mut self) -> Result<(), Error> {
        self.handler
            .change_shadow_value(&mut self.state, None, Some(false))?;
        Ok(())
    }

    /// Update the state of the shadow.
    ///
    /// This function will update the desired state of the shadow in the cloud,
    /// and depending on whether the state update is rejected or accepted, it
    /// will automatically update the local version after response
    pub fn update<F: FnOnce(&S, &mut S::PatchState)>(&mut self, f: F) -> Result<(), Error> {
        let mut desired = S::PatchState::default();
        f(&self.state, &mut desired);

        self.handler
            .change_shadow_value(&mut self.state, Some(desired), Some(false))?;

        Ok(())
    }

    /// Initiate a `GetShadow` request, updating the local state from the cloud.
    pub fn get_shadow(&self) -> Result<(), Error> {
        self.handler.get_shadow()
    }

    pub fn delete_shadow(&mut self) -> Result<(), Error> {
        self.handler.delete_shadow()
    }
}

impl<'a, S, M> core::fmt::Debug for Shadow<'a, S, M>
where
    S: ShadowState + core::fmt::Debug,
    M: Mqtt,
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "[{:?}] = {:?}",
            S::NAME.unwrap_or_else(|| CLASSIC_SHADOW),
            self.get()
        )
    }
}

#[cfg(feature = "defmt")]
impl<'a, S, M> defmt::Format for Shadow<'a, S, M>
where
    S: ShadowState + defmt::Format,
    M: Mqtt,
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    fn format(&self, fmt: defmt::Formatter) {
        defmt::write!(
            fmt,
            "[{:?}] = {:?}",
            S::NAME.unwrap_or_else(|| CLASSIC_SHADOW),
            self.get()
        )
    }
}

impl<'a, S, M> Drop for Shadow<'a, S, M>
where
    S: ShadowState,
    M: Mqtt,
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    fn drop(&mut self) {
        self.unsubscribe().ok();
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate as rustot;
//     use crate::test::MockMqtt;
//     use dao::StdIODAO;
//     use derive::ShadowState;
//     use serde::{Deserialize, Serialize};

//     // #[derive(Debug, Default, Clone, Serialize, ShadowDiff, Deserialize, PartialEq)]
//     // pub struct Test {}

//     #[derive(Debug, Default, Serialize, Deserialize, ShadowState, PartialEq)]
//     pub struct SerdeRename {
//         #[serde(rename = "SomeRenamedField")]
//         #[unit_shadow_field]
//         some_renamed_field: u8,
//     }

//     #[derive(Debug, Default, Serialize, Deserialize, ShadowState, PartialEq)]
//     #[serde(rename_all = "UPPERCASE")]
//     pub struct SerdeRenameAll {
//         test: u8,
//     }

//     #[derive(Debug, Default, Serialize, Deserialize, ShadowState, PartialEq)]
//     #[shadow("config")]
//     pub struct Config {
//         id: u8,
//     }

//     #[test]
//     fn shadow_name() {
//         assert_eq!(<Config as ShadowState>::NAME, Some("config"))
//     }

//     #[test]
//     fn serde_rename() {
//         const RENAMED_STATE_FIELD: &[u8] = b"{\"SomeRenamedField\":  100}";
//         const RENAMED_STATE_ALL: &[u8] = b"{\"TEST\":  100}";

//         let (state, _) = serde_json_core::from_slice::<<SerdeRename as ShadowDiff>::PatchState>(
//             RENAMED_STATE_FIELD,
//         )
//         .unwrap();

//         assert!(state.some_renamed_field.is_some());

//         let (state, _) = serde_json_core::from_slice::<<SerdeRenameAll as ShadowDiff>::PatchState>(
//             RENAMED_STATE_ALL,
//         )
//         .unwrap();

//         assert!(state.test.is_some());
//     }

//     #[test]
//     fn handles_additional_fields() {
//         const JSON_PATCH: &[u8] =
//             b"{\"state\": {\"id\": 100, \"extra_field\": 123}, \"timestamp\": 12345}";

//         let mqtt = &MockMqtt::new();

//         let config = Config::default();
//         let mut config_shadow = Shadow::new(config, mqtt).unwrap();

//         mqtt.tx.borrow_mut().clear();

//         let (updated, _) = config_shadow
//             .handle_message(
//                 &format!(
//                     "$aws/things/{}/shadow/{}update/delta",
//                     mqtt.client_id(),
//                     <Config as ShadowState>::NAME
//                         .map_or_else(|| "".to_string(), |n| format!("name/{}/", n))
//                 ),
//                 JSON_PATCH,
//             )
//             .expect("handle additional fields in received delta");

//         assert_eq!(mqtt.tx.borrow_mut().len(), 1);

//         assert_eq!(updated, &Config { id: 100 });
//     }

//     #[test]
//     fn initialization_packets() {
//         let mqtt = &MockMqtt::new();

//         let config = Config::default();
//         let mut config_shadow = Shadow::new(config, mqtt).unwrap();

//         config_shadow.get_shadow().unwrap();

//         // Check that we have 2 subscribe packets + 1 publish packet (get request)
//         assert_eq!(mqtt.tx.borrow_mut().len(), 2);
//         mqtt.tx.borrow_mut().clear();

//         config_shadow
//             .update(|_current, desired| desired.id = Some(7))
//             .unwrap();

//         // Check that we have 1 publish packet (update request)
//         assert_eq!(mqtt.tx.borrow_mut().len(), 1);
//     }

//     #[test]
//     fn persists_state() {
//         let mqtt = &MockMqtt::new();
//         let config = Config::default();

//         let storage = std::io::Cursor::new(std::vec::Vec::with_capacity(1024));
//         let shadow_dao = StdIODAO::new(storage);
//         let mut config_shadow = PersistedShadow::new(config, mqtt, shadow_dao).unwrap();

//         mqtt.tx.borrow_mut().clear();

//         config_shadow
//             .update(|_, desired| {
//                 desired.id = Some(100);
//             })
//             .unwrap();

//         let updated = config_shadow.get();

//         assert_eq!(mqtt.tx.borrow_mut().len(), 1);
//         assert_eq!(updated, &Config { id: 100 });

//         let dao = config_shadow.dao;
//         assert_eq!(dao.0.position(), 10);

//         let dao_storage = dao.0.into_inner();
//         assert_eq!(dao_storage.len(), 10);
//         assert_eq!(&dao_storage, b"{\"id\":100}");
//     }
// }
