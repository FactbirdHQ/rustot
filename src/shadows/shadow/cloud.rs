//! MQTT cloud communication methods for Shadow

use core::ops::DerefMut;

use embassy_sync::{blocking_mutex::raw::RawMutex, mutex::Mutex};
use serde::Serialize;

use crate::mqtt::{
    DeferredPayload, MqttClient, MqttMessage, MqttSubscription, PayloadError, PublishOptions, QoS,
    ToPayload,
};

use crate::shadows::{
    ShadowRoot,
    data_types::{
        AcceptedResponse, DeltaResponse, DeltaState, ErrorResponse, Request, RequestState,
    },
    error::{Error, ShadowError},
    store::StateStore,
    topics::{Topic, max_topic_len},
};

use super::Shadow;

/// Drop-guard that clears `Shadow::subscription` unless explicitly disarmed.
///
/// `handle_delta`'s lazy-subscribe path installs the delta-topic subscription
/// before the initial GET drain has finished. If the future is cancelled in
/// that window (e.g. a `tokio::select!` branch firing), the subscription
/// would be left in place and every subsequent call would silently wait for
/// a delta message on the subscription topic — a permanent hang, because
/// the cloud only publishes deltas when `desired` changes.
///
/// This guard restores the invariant `subscription == Some <=> initial sync
/// has fully completed` by taking the subscription back on any early exit.
/// `disarm()` is called on the happy path to keep the subscription cached.
struct ResetSubscriptionOnDrop<'s, M: RawMutex, T> {
    subscription: &'s Mutex<M, Option<T>>,
    armed: bool,
}

