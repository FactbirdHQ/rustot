pub mod dao;
pub mod data_types;
pub mod error;
pub mod topics;

pub use rustot_derive;

use core::{marker::PhantomData, ops::DerefMut};

pub use data_types::Patch;
use embassy_sync::{
    blocking_mutex::raw::{NoopRawMutex, RawMutex},
    mutex::Mutex,
};
pub use error::Error;
use mqttrust::{DeferredPayload, Publish, Subscribe, SubscribeTopic, ToPayload};
use serde::{de::DeserializeOwned, Serialize};

use data_types::{
    AcceptedResponse, DeltaResponse, DeltaState, ErrorResponse, Request, RequestState,
};
use topics::Topic;

use self::dao::ShadowDAO;

const MAX_TOPIC_LEN: usize = 128;
const PARTIAL_REQUEST_OVERHEAD: usize = 64;
const CLASSIC_SHADOW: &str = "Classic";

pub trait ShadowState: ShadowPatch {
    const NAME: Option<&'static str>;
    const PREFIX: &'static str = "$aws";

    const MAX_PAYLOAD_SIZE: usize = 512;
}

pub trait ShadowPatch: Default + Clone + Sized {
    // Contains all fields from `Self` as optionals
    type Delta: DeserializeOwned + Serialize + Clone + Default;

    // Contains all fields from `Delta` + additional optional fields
    type Reported: From<Self> + Serialize + Default;

    fn apply_patch(&mut self, delta: Self::Delta);
}

struct ShadowHandler<'a, 'm, M: RawMutex, S> {
    mqtt: &'m mqttrust::MqttClient<'a, M>,
    subscription: Mutex<NoopRawMutex, Option<mqttrust::Subscription<'a, 'm, M, 2>>>,
    _shadow: PhantomData<S>,
}

