pub mod dao;
pub mod data_types;
mod error;
mod shadow_diff;
pub mod topics;

use core::marker::PhantomData;

pub use data_types::Patch;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_mqtt::{Publish, QoS, RetainHandling, Subscribe, SubscribeTopic};
pub use error::Error;
use serde::de::DeserializeOwned;
pub use shadow_derive as derive;
pub use shadow_diff::ShadowPatch;

use data_types::DeltaResponse;
use topics::Topic;

use self::dao::ShadowDAO;

const MAX_TOPIC_LEN: usize = 128;
const PARTIAL_REQUEST_OVERHEAD: usize = 64;
const CLASSIC_SHADOW: &str = "Classic";

pub trait ShadowState: ShadowPatch {
    const NAME: Option<&'static str>;

    const MAX_PAYLOAD_SIZE: usize = 512;
}

struct ShadowHandler<'a, 'm, M: RawMutex, S: ShadowState, const SUBS: usize>
where
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    mqtt: &'m embedded_mqtt::MqttClient<'a, M, SUBS>,
    subscription: Option<embedded_mqtt::Subscription<'a, 'm, M, SUBS, 1>>,
    _shadow: PhantomData<S>,
}

impl<'a, 'm, M: RawMutex, S: ShadowState, const SUBS: usize> ShadowHandler<'a, 'm, M, S, SUBS>
where
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    async fn handle_delta(
        &mut self,
        current_state: &mut S,
    ) -> Result<Option<S::PatchState>, Error> {
        let delta_subscription = if self.subscription.is_some() {
            self.subscription.as_mut().unwrap()
        } else {
            self.mqtt.wait_connected().await;

            let sub = self
                .mqtt
                .subscribe::<1>(Subscribe::new(&[SubscribeTopic {
                    topic_path: topics::Topic::UpdateDelta
                        .format::<64>(self.mqtt.client_id(), S::NAME)?
                        .as_str(),
                    maximum_qos: QoS::AtLeastOnce,
                    no_local: false,
                    retain_as_published: false,
                    retain_handling: RetainHandling::SendAtSubscribeTime,
                }]))
                .await
                .map_err(Error::MqttError)?;
            self.subscription.insert(sub)
        };

        let delta_message = delta_subscription
            .next_message()
            .await
            .ok_or(Error::InvalidPayload)?;

        // Update the device's state to match the desired state in the
        // message body.
        debug!(
            "[{:?}] Received shadow delta event.",
            S::NAME.unwrap_or(CLASSIC_SHADOW),
        );

        match serde_json_core::from_slice::<DeltaResponse<S::PatchState>>(delta_message.payload()) {
            Ok((delta, _)) => {
                if delta.state.is_some() {
                    debug!(
                        "[{:?}] Delta reports new desired value. Changing local value...",
                        S::NAME.unwrap_or(CLASSIC_SHADOW),
                    );
                }
                self.change_shadow_value(current_state, delta.state.clone(), Some(false))
                    .await?;
                Ok(delta.state)
            }
            Err(_) => Err(Error::InvalidPayload),
        }
    }

    /// Internal helper function for applying a delta state to the actual shadow
    /// state, and update the cloud shadow.
    async fn change_shadow_value(
        &mut self,
        state: &mut S,
        delta: Option<S::PatchState>,
        update_desired: Option<bool>,
    ) -> Result<(), Error> {
        if let Some(delta) = delta {
            state.apply_patch(delta);
        }

        debug!(
            "[{:?}] Updating reported shadow value. Update_desired: {:?}",
            S::NAME.unwrap_or(CLASSIC_SHADOW),
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

            // FIXME: Serialize directly into the publish payload through `DeferredPublish` API
            let payload = serde_json_core::to_vec::<
                _,
                { S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD },
            >(&request)
            .map_err(|_| Error::Overflow)?;

            let update_topic =
                Topic::Update.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;

            self.mqtt
                .publish(Publish {
                    dup: false,
                    qos: QoS::AtLeastOnce,
                    retain: false,
                    pid: None,
                    topic_name: update_topic.as_str(),
                    payload: payload.as_slice(),
                    properties: embedded_mqtt::Properties::Slice(&[]),
                })
                .await
                .map_err(Error::MqttError)?;
        }

        Ok(())
    }

    /// Initiate a `GetShadow` request, updating the local state from the cloud.
    pub async fn get_shadow(&self) -> Result<(), Error> {
        let get_topic = Topic::Get.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(Publish {
                dup: false,
                qos: QoS::AtLeastOnce,
                retain: false,
                pid: None,
                topic_name: get_topic.as_str(),
                payload: b"",
                properties: embedded_mqtt::Properties::Slice(&[]),
            })
            .await
            .map_err(Error::MqttError)?;
        Ok(())
    }

    pub async fn delete_shadow(&mut self) -> Result<(), Error> {
        let delete_topic = Topic::Delete.format::<MAX_TOPIC_LEN>(self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(Publish {
                dup: false,
                qos: QoS::AtLeastOnce,
                retain: false,
                pid: None,
                topic_name: delete_topic.as_str(),
                payload: b"",
                properties: embedded_mqtt::Properties::Slice(&[]),
            })
            .await
            .map_err(Error::MqttError)?;
        Ok(())
    }
}

pub struct PersistedShadow<
    'a,
    'm,
    S: ShadowState + DeserializeOwned,
    M: RawMutex,
    D: ShadowDAO<S>,
    const SUBS: usize,
> where
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    handler: ShadowHandler<'a, 'm, M, S, SUBS>,
    pub(crate) dao: D,
}