impl<M: RawMutex, T> ResetSubscriptionOnDrop<'_, M, T> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl<M: RawMutex, T> Drop for ResetSubscriptionOnDrop<'_, M, T> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // `try_lock` is synchronous; Drop cannot await. In practice the lock
        // is free here: any holder would have been on the cancelled future's
        // stack and its guard is dropped before ours. If the lock is somehow
        // contended, skipping the reset is safe — we'd merely miss the
        // cleanup for this one cancellation, which is better than blocking.
        if let Ok(mut guard) = self.subscription.try_lock() {
            guard.take();
        }
    }
}

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
    S::Delta: Serialize,
    S::Reported: Serialize,
    C: MqttClient,
    K: StateStore<S>,
    [(); max_topic_len(S::PREFIX, S::NAME)]:,
{
    // =========================================================================
    // Private MQTT Helper Methods (ported from ShadowHandler)
    // =========================================================================

    /// Ensure the cloud doc exists. Holds a mutex so only one caller per
    /// shadow runs the initial GET; the rest wait, then short-circuit on the
    /// flag. The delta from the GET is intentionally discarded — `wait_delta`
    /// and `sync_shadow` do their own GET to retrieve and apply any pending
    /// delta. Publish-side callers (`update_reported`, etc.) only need the
    /// doc to exist.
    pub(crate) async fn ensure_initialized(&self) -> Result<(), Error> {
        let mut init_guard = self.initialized.lock().await;
        if *init_guard {
            return Ok(());
        }

        warn!(
            "[{:?}] shadow not initialized — running initial GET",
            S::NAME.unwrap_or(CLASSIC_SHADOW)
        );

        // GET (404 falls back to `create_shadow_inner`). Discard the delta.
        let _ = self.get_shadow_from_cloud().await?;

        *init_guard = true;
        Ok(())
    }

    /// Apply a delta to local state, then ack the cloud with the resulting
    /// partial reported (plus any desired-side cleanup for variant switches).
    async fn apply_delta_and_ack(&self, delta: &S::Delta) -> Result<S, Error> {
        let state = self
            .apply_and_save(delta)
            .await
            .map_err(|_| Error::DaoWrite)?;
        let reported = state.into_partial_reported(delta);
        let cleanup = state.desired_cleanup(delta);
        self.update_shadow(cleanup, Some(reported)).await?;
        Ok(state)
    }

    /// Await the next delta and return both it and the resulting state.
    ///
    /// Lazily (re)subscribes to the delta topic on first call and on
    /// clean-session reconnect; each (re)subscribe is followed by a GET so
    /// that any pending cloud state — including deltas that occurred while
    /// not subscribed — is drained as a delta immediately. Returns as soon
    /// as a delta is available, either from the GET drain or from a live
    /// subscription message. Echo messages and empty-state messages are
    /// silently looped over.
    async fn handle_delta(&self) -> Result<(S, Option<S::Delta>), Error> {
        loop {
            let mut sub_ref = self.subscription.lock().await;

            if sub_ref.is_none() {
                debug!("Subscribing to delta topic");
                self.mqtt.wait_connected().await;

                let topic = Topic::UpdateDelta.format::<{ max_topic_len(S::PREFIX, S::NAME) }>(
                    S::PREFIX,
                    self.mqtt.client_id(),
                    S::NAME,
                )?;

                let sub = self
                    .mqtt
                    .subscribe(&[(topic.as_str(), QoS::AtLeastOnce)])
                    .await
                    .map_err(|_| Error::Mqtt)?;

                let _ = sub_ref.insert(sub);
                drop(sub_ref);

                // If we get cancelled doing the sync_shadow we need to drop the subscription to not skip the sync_shadow
                // On next iteration. Only when we know sync_shadow has completed we can keep the subscription.
                let mut reset_guard = ResetSubscriptionOnDrop {
                    subscription: &self.subscription,
                    armed: true,
                };

                // Drain pending state via GET. If a delta came through,
                // return it. Otherwise loop back and await a live one.
                let (state, delta) = self.sync_shadow().await?;
                reset_guard.disarm();

                if delta.is_some() {
                    return Ok((state, delta));
                }
                continue;
            }

            // Scope the mutable borrow so we can call sub_ref.take() on
            // clean-session below. The parsed DeltaResponse borrows from
            // the message payload, so everything must resolve in this block.
            let result: Result<Option<S::Delta>, ()> = {
                let sub = sub_ref.deref_mut().as_mut().unwrap();
                match sub.next_message().await {
                    Some(delta_message) => {
                        debug!(
                            "[{:?}] Received shadow delta event.",
                            S::NAME.unwrap_or(CLASSIC_SHADOW),
                        );

                        let resolver = self.store.resolver(Self::prefix());
                        let parsed =
                            DeltaResponse::parse::<S, _>(delta_message.payload(), &resolver)
                                .await
                                .map_err(|_| Error::InvalidPayload)?;

                        if parsed.client_token == Some(self.mqtt.client_id()) {
                            Ok(None)
                        } else {
                            Ok(parsed.state)
                        }
                    }
                    None => Err(()),
                }
            };

            match result {
                Ok(Some(delta)) => {
                    drop(sub_ref);
                    debug!(
                        "[{:?}] Delta reports new desired value. Changing local value...",
                        S::NAME.unwrap_or(CLASSIC_SHADOW),
                    );
                    let state = self.apply_delta_and_ack(&delta).await?;
                    return Ok((state, Some(delta)));
                }
                Ok(None) => continue,
                Err(()) => {
                    info!(
                        "[{:?}] Clean session detected, resubscribing to delta topic",
                        S::NAME.unwrap_or(CLASSIC_SHADOW)
                    );
                    sub_ref.take();
                    drop(sub_ref);
                    embassy_time::Timer::after(embassy_time::Duration::from_secs(1)).await;
                    continue;
                }
            }
        }
    }

    /// Publish an update request to the cloud and wait for response.
    async fn update_shadow<D: Serialize>(
        &self,
        desired: Option<D>,
        reported: Option<S::Reported>,
    ) -> Result<DeltaState<S::Delta, S::Delta>, Error> {
        debug!(
            "[{:?}] Updating shadow value.",
            S::NAME.unwrap_or(CLASSIC_SHADOW),
        );

        let request: Request<'_, D, S::Reported> = Request {
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

        let result = loop {
            let message = sub.next_message().await.ok_or(Error::InvalidPayload)?;

            match Topic::from_str(S::PREFIX, message.topic_name()) {
                Some((Topic::UpdateAccepted, _, _)) => {
                    let resolver = self.store.resolver(Self::prefix());
                    let response = AcceptedResponse::parse::<S, _>(message.payload(), &resolver)
                        .await
                        .map_err(|_| Error::InvalidPayload)?;

                    if response.client_token != Some(self.mqtt.client_id()) {
                        continue;
                    }

                    break Ok(response.state);
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

                    break Err(Error::ShadowError(
                        error_response.try_into().unwrap_or(ShadowError::NotFound),
                    ));
                }
                _ => {
                    error!("Expected Topic name GetRejected or GetAccepted but got something else");
                    break Err(Error::WrongShadowName);
                }
            }
        };

        let _ = sub.unsubscribe().await;
        result
    }

    /// Fetch the shadow state from the cloud.
    async fn get_shadow_from_cloud(&self) -> Result<DeltaState<S::Delta, S::Delta>, Error> {
        // Wait for mqtt to connect
        self.mqtt.wait_connected().await;

        let mut sub = self.publish_and_subscribe(Topic::Get, b"").await?;

        // Scope the message so it's dropped before we unsubscribe.
        // Returns Ok(Some(state)) on success, Ok(None) on 404 (needs create),
        // or Err on other failures.
        let result = {
            let get_message = sub.next_message().await.ok_or(Error::InvalidPayload)?;

            match Topic::from_str(S::PREFIX, get_message.topic_name()) {
                Some((Topic::GetAccepted, _, _)) => {
                    let resolver = self.store.resolver(Self::prefix());
                    let response =
                        AcceptedResponse::parse::<S, _>(get_message.payload(), &resolver)
                            .await
                            .map_err(|_| Error::InvalidPayload)?;

                    Ok(Some(response.state))
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
                        Ok(None)
                    } else {
                        Err(Error::ShadowError(
                            error_response.try_into().unwrap_or(ShadowError::NotFound),
                        ))
                    }
                }
                _ => {
                    error!(
                        "Expected topic name to be GetRejected or GetAccepted but got something else"
                    );
                    Err(Error::WrongShadowName)
                }
            }
        };

        let _ = sub.unsubscribe().await;

        match result? {
            Some(state) => Ok(state),
            None => self.create_shadow_inner().await,
        }
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
        let accepted_topic = accepted.format::<{ max_topic_len(S::PREFIX, S::NAME) }>(
            S::PREFIX,
            self.mqtt.client_id(),
            S::NAME,
        )?;
        let rejected_topic = rejected.format::<{ max_topic_len(S::PREFIX, S::NAME) }>(
            S::PREFIX,
            self.mqtt.client_id(),
            S::NAME,
        )?;

        let sub = self
            .mqtt
            .subscribe(&[
                (accepted_topic.as_str(), QoS::AtLeastOnce),
                (rejected_topic.as_str(), QoS::AtLeastOnce),
            ])
            .await
            .map_err(|_| Error::Mqtt)?;

        //*** PUBLISH REQUEST ***/
        let topic_name = topic.format::<{ max_topic_len(S::PREFIX, S::NAME) }>(
            S::PREFIX,
            self.mqtt.client_id(),
            S::NAME,
        )?;
        self.mqtt
            .publish_with_options(
                topic_name.as_str(),
                payload,
                PublishOptions::new().qos(QoS::AtLeastOnce),
            )
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
        info!(
            "[{:?}] wait_delta: entry",
            S::NAME.unwrap_or(CLASSIC_SHADOW)
        );

        // No `ensure_initialized` here — `handle_delta`'s drain path GETs
        // via `sync_shadow`, which handles 404→create and flips the
        // `initialized` gate itself.
        let (state, delta) = self.handle_delta().await?;

        info!(
            "[{:?}] wait_delta: return has_delta={}",
            S::NAME.unwrap_or(CLASSIC_SHADOW),
            delta.is_some()
        );
        Ok((state, delta))
    }

    /// Report state changes to the cloud.
    ///
    /// Use this method to update the reported state in the cloud after
    /// local changes. Pass a `Reported` struct with fields to update.
    /// Use the `reported()` builder to construct it conveniently.
    ///
    /// Accepts anything convertible to `S::Reported` via `Into`, including
    /// the state type `S` itself (reports all fields).
    ///
    /// If the cloud responds with a delta (because reported differs from
    /// desired), the delta is automatically applied and persisted.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Report specific fields using builder (requires "builders" feature)
    /// shadow.update_reported(
    ///     MyState::reported().timeout(5000u32).build()
    /// ).await?;
    ///
    /// // Report full state (From<S> for Reported is auto-generated)
    /// shadow.update_reported(state).await?;
    /// ```
    pub async fn update_reported(&self, reported: impl Into<S::Reported>) -> Result<(), Error> {
        self.ensure_initialized().await?;

        let reported: S::Reported = reported.into();

        // Persist any report_only(persist) fields to KV before sending to cloud.
        self.store
            .persist_reported(Self::prefix(), &reported)
            .await
            .map_err(|_| Error::DaoWrite)?;

        let response = self.update_shadow::<S::Delta>(None, Some(reported)).await?;

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
    /// triggered by user interaction like a button press). Pass a `Delta`
    /// struct with fields to update. Use the `desired()` builder to
    /// construct it conveniently.
    ///
    /// Accepts anything convertible to `S::Delta` via `Into`, including
    /// `DesiredFoo` types for adjacently-tagged enums.
    ///
    /// Sends both the desired change and the corresponding reported state
    /// in a single update, so the cloud resolves the delta immediately
    /// without requiring an extra round-trip through `wait_delta`.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Request timeout change using builder (requires "builders" feature)
    /// shadow.update_desired(
    ///     MyState::desired().timeout(5000).build()
    /// ).await?;
    /// ```
    pub async fn update_desired(&self, desired: impl Into<S::Delta>) -> Result<(), Error> {
        self.ensure_initialized().await?;

        let state: S = self
            .store
            .get_state(Self::prefix())
            .await
            .map_err(|_| Error::DaoWrite)?;

        let desired: S::Delta = desired.into();
        let reported = state.into_partial_reported(&desired);

        let response = self.update_shadow(Some(desired), Some(reported)).await?;

        if let Some(delta) = response.delta {
            self.apply_and_save(&delta)
                .await
                .map_err(|_| Error::DaoWrite)?;
        }

        Ok(())
    }

    /// GET the shadow, apply any pending delta, ack it, and return the
    /// resulting local state and the delta (if any). Flips the `initialized`
    /// gate so subsequent publish-side callers can skip their own GET. Use
    /// on startup to reconcile local state with the cloud.
    ///
    /// Prefer `desired` over `delta` — `delta` is a diff against the cloud's
    /// possibly-stale reported, so applying only the diff can leave local
    /// fields inconsistent (e.g. a URL updated while its parent variant
    /// discriminator is unchanged). The full `desired` converges reliably.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// shadow.load().await?;                              // Load from storage
    /// let (state, _) = shadow.sync_shadow().await?;      // Sync with cloud
    /// ```
    pub async fn sync_shadow(&self) -> Result<(S, Option<S::Delta>), Error> {
        let delta_state = self.get_shadow_from_cloud().await?;
        // A successful GET (or 404→create) confirms the cloud doc exists,
        // so the gate flips.
        *self.initialized.lock().await = true;
        let delta = delta_state.desired.or(delta_state.delta);

        let state = if let Some(ref d) = delta {
            self.apply_delta_and_ack(d).await?
        } else {
            self.store
                .get_state(Self::prefix())
                .await
                .map_err(|_| Error::DaoWrite)?
        };

        Ok((state, delta))
    }

    /// Create a new shadow in the cloud with the current device state.
    ///
    /// Reads the actual state from the KV store and publishes it to the cloud
    /// with both `desired` (fully populated delta — all modifiable keys) and
    /// `reported` (full reported state including report_only fields). This
    /// ensures the cloud has a complete picture of the device's state from
    /// the start.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let delta_state = shadow.create_shadow().await?;
    /// ```
    pub async fn create_shadow(&self) -> Result<DeltaState<S::Delta, S::Delta>, Error> {
        let r = self.create_shadow_inner().await?;
        // External callers have explicitly chosen to push full state; the
        // cloud doc now exists with our defaults, so the gate flips. Locks
        // outside the get_shadow_from_cloud → 404 path (which uses _inner
        // directly and avoids the relock that would deadlock).
        *self.initialized.lock().await = true;
        Ok(r)
    }

    /// Inner create — publishes the current local state as full desired +
    /// reported. Does not touch `initialized`, so it's safe to call from
    /// `get_shadow_from_cloud`'s 404 fallback while `ensure_initialized`
    /// holds the lock.
    async fn create_shadow_inner(&self) -> Result<DeltaState<S::Delta, S::Delta>, Error> {
        debug!(
            "[{:?}] Creating initial shadow value.",
            S::NAME.unwrap_or(CLASSIC_SHADOW),
        );

        let state: S = self
            .store
            .get_state(Self::prefix())
            .await
            .map_err(|_| Error::DaoWrite)?;

        let desired = state.into_delta();
        let reported = state.into_reported();

        self.update_shadow(Some(desired), Some(reported)).await
    }

    /// Delete the shadow from the cloud and remove all persisted state.
    ///
    /// Publishes a delete request via MQTT, waits for acceptance, then
    /// removes all stored KV entries for this shadow's prefix.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// shadow.delete_shadow().await?;  // Gone from cloud, storage cleaned up
    /// ```
    pub async fn delete_shadow(&self) -> Result<(), Error> {
        self.mqtt.wait_connected().await;

        let mut sub = self.publish_and_subscribe(Topic::Delete, b"").await?;

        let result = {
            let message = sub.next_message().await.ok_or(Error::InvalidPayload)?;

            match Topic::from_str(S::PREFIX, message.topic_name()) {
                Some((Topic::DeleteAccepted, _, _)) => Ok(()),
                Some((Topic::DeleteRejected, _, _)) => {
                    let mut buf = [0u8; 64];
                    let (error_response, _) = serde_json_core::from_slice_escaped::<ErrorResponse>(
                        message.payload(),
                        &mut buf,
                    )
                    .map_err(|_| Error::ShadowError(ShadowError::NotFound))?;

                    // 404 means the cloud doc is already gone — treat as
                    // success so delete is idempotent.
                    if error_response.code == 404 {
                        debug!(
                            "[{:?}] Shadow already absent in cloud — treating delete as success.",
                            S::NAME.unwrap_or(CLASSIC_SHADOW)
                        );
                        Ok(())
                    } else {
                        Err(Error::ShadowError(
                            error_response.try_into().unwrap_or(ShadowError::NotFound),
                        ))
                    }
                }
                _ => {
                    error!(
                        "Expected Topic name DeleteRejected or DeleteAccepted but got something else"
                    );
                    Err(Error::WrongShadowName)
                }
            }
        };

        let _ = sub.unsubscribe().await;
        result?;

        self.store
            .delete_state(Self::prefix())
            .await
            .map_err(|_| Error::DaoWrite)?;

        // Reset the gate so the next publish re-initializes the cloud doc.
        *self.initialized.lock().await = false;

        Ok(())
    }
}