impl<'a, M: RawMutex, S: ShadowState> ShadowHandler<'a, '_, M, S> {
    async fn handle_delta(&self) -> Result<Option<S::Delta>, Error> {
        // Loop to automatically retry on clean session
        loop {
            let mut sub_ref = self.subscription.lock().await;

            let delta_subscription = match sub_ref.deref_mut() {
                Some(sub) => sub,
                None => {
                    info!("Subscribing to delta topic");
                    self.mqtt.wait_connected().await;

                    let sub = self
                        .mqtt
                        .subscribe::<2>(
                            Subscribe::builder()
                                .topics(&[SubscribeTopic::builder()
                                    .topic_path(
                                        topics::Topic::UpdateDelta
                                            .format::<64>(
                                                S::PREFIX,
                                                self.mqtt.client_id(),
                                                S::NAME,
                                            )?
                                            .as_str(),
                                    )
                                    .build()])
                                .build(),
                        )
                        .await
                        .map_err(Error::MqttError)?;

                    let _ = sub_ref.insert(sub);

                    let delta_state = self.get_shadow().await?;

                    return Ok(delta_state.delta);
                }
            };

            let delta_message = match delta_subscription.next_message().await {
                Some(msg) => msg,
                None => {
                    // Clear subscription if we get clean session
                    info!(
                        "[{:?}] Clean session detected, resubscribing to delta topic",
                        S::NAME.unwrap_or(CLASSIC_SHADOW)
                    );
                    sub_ref.take();
                    // Drop the lock and continue the loop to retry
                    drop(sub_ref);
                    continue;
                }
            };

            // Update the device's state to match the desired state in the
            // message body.
            debug!(
                "[{:?}] Received shadow delta event.",
                S::NAME.unwrap_or(CLASSIC_SHADOW),
            );

            // Buffer to temporarily hold escaped characters data
            let mut buf = [0u8; 64];

            // Use from_slice_escaped to properly handle escaped characters
            let (delta, _) = serde_json_core::from_slice_escaped::<DeltaResponse<S::Delta>>(
                delta_message.payload(),
                &mut buf,
            )
            .map_err(|_| Error::InvalidPayload)?;

            return Ok(delta.state);
        }
    }

    /// Internal helper function for applying a delta state to the actual shadow
    /// state, and update the cloud shadow.
    async fn update_shadow(
        &self,
        desired: Option<S::Delta>,
        reported: Option<S::Reported>,
    ) -> Result<DeltaState<S::Delta, S::Delta>, Error> {
        debug!(
            "[{:?}] Updating reported shadow value.",
            S::NAME.unwrap_or(CLASSIC_SHADOW),
        );

        if desired.is_some() && reported.is_some() {
            // Do not edit both reported and desired at the same time
            return Err(Error::ShadowError(error::ShadowError::Forbidden));
        }

        let request: Request<'_, S::Delta, S::Reported> = Request {
            state: RequestState { desired, reported },
            client_token: Some(self.mqtt.client_id()),
            version: None,
        };

        let payload = DeferredPayload::new(
            |buf: &mut [u8]| {
                serde_json_core::to_slice(&request, buf)
                    .map_err(|_| mqttrust::EncodingError::BufferSize)
            },
            S::MAX_PAYLOAD_SIZE + PARTIAL_REQUEST_OVERHEAD,
        );

        // Wait for mqtt to connect
        self.mqtt.wait_connected().await;

        let mut sub = self.publish_and_subscribe(Topic::Update, payload).await?;

        //*** WAIT RESPONSE ***/
        debug!("Wait for Accepted or Rejected");

        loop {
            let message = sub.next_message().await.ok_or(Error::InvalidPayload)?;

            match Topic::from_str(S::PREFIX, message.topic_name()) {
                Some((Topic::UpdateAccepted, _, _)) => {
                    let mut buf = [0u8; 64];
                    let (response, _) = serde_json_core::from_slice_escaped::<
                        // FIXME:
                        AcceptedResponse<S::Delta, S::Delta>,
                    >(message.payload(), &mut buf)
                    .map_err(|_| Error::InvalidPayload)?;

                    if response.client_token != Some(self.mqtt.client_id()) {
                        continue;
                    }

                    return Ok(response.state);
                }
                Some((Topic::UpdateRejected, _, _)) => {
                    let mut buf = [0u8; 64];
                    let (error_response, _) = serde_json_core::from_slice_escaped::<ErrorResponse>(
                        message.payload(),
                        &mut buf,
                    )
                    .map_err(|_| Error::ShadowError(error::ShadowError::NotFound))?;

                    if error_response.client_token != Some(self.mqtt.client_id()) {
                        continue;
                    }

                    return Err(Error::ShadowError(
                        error_response
                            .try_into()
                            .unwrap_or(error::ShadowError::NotFound),
                    ));
                }
                _ => {
                    error!("Expected Topic name GetRejected or GetAccepted but got something else");
                    return Err(Error::WrongShadowName);
                }
            }
        }
    }

    /// Initiate a `GetShadow` request, updating the local state from the cloud.
    async fn get_shadow(&self) -> Result<DeltaState<S::Delta, S::Delta>, Error> {
        // Wait for mqtt to connect
        self.mqtt.wait_connected().await;

        let mut sub = self.publish_and_subscribe(Topic::Get, b"").await?;

        let get_message = sub.next_message().await.ok_or(Error::InvalidPayload)?;

        // Check if topic is GetAccepted
        // Deserialize message
        // Persist shadow and return new shadow
        match Topic::from_str(S::PREFIX, get_message.topic_name()) {
            Some((Topic::GetAccepted, _, _)) => {
                let mut buf = [0u8; 64];
                let (response, _) = serde_json_core::from_slice_escaped::<
                    AcceptedResponse<S::Delta, S::Delta>,
                >(get_message.payload(), &mut buf)
                .map_err(|_| Error::InvalidPayload)?;

                Ok(response.state)
            }
            Some((Topic::GetRejected, _, _)) => {
                let mut buf = [0u8; 64];
                let (error_response, _) = serde_json_core::from_slice_escaped::<ErrorResponse>(
                    get_message.payload(),
                    &mut buf,
                )
                .map_err(|_| Error::ShadowError(error::ShadowError::NotFound))?;

                if error_response.code == 404 {
                    debug!(
                        "[{:?}] Thing has no shadow document. Creating with defaults...",
                        S::NAME.unwrap_or(CLASSIC_SHADOW)
                    );
                    self.create_shadow().await?;
                }

                Err(Error::ShadowError(
                    error_response
                        .try_into()
                        .unwrap_or(error::ShadowError::NotFound),
                ))
            }
            _ => {
                error!(
                    "Expected topic name to be GetRejected or GetAccepted but got something else"
                );
                Err(Error::WrongShadowName)
            }
        }
    }

    pub async fn delete_shadow(&self) -> Result<(), Error> {
        // Wait for mqtt to connect
        self.mqtt.wait_connected().await;

        let mut sub = self
            .publish_and_subscribe(topics::Topic::Delete, b"")
            .await?;

        let message = sub.next_message().await.ok_or(Error::InvalidPayload)?;

        // Check if topic is DeleteAccepted
        match Topic::from_str(S::PREFIX, message.topic_name()) {
            Some((Topic::DeleteAccepted, _, _)) => Ok(()),
            Some((Topic::DeleteRejected, _, _)) => {
                let mut buf = [0u8; 64];
                let (error_response, _) = serde_json_core::from_slice_escaped::<ErrorResponse>(
                    message.payload(),
                    &mut buf,
                )
                .map_err(|_| Error::ShadowError(error::ShadowError::NotFound))?;

                Err(Error::ShadowError(
                    error_response
                        .try_into()
                        .unwrap_or(error::ShadowError::NotFound),
                ))
            }
            _ => {
                error!("Expected Topic name GetRejected or GetAccepted but got something else");
                Err(Error::WrongShadowName)
            }
        }
    }

    pub async fn create_shadow(&self) -> Result<DeltaState<S::Delta, S::Delta>, Error> {
        debug!(
            "[{:?}] Creating initial shadow value.",
            S::NAME.unwrap_or(CLASSIC_SHADOW),
        );

        self.update_shadow(None, Some(S::Reported::default())).await
    }

    /// This function will subscribe to accepted and rejected topics and then do a publish.
    /// It will only return when something is accepted or rejected
    /// Topic is the topic you want to publish to
    /// The function will automatically subscribe to the accepted and rejected topic related to the publish topic
    async fn publish_and_subscribe(
        &self,
        topic: topics::Topic,
        payload: impl ToPayload,
    ) -> Result<mqttrust::Subscription<'a, '_, M, 2>, Error> {
        let (accepted, rejected) = match topic {
            Topic::Get => (Topic::GetAccepted, Topic::GetRejected),
            Topic::Update => (Topic::UpdateAccepted, Topic::UpdateRejected),
            Topic::Delete => (Topic::DeleteAccepted, Topic::DeleteRejected),
            _ => return Err(Error::ShadowError(error::ShadowError::Forbidden)),
        };

        //*** SUBSCRIBE ***/
        let sub = self
            .mqtt
            .subscribe::<2>(
                Subscribe::builder()
                    .topics(&[
                        SubscribeTopic::builder()
                            .topic_path(
                                accepted
                                    .format::<65>(S::PREFIX, self.mqtt.client_id(), S::NAME)?
                                    .as_str(),
                            )
                            .build(),
                        SubscribeTopic::builder()
                            .topic_path(
                                rejected
                                    .format::<65>(S::PREFIX, self.mqtt.client_id(), S::NAME)?
                                    .as_str(),
                            )
                            .build(),
                    ])
                    .build(),
            )
            .await
            .map_err(Error::MqttError)?;

        //*** PUBLISH REQUEST ***/
        let topic_name =
            topic.format::<MAX_TOPIC_LEN>(S::PREFIX, self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(
                Publish::builder()
                    .topic_name(topic_name.as_str())
                    .payload(payload)
                    .build(),
            )
            .await
            .map_err(Error::MqttError)?;

        Ok(sub)
    }
}

