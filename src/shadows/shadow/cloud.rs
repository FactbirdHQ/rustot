//! MQTT cloud communication methods for Shadow

use core::ops::DerefMut;

use serde::{de::DeserializeOwned, Serialize};

use crate::mqtt::{
    DeferredPayload, MqttClient, MqttMessage, MqttSubscription, PayloadError, QoS, ToPayload,
};

use crate::shadows::{
    data_types::{
        AcceptedResponse, DeltaResponse, DeltaState, ErrorResponse, Request, RequestState,
    },
    error::{Error, ShadowError},
    store::StateStore,
    topics::Topic,
    ShadowRoot,
};

use super::Shadow;

/// Maximum topic length for MQTT operations.
const MAX_TOPIC_LEN: usize = 128;

/// Overhead for partial request JSON formatting.
const PARTIAL_REQUEST_OVERHEAD: usize = 64;

/// Default name for classic/unnamed shadows.
const CLASSIC_SHADOW: &str = "classic";

// =============================================================================
// Cloud Communication Methods (MQTT)
// =============================================================================

impl<'a, 'm, S, C, K> Shadow<'a, 'm, S, C, K>
where
    S: ShadowRoot + Clone,
    S::Delta: Serialize + DeserializeOwned + Default,
    S::Reported: Serialize + Default,
    C: MqttClient,
    K: StateStore<S>,
{
    // =========================================================================
    // Private MQTT Helper Methods (ported from ShadowHandler)
    // =========================================================================

    /// Subscribe to delta topic and wait for message.
    ///
    /// On first call, subscribes to the delta topic and fetches current shadow
    /// state from cloud. On subsequent calls, waits for delta messages.
    async fn handle_delta(&self) -> Result<Option<S::Delta>, Error> {
        // Loop to automatically retry on clean session
        loop {
            let mut sub_ref = self.subscription.lock().await;

            if sub_ref.is_none() {
                debug!("Subscribing to delta topic");
                self.mqtt.wait_connected().await;

                let topic =
                    Topic::UpdateDelta.format::<64>(S::PREFIX, self.mqtt.client_id(), S::NAME)?;

                let sub = self
                    .mqtt
                    .subscribe(&[(topic.as_str(), QoS::AtMostOnce)])
                    .await
                    .map_err(|_| Error::Mqtt)?;

                let _ = sub_ref.insert(sub);

                let delta_state = self.get_shadow_from_cloud().await?;

                return Ok(delta_state.delta);
            }

            // Scope the mutable borrow of the subscription so we can call
            // sub_ref.take() in the clean-session path below.
            let result = {
                let sub = sub_ref.deref_mut().as_mut().unwrap();
                match sub.next_message().await {
                    Some(delta_message) => {
                        debug!(
                            "[{:?}] Received shadow delta event.",
                            S::NAME.unwrap_or(CLASSIC_SHADOW),
                        );

                        let mut buf = [0u8; 64];
                        let parsed = serde_json_core::from_slice_escaped::<DeltaResponse<S::Delta>>(
                            delta_message.payload(),
                            &mut buf,
                        );

                        Some(
                            parsed
                                .map(|(delta, _)| delta.state)
                                .map_err(|_| Error::InvalidPayload),
                        )
                    }
                    None => None,
                }
            };

            match result {
                Some(Ok(state)) => return Ok(state),
                Some(Err(e)) => return Err(e),
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
            }
        }
    }

    /// Publish an update request to the cloud and wait for response.
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
            return Err(Error::ShadowError(ShadowError::Forbidden));
        }

        let request: Request<'_, S::Delta, S::Reported> = Request {
            state: RequestState { desired, reported },
            client_token: Some(self.mqtt.client_id()),
            version: None,
        };

        let payload = DeferredPayload::new(
            |buf: &mut [u8]| {
                serde_json_core::to_slice(&request, buf).map_err(|_| PayloadError::EncodingFailed)
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
                    .map_err(|_| Error::ShadowError(ShadowError::NotFound))?;

                    if error_response.client_token != Some(self.mqtt.client_id()) {
                        continue;
                    }

                    return Err(Error::ShadowError(
                        error_response.try_into().unwrap_or(ShadowError::NotFound),
                    ));
                }
                _ => {
                    error!("Expected Topic name GetRejected or GetAccepted but got something else");
                    return Err(Error::WrongShadowName);
                }
            }
        }
    }

    /// Fetch the shadow state from the cloud.
    async fn get_shadow_from_cloud(&self) -> Result<DeltaState<S::Delta, S::Delta>, Error> {
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
                .map_err(|_| Error::ShadowError(ShadowError::NotFound))?;

                if error_response.code == 404 {
                    debug!(
                        "[{:?}] Thing has no shadow document. Creating with defaults...",
                        S::NAME.unwrap_or(CLASSIC_SHADOW)
                    );
                    return self.create_shadow().await;
                }

                Err(Error::ShadowError(
                    error_response.try_into().unwrap_or(ShadowError::NotFound),
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

    /// Delete the shadow from the cloud.
    async fn delete_from_cloud(&self) -> Result<(), Error> {
        // Wait for mqtt to connect
        self.mqtt.wait_connected().await;

        let mut sub = self.publish_and_subscribe(Topic::Delete, b"").await?;

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
                .map_err(|_| Error::ShadowError(ShadowError::NotFound))?;

                Err(Error::ShadowError(
                    error_response.try_into().unwrap_or(ShadowError::NotFound),
                ))
            }
            _ => {
                error!(
                    "Expected Topic name DeleteRejected or DeleteAccepted but got something else"
                );
                Err(Error::WrongShadowName)
            }
        }
    }

    /// Create a new shadow with default state.
    async fn create_shadow(&self) -> Result<DeltaState<S::Delta, S::Delta>, Error> {
        debug!(
            "[{:?}] Creating initial shadow value.",
            S::NAME.unwrap_or(CLASSIC_SHADOW),
        );

        self.update_shadow(None, Some(S::Reported::default())).await
    }

    /// Subscribe to accepted/rejected topics and then publish a request.
    ///
    /// This helper handles the subscribe-then-publish pattern used by
    /// Get, Update, and Delete operations.
    async fn publish_and_subscribe(
        &self,
        topic: Topic,
        payload: impl ToPayload,
    ) -> Result<C::Subscription<'m, 2>, Error> {
        let (accepted, rejected) = match topic {
            Topic::Get => (Topic::GetAccepted, Topic::GetRejected),
            Topic::Update => (Topic::UpdateAccepted, Topic::UpdateRejected),
            Topic::Delete => (Topic::DeleteAccepted, Topic::DeleteRejected),
            _ => return Err(Error::ShadowError(ShadowError::Forbidden)),
        };

        //*** SUBSCRIBE ***/
        let accepted_topic = accepted.format::<65>(S::PREFIX, self.mqtt.client_id(), S::NAME)?;
        let rejected_topic = rejected.format::<65>(S::PREFIX, self.mqtt.client_id(), S::NAME)?;

        let sub = self
            .mqtt
            .subscribe(&[
                (accepted_topic.as_str(), QoS::AtLeastOnce),
                (rejected_topic.as_str(), QoS::AtLeastOnce),
            ])
            .await
            .map_err(|_| Error::Mqtt)?;

        //*** PUBLISH REQUEST ***/
        let topic_name =
            topic.format::<MAX_TOPIC_LEN>(S::PREFIX, self.mqtt.client_id(), S::NAME)?;
        self.mqtt
            .publish(topic_name.as_str(), payload)
            .await
            .map_err(|_| Error::Mqtt)?;

        Ok(sub)
    }

    // =========================================================================
    // Public Cloud Communication Methods
    // =========================================================================

    /// Wait for delta updates from the cloud, apply them, persist, and acknowledge.
    ///
    /// This is the primary method for receiving cloud updates. It:
    /// 1. Subscribes to the delta topic (on first call)
    /// 2. Waits for a delta message from the cloud
    /// 3. Applies the delta to local state
    /// 4. Persists changed fields to storage
    /// 5. Acknowledges the update to the cloud
    ///
    /// Returns the current state and the delta that was applied (if any).
    ///
    /// ## Example
    ///
    /// ```ignore
    /// loop {
    ///     let (state, delta) = shadow.wait_delta().await?;
    ///     if let Some(delta) = delta {
    ///         info!("State updated from cloud");
    ///     }
    /// }
    /// ```
    pub async fn wait_delta(&self) -> Result<(S, Option<S::Delta>), Error> {
        let delta = self.handle_delta().await?;

        let state = if let Some(ref delta) = delta {
            debug!(
                "[{:?}] Delta reports new desired value. Changing local value...",
                S::NAME.unwrap_or(CLASSIC_SHADOW),
            );

            // Apply and persist using StateStore
            let state = self
                .apply_and_save(delta)
                .await
                .map_err(|_| Error::DaoWrite)?;

            // Acknowledge to cloud
            self.update_shadow(None, Some(state.clone().into_reported()))
                .await?;

            state
        } else {
            // No delta, just get current state
            self.store
                .get_state(Self::prefix())
                .await
                .map_err(|_| Error::DaoWrite)?
        };

        Ok((state, delta))
    }

    /// Report state changes to the cloud.
    ///
    /// Use this method to update the reported state in the cloud after
    /// local changes. The closure receives the current state and a mutable
    /// reference to the reported struct to populate.
    ///
    /// If the cloud responds with a delta (because reported differs from
    /// desired), the delta is automatically applied and persisted.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Report current timeout value
    /// shadow.update(|state, reported| {
    ///     reported.timeout = Some(state.timeout);
    /// }).await?;
    /// ```
    pub async fn update<F>(&self, f: F) -> Result<(), Error>
    where
        F: FnOnce(&S, &mut S::Reported),
    {
        let state = self
            .store
            .get_state(Self::prefix())
            .await
            .map_err(|_| Error::DaoWrite)?;

        let mut reported = S::Reported::default();
        f(&state, &mut reported);

        let response = self.update_shadow(None, Some(reported)).await?;

        if let Some(delta) = response.delta {
            self.apply_and_save(&delta)
                .await
                .map_err(|_| Error::DaoWrite)?;
        }

        Ok(())
    }

    /// Request state changes from the cloud.
    ///
    /// Use this method to request changes to the desired state (typically
    /// triggered by user interaction like a button press). The closure
    /// receives a mutable reference to a delta struct to populate.
    ///
    /// If the cloud accepts the change and returns a delta, it is
    /// automatically applied and persisted.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // User pressed button to change timeout
    /// shadow.update_desired(|delta| {
    ///     delta.timeout = Some(5000);
    /// }).await?;
    /// ```
    pub async fn update_desired<F>(&self, f: F) -> Result<(), Error>
    where
        F: FnOnce(&mut S::Delta),
    {
        let mut desired = S::Delta::default();
        f(&mut desired);

        let response = self.update_shadow(Some(desired), None).await?;

        if let Some(delta) = response.delta {
            self.apply_and_save(&delta)
                .await
                .map_err(|_| Error::DaoWrite)?;
        }

        Ok(())
    }

    /// Fetch and synchronize state from the cloud.
    ///
    /// Fetches the current shadow state from the cloud and applies any
    /// delta to local state. Use this on startup to ensure local state
    /// matches the cloud.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// shadow.load().await?;  // Load from storage
    /// let state = shadow.sync_shadow().await?;  // Sync with cloud
    /// ```
    pub async fn sync_shadow(&self) -> Result<S, Error> {
        let delta_state = self.get_shadow_from_cloud().await?;

        let state = if let Some(delta) = delta_state.delta {
            let state = self
                .apply_and_save(&delta)
                .await
                .map_err(|_| Error::DaoWrite)?;
            self.update_shadow(None, Some(state.clone().into_reported()))
                .await?;
            state
        } else {
            self.store
                .get_state(Self::prefix())
                .await
                .map_err(|_| Error::DaoWrite)?
        };

        Ok(state)
    }

    /// Delete the shadow from the cloud and reset local state to defaults.
    ///
    /// This removes the shadow from the cloud and resets the local state
    /// to defaults in storage.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// shadow.delete_shadow().await?;  // Gone from cloud, local reset to defaults
    /// ```
    pub async fn delete_shadow(&self) -> Result<(), Error> {
        // Delete from cloud
        self.delete_from_cloud().await?;

        // Reset state to default in storage
        let prefix = Self::prefix();
        let state = S::default();
        self.store
            .set_state(prefix, &state)
            .await
            .map_err(|_| Error::DaoWrite)?;

        Ok(())
    }
}