impl<'a, 'm, S, M, D, const SUBS: usize> PersistedShadow<'a, 'm, S, M, D, SUBS>
where
    S: ShadowState + DeserializeOwned + Default,
    M: RawMutex,
    D: ShadowDAO<S>,
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    /// Instantiate a new shadow that will be automatically persisted to NVM
    /// based on the passed `DAO`.
    pub fn new(mqtt: &'m embedded_mqtt::MqttClient<'a, M, SUBS>, dao: D) -> Result<Self, Error> {
        let handler = ShadowHandler {
            mqtt,
            subscription: None,
            _shadow: PhantomData,
        };

        Ok(Self { handler, dao })
    }

    /// Wait delta will subscribe if not already to Updatedelta and wait for changes
    ///
    pub async fn wait_delta(&mut self) -> Result<(S, Option<S::PatchState>), Error> {
        let mut state = match self.dao.read().await {
            Ok(state) => state,
            Err(_) => {
                self.dao.write(&S::default()).await?;
                S::default()
            }
        };

        let delta = self.handler.handle_delta(&mut state).await?;

        // Something has changed as part of handling a message. Persist it
        // to NVM storage.
        if delta.is_some() {
            self.dao.write(&state).await?;
        }

        Ok((state, delta))
    }

    /// Get an immutable reference to the internal local state.
    pub async fn try_get(&mut self) -> Result<S, Error> {
        self.dao.read().await
    }

    /// Initiate a `GetShadow` request, updating the local state from the cloud.
    pub async fn get_shadow(&self) -> Result<(), Error> {
        self.handler.get_shadow().await
    }

    /// Initiate an `UpdateShadow` request, reporting the local state to the cloud.
    pub async fn report_shadow(&mut self) -> Result<(), Error> {
        let mut state = self.dao.read().await?;
        self.handler
            .change_shadow_value(&mut state, None, Some(false))
            .await?;
        Ok(())
    }

    /// Update the state of the shadow.
    ///
    /// This function will update the desired state of the shadow in the cloud,
    /// and depending on whether the state update is rejected or accepted, it
    /// will automatically update the local version after response
    ///
    /// The returned `bool` from the update closure will determine wether the
    /// update is persisted using the `DAO`, or just updated in the cloud. This
    /// can be handy for activity or status field updates that are not relevant
    /// to store persistant on the device, but are required to be part of the
    /// same cloud shadow.
    pub async fn update<F: FnOnce(&S, &mut S::PatchState)>(&mut self, f: F) -> Result<(), Error> {
        let mut desired = S::PatchState::default();
        let mut state = self.dao.read().await?;
        f(&state, &mut desired);

        self.handler
            .change_shadow_value(&mut state, Some(desired), Some(false))
            .await?;

        //Always persist
        self.dao.write(&state).await?;

        Ok(())
    }

    pub async fn delete_shadow(&mut self) -> Result<(), Error> {
        self.handler.delete_shadow().await
    }
}

pub struct Shadow<'a, 'm, S: ShadowState, M: RawMutex, const SUBS: usize>
where
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    state: S,
    handler: ShadowHandler<'a, 'm, M, S, SUBS>,
}

impl<'a, 'm, S, M, const SUBS: usize> Shadow<'a, 'm, S, M, SUBS>
where
    S: ShadowState,
    M: RawMutex,
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    /// Instantiate a new non-persisted shadow
    pub fn new(state: S, mqtt: &'m embedded_mqtt::MqttClient<'a, M, SUBS>) -> Result<Self, Error> {
        let handler = ShadowHandler {
            mqtt,
            subscription: None,
            _shadow: PhantomData,
        };
        Ok(Self { handler, state })
    }

    /// Handle incoming publish messages from the cloud on any topics relevant
    /// for this particular shadow.
    ///
    /// This function needs to be fed all relevant incoming MQTT payloads in
    /// order for the shadow manager to work.
    pub async fn handle_message(&mut self) -> Result<(&S, Option<S::PatchState>), Error> {
        let delta = self.handler.handle_delta(&mut self.state).await?;
        Ok((&self.state, delta))
    }

    /// Get an immutable reference to the internal local state.
    pub fn get(&self) -> &S {
        &self.state
    }

    /// Initiate an `UpdateShadow` request, reporting the local state to the cloud.
    pub async fn report_shadow(&mut self) -> Result<(), Error> {
        self.handler
            .change_shadow_value(&mut self.state, None, Some(false))
            .await?;
        Ok(())
    }

    /// Update the state of the shadow.
    ///
    /// This function will update the desired state of the shadow in the cloud,
    /// and depending on whether the state update is rejected or accepted, it
    /// will automatically update the local version after response
    pub async fn update<F: FnOnce(&S, &mut S::PatchState)>(&mut self, f: F) -> Result<(), Error> {
        let mut desired = S::PatchState::default();
        f(&self.state, &mut desired);

        self.handler
            .change_shadow_value(&mut self.state, Some(desired), Some(false))
            .await?;

        Ok(())
    }

    /// Initiate a `GetShadow` request, updating the local state from the cloud.
    pub async fn get_shadow(&self) -> Result<(), Error> {
        self.handler.get_shadow().await
    }

    pub async fn delete_shadow(&mut self) -> Result<(), Error> {
        self.handler.delete_shadow().await
    }
}

impl<'a, 'm, S, M, const SUBS: usize> core::fmt::Debug for Shadow<'a, 'm, S, M, SUBS>
where
    S: ShadowState + core::fmt::Debug,
    M: RawMutex,
    [(); S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD]:,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "[{:?}] = {:?}",
            S::NAME.unwrap_or(CLASSIC_SHADOW),
            self.get()
        )
    }
}

#[cfg(feature = "defmt")]
impl<'a, 'm, S, M> defmt::Format for Shadow<'a, 'm, S, M>
where
    S: ShadowState + defmt::Format,
    M: RawMutex,
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