pub struct PersistedShadow<'a, 'm, S, M: RawMutex, D> {
    handler: ShadowHandler<'a, 'm, M, S>,
    pub(crate) dao: Mutex<NoopRawMutex, D>,
}

impl<'a, 'm, S, M, D> PersistedShadow<'a, 'm, S, M, D>
where
    S: ShadowState + Serialize + DeserializeOwned,
    M: RawMutex,
    D: ShadowDAO<S>,
{
    /// Instantiate a new shadow that will be automatically persisted to NVM
    /// based on the passed `DAO`.
    pub fn new(mqtt: &'m mqttrust::MqttClient<'a, M>, dao: D) -> Self {
        let handler = ShadowHandler {
            mqtt,
            subscription: Mutex::new(None),
            _shadow: PhantomData,
        };

        Self {
            handler,
            dao: Mutex::new(dao),
        }
    }

    /// Wait delta will subscribe if not already to Updatedelta and wait for changes
    ///
    pub async fn wait_delta(&self) -> Result<(S, Option<S::Delta>), Error> {
        let mut dao = self.dao.lock().await;

        let mut state = match dao.read().await {
            Ok(state) => state,
            Err(_) => {
                error!("Could not read state from flash writing default");
                let state = S::default();
                dao.write(&state).await?;
                state
            }
        };

        // Drop the lock to avoid deadlock
        drop(dao);

        let delta = self.handler.handle_delta().await?;

        // Something has changed as part of handling a message. Persist it
        // to NVM storage.
        if let Some(ref delta) = delta {
            debug!(
                "[{:?}] Delta reports new desired value. Changing local value...",
                S::NAME.unwrap_or(CLASSIC_SHADOW),
            );

            state.apply_patch(delta.clone());

            self.handler
                .update_shadow(None, Some(state.clone().into()))
                .await?;

            self.dao.lock().await.write(&state).await?;
        }

        Ok((state, delta))
    }

    /// Get an immutable reference to the internal local state.
    pub async fn try_get(&self) -> Result<S, Error> {
        self.dao.lock().await.read().await
    }

    /// Initiate a `GetShadow` request, updating the local state from the cloud.
    pub async fn get_shadow(&self) -> Result<S, Error> {
        let delta_state = self.handler.get_shadow().await?;

        debug!("Persisting new state after get shadow request");
        let mut state = self.dao.lock().await.read().await.unwrap_or_default();
        if let Some(delta) = delta_state.delta {
            state.apply_patch(delta.clone());
            self.dao.lock().await.write(&state).await?;
            self.handler
                .update_shadow(None, Some(state.clone().into()))
                .await?;
        }

        Ok(state)
    }

    /// Report the state of the shadow.
    pub async fn report(&self) -> Result<(), Error> {
        let mut dao = self.dao.lock().await;

        let state = match dao.read().await {
            Ok(state) => state,
            Err(_) => {
                error!("Could not read state from flash writing default");
                let state = S::default();
                dao.write(&state).await?;
                state
            }
        };

        // Drop the lock to avoid deadlock
        drop(dao);

        self.handler.update_shadow(None, Some(state.into())).await?;
        Ok(())
    }

    /// Update the state of the shadow.
    ///
    /// This function will update the desired state of the shadow in the cloud,
    /// and depending on whether the state update is rejected or accepted, it
    /// will automatically update the local version after response
    ///
    /// The returned `bool` from the update closure will determine whether the
    /// update is persisted using the `DAO`, or just updated in the cloud. This
    /// can be handy for activity or status field updates that are not relevant
    /// to store persistent on the device, but are required to be part of the
    /// same cloud shadow.
    pub async fn update<F: FnOnce(&S, &mut S::Reported)>(&self, f: F) -> Result<(), Error> {
        let mut update = S::Reported::default();
        let mut state = self.dao.lock().await.read().await?;
        f(&state, &mut update);

        let response = self.handler.update_shadow(None, Some(update)).await?;

        if let Some(delta) = response.delta {
            state.apply_patch(delta.clone());

            self.dao.lock().await.write(&state).await?;
        }

        Ok(())
    }

    /// Updating desired should only be done on user requests e.g. button press or similar.
    /// State changes within the device should only change reported state.
    pub async fn update_desired<F: FnOnce(&mut S::Delta)>(&self, f: F) -> Result<(), Error> {
        let mut update = S::Delta::default();
        f(&mut update);

        let response = self.handler.update_shadow(Some(update), None).await?;

        if let Some(delta) = response.delta {
            let mut state = self.dao.lock().await.read().await?;
            state.apply_patch(delta.clone());
            self.dao.lock().await.write(&state).await?;
        }

        Ok(())
    }

    pub async fn delete_shadow(&self) -> Result<(), Error> {
        self.handler.delete_shadow().await?;
        self.dao.lock().await.write(&S::default()).await?;
        Ok(())
    }
}

pub struct Shadow<'a, 'm, S, M: RawMutex> {
    state: S,
    handler: ShadowHandler<'a, 'm, M, S>,
}

impl<'a, 'm, S, M> Shadow<'a, 'm, S, M>
where
    S: ShadowState,
    M: RawMutex,
{
    /// Instantiate a new non-persisted shadow
    pub fn new(state: S, mqtt: &'m mqttrust::MqttClient<'a, M>) -> Self {
        let handler = ShadowHandler {
            mqtt,
            subscription: Mutex::new(None),
            _shadow: PhantomData,
        };
        Self { handler, state }
    }

    /// Handle incoming publish messages from the cloud on any topics relevant
    /// for this particular shadow.
    ///
    /// This function needs to be fed all relevant incoming MQTT payloads in
    /// order for the shadow manager to work.
    pub async fn wait_delta(&mut self) -> Result<(&S, Option<S::Delta>), Error> {
        let delta = self.handler.handle_delta().await?;

        // Something has changed as part of handling a message. Persist it
        // to NVM storage.
        if let Some(ref delta) = delta {
            debug!(
                "[{:?}] Delta reports new desired value. Changing local value...",
                S::NAME.unwrap_or(CLASSIC_SHADOW),
            );

            self.state.apply_patch(delta.clone());

            self.handler
                .update_shadow(None, Some(self.state.clone().into()))
                .await?;
        }

        Ok((&self.state, delta))
    }

    /// Get an immutable reference to the internal local state.
    pub fn get(&self) -> &S {
        &self.state
    }

    /// Report the state of the shadow.
    pub async fn report(&mut self) -> Result<(), Error> {
        self.handler
            .update_shadow(None, Some(self.state.clone().into()))
            .await?;
        Ok(())
    }

    /// Update the state of the shadow.
    ///
    /// This function will update the desired state of the shadow in the cloud,
    /// and depending on whether the state update is rejected or accepted, it
    /// will automatically update the local version after response
    pub async fn update<F: FnOnce(&S, &mut S::Reported)>(&mut self, f: F) -> Result<(), Error> {
        let mut update = S::Reported::default();
        f(&self.state, &mut update);

        let response = self.handler.update_shadow(None, Some(update)).await?;

        if let Some(delta) = response.delta {
            self.state.apply_patch(delta.clone());
        }

        Ok(())
    }

    /// Initiate a `GetShadow` request, updating the local state from the cloud.
    pub async fn get_shadow(&mut self) -> Result<&S, Error> {
        let delta_state = self.handler.get_shadow().await?;

        debug!("Persisting new state after get shadow request");
        if let Some(delta) = delta_state.delta {
            self.state.apply_patch(delta.clone());
            self.handler
                .update_shadow(None, Some(self.state.clone().into()))
                .await?;
        }

        Ok(&self.state)
    }

    pub async fn delete_shadow(&mut self) -> Result<(), Error> {
        self.handler.delete_shadow().await
    }
}

impl<S, M> core::fmt::Debug for Shadow<'_, '_, S, M>
where
    S: ShadowState + core::fmt::Debug,
    M: RawMutex,
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

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
    struct TestDelta {
        field: heapless::String<20>,
    }

    #[test]
    fn test_from_slice_escaped() {
        let delta_message = b"{\"field\":\"\\\\HELLO WORLD\"}"; // FROM 4 backslashes in my string is saved in json as 2 backslashes which will be deserialized to 1 backslash
        let mut buf = [0u8; 64];

        let (delta, _) = serde_json_core::from_slice_escaped::<TestDelta>(delta_message, &mut buf)
            .expect("Failed to deserialize");

        println!("{}", delta.field);

        assert_eq!(delta.field.as_str(), "\\HELLO WORLD");
    }

    #[test]
    fn test_to_slice_escaping() {
        // Create a struct with a backslash in the string
        let mut test_data = TestDelta::default();
        test_data.field.push_str("\\HELLO WORLD").unwrap();

        let mut output = [0u8; 128];
        let bytes_written =
            serde_json_core::to_slice(&test_data, &mut output).expect("Failed to serialize");

        let serialized = &output[..bytes_written];
        let json_str = core::str::from_utf8(serialized).unwrap();
        println!("Serialized JSON: {}", json_str);

        // The JSON should contain \\ (escaped backslash)
        assert!(
            json_str.contains("\\\\"),
            "JSON should contain escaped backslash"
        );
        assert_eq!(json_str, r#"{"field":"\\HELLO WORLD"}"#);

        // Now test round-trip: deserialize it back
        let mut buf = [0u8; 64];
        let (deserialized, _) =
            serde_json_core::from_slice_escaped::<TestDelta>(serialized, &mut buf)
                .expect("Failed to deserialize");

        assert_eq!(deserialized.field.as_str(), "\\HELLO WORLD");
    }
}

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
