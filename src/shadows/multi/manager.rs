//! Multi-shadow manager with wildcard subscription support.
//!
//! Manages multiple runtime-named shadows using a single wildcard
//! MQTT subscription per operation type.

use serde::Serialize;

use crate::mqtt::{MqttClient, MqttMessage, MqttSubscription, QoS};
use crate::shadows::data_types::{
    AcceptedResponse, DeltaResponse, DeltaState, ErrorResponse, Request, RequestState,
};
use crate::shadows::store::StateStore;
use crate::shadows::topics::Topic;

use super::error::{MultiShadowError, MultiShadowResult};
use super::MultiShadowRoot;

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::time::Duration;
use tokio::sync::RwLock;

/// Default timeout for shadow operations.
const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Buffer size for formatting MQTT topic strings.
const MAX_TOPIC_BUF: usize = 256;

/// Multi-shadow manager that handles multiple runtime-named shadows.
///
/// Uses wildcard MQTT subscriptions for efficient monitoring and
/// any [`StateStore`] implementation for persistence.
///
/// Unlike [`Shadow`](crate::shadows::Shadow) which manages a single
/// compile-time-named shadow, this manager handles N shadows whose
/// names share a common pattern prefix (e.g., "flow-pump-01", "flow-valve-02").
///
/// # Sharing
///
/// The manager takes `&self` for all operations. Wrap in `Arc` if you need
/// to share across tasks.
pub struct MultiShadowManager<T, C, K>
where
    T: MultiShadowRoot,
    C: MqttClient,
    K: StateStore<T>,
{
    /// MQTT client for cloud communication.
    mqtt: C,

    /// Thing name for all shadows.
    thing_name: String,

    /// State store for persistence.
    store: K,

    /// Currently managed shadow IDs (without pattern prefix).
    shadow_ids: RwLock<HashSet<String>>,

    /// Operation timeout.
    operation_timeout: Duration,

    /// Phantom data for type parameter.
    _marker: PhantomData<T>,
}

impl<T, C, K> MultiShadowManager<T, C, K>
where
    T: MultiShadowRoot,
    T::Delta: Send + Sync,
    T::Reported: Default + Send + Sync,
    C: MqttClient,
    K: StateStore<T>,
{
    /// Create a new multi-shadow manager.
    ///
    /// # Arguments
    ///
    /// * `mqtt` - MQTT client (any [`MqttClient`] implementation)
    /// * `thing_name` - AWS IoT thing name
    /// * `store` - State store for persistence (e.g., [`FileKVStore`](crate::shadows::FileKVStore))
    pub fn new(mqtt: C, thing_name: String, store: K) -> Self {
        Self {
            mqtt,
            thing_name,
            store,
            shadow_ids: RwLock::new(HashSet::new()),
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
            _marker: PhantomData,
        }
    }

    /// Get the full shadow name from an ID.
    fn shadow_name(id: &str) -> String {
        format!("{}{}", T::PATTERN, id)
    }

    // =========================================================================
    // Initialization
    // =========================================================================

    /// Initialize the manager by discovering existing shadows from AWS IoT.
    ///
    /// Discovers shadows matching the pattern via the AWS IoT data plane API,
    /// then fetches and syncs each one.
    pub async fn initialize(&self) -> MultiShadowResult<()> {
        let shadow_names = self.discover_shadows().await?;

        for shadow_name in &shadow_names {
            if let Some(id) = shadow_name.strip_prefix(T::PATTERN) {
                self.add_shadow_id(id).await;
            }
        }

        for shadow_name in &shadow_names {
            if let Some(id) = shadow_name.strip_prefix(T::PATTERN) {
                if let Err(e) = self.get_shadow(id).await {
                    warn!("Failed to sync shadow '{}': {}", shadow_name, e);
                }
            }
        }

        Ok(())
    }

    /// Initialize with seeding from a known set of shadow names.
    ///
    /// Returns the names that existed locally but are no longer in the cloud
    /// (i.e., were deleted).
    pub async fn initialize_with_seed(
        &self,
        existing_shadow_names: Vec<String>,
    ) -> MultiShadowResult<Vec<String>> {
        let cloud_shadows = self.discover_shadows().await?;

        let deleted_shadows: Vec<String> = existing_shadow_names
            .iter()
            .filter(|name| !cloud_shadows.contains(name))
            .cloned()
            .collect();

        for shadow_name in &cloud_shadows {
            if let Some(id) = shadow_name.strip_prefix(T::PATTERN) {
                self.add_shadow_id(id).await;
            }
        }

        Ok(deleted_shadows)
    }

    /// Discover shadows matching the pattern from AWS IoT.
    async fn discover_shadows(&self) -> MultiShadowResult<Vec<String>> {
        let config = aws_config::load_from_env().await;
        let iot_client = aws_sdk_iotdataplane::Client::new(&config);

        let response = iot_client
            .list_named_shadows_for_thing()
            .thing_name(&self.thing_name)
            .page_size(100)
            .send()
            .await
            .map_err(|e| MultiShadowError::DiscoveryError(Box::new(e)))?;

        let matching_shadows = response
            .results
            .unwrap_or_default()
            .into_iter()
            .filter(|name| name.starts_with(T::PATTERN))
            .collect();

        Ok(matching_shadows)
    }

    // =========================================================================
    // Shadow ID Management
    // =========================================================================

    async fn add_shadow_id(&self, id: &str) {
        self.shadow_ids.write().await.insert(id.to_string());
    }

    /// Add a new shadow to be managed by ID.
    pub async fn add_shadow_by_id(&self, id: &str) {
        self.add_shadow_id(id).await;
    }

    /// Get managed shadow IDs (without the pattern prefix).
    pub async fn get_managed_ids(&self) -> Vec<String> {
        self.shadow_ids.read().await.iter().cloned().collect()
    }

    /// Check if an ID is currently managed.
    pub async fn is_id_managed(&self, id: &str) -> bool {
        self.shadow_ids.read().await.contains(id)
    }

    // =========================================================================
    // Delta Subscriptions
    // =========================================================================

    /// Create a wildcard delta subscription for all shadows matching the pattern.
    ///
    /// Returns a subscription that yields delta messages for any shadow whose
    /// name starts with [`MultiShadowRoot::PATTERN`].
    pub async fn create_delta_subscription(&self) -> MultiShadowResult<C::Subscription<'_, 1>> {
        let topic = Topic::UpdateDelta
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some("+"))
            .map_err(|_| MultiShadowError::InvalidTopic("delta wildcard".into()))?;

        self.mqtt
            .subscribe(&[(topic.as_str(), QoS::AtLeastOnce)])
            .await
            .map_err(|e| MultiShadowError::Mqtt(format!("{:?}", e)))
    }

    /// Create a wildcard deletion subscription for monitoring shadow deletions.
    pub async fn create_deletion_subscription(&self) -> MultiShadowResult<C::Subscription<'_, 1>> {
        let topic = Topic::DeleteAccepted
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some("+"))
            .map_err(|_| MultiShadowError::InvalidTopic("delete wildcard".into()))?;

        self.mqtt
            .subscribe(&[(topic.as_str(), QoS::AtLeastOnce)])
            .await
            .map_err(|e| MultiShadowError::Mqtt(format!("{:?}", e)))
    }

    // =========================================================================
    // Wait for Deltas
    // =========================================================================

    /// Wait for the next delta change from any managed shadow.
    ///
    /// Returns `(shadow_id, full_state, delta)`.
    pub async fn wait_delta(&self) -> MultiShadowResult<(String, T, Option<T::Delta>)> {
        let mut sub = self.create_delta_subscription().await?;

        if let Some(msg) = sub.next_message().await {
            return self
                .parse_delta_from_parts(msg.topic_name(), msg.payload())
                .await;
        }

        Err(MultiShadowError::Timeout)
    }

    /// Wait for the next shadow deletion from any managed shadow.
    ///
    /// Returns the deleted shadow ID, or `None` if the deletion was for
    /// an unrecognized shadow (not matching the pattern).
    pub async fn wait_deletion(&self) -> MultiShadowResult<Option<String>> {
        let mut sub = self.create_deletion_subscription().await?;

        if let Some(msg) = sub.next_message().await {
            return self.handle_deletion_from_parts(msg.topic_name()).await;
        }

        Err(MultiShadowError::Timeout)
    }

    /// Parse a delta message from topic and payload.
    ///
    /// Applies the delta to the local state store, reports the updated state,
    /// and auto-tracks the shadow if not already managed.
    pub async fn parse_delta_from_parts(
        &self,
        topic: &str,
        payload: &[u8],
    ) -> MultiShadowResult<(String, T, Option<T::Delta>)> {
        if let Some((Topic::UpdateDelta, _thing, Some(shadow_name))) =
            Topic::from_str(T::PREFIX, topic)
        {
            if let Some(id) = shadow_name.strip_prefix(T::PATTERN) {
                let prefix = Self::shadow_name(id);
                let resolver = self.store.resolver(&prefix);
                let delta_response = DeltaResponse::parse::<T, _>(payload, &resolver)
                    .await
                    .map_err(|e| {
                        MultiShadowError::InvalidDocument(format!("Failed to parse delta: {:?}", e))
                    })?;

                let state: T = if let Some(ref delta) = delta_response.state {
                    self.store.apply_delta(&prefix, delta).await.map_err(|e| {
                        MultiShadowError::StorageError(format!("Failed to apply delta: {:?}", e))
                    })?
                } else {
                    self.store.get_state(&prefix).await.map_err(|e| {
                        MultiShadowError::StorageError(format!("Failed to get state: {:?}", e))
                    })?
                };

                self.report(id, state.clone().into_reported()).await?;

                if !self.shadow_ids.read().await.contains(id) {
                    self.add_shadow_id(id).await;
                }

                return Ok((id.to_string(), state, delta_response.state));
            }
        }

        Err(MultiShadowError::InvalidDocument(
            "Failed to parse delta message".to_string(),
        ))
    }

    /// Parse a deletion message and handle shadow cleanup.
    ///
    /// Returns the deleted shadow ID if the message was a deletion
    /// for a managed shadow.
    pub async fn handle_deletion_from_parts(
        &self,
        topic: &str,
    ) -> MultiShadowResult<Option<String>> {
        if let Some((Topic::DeleteAccepted, _thing, Some(shadow_name))) =
            Topic::from_str(T::PREFIX, topic)
        {
            if let Some(id) = shadow_name.strip_prefix(T::PATTERN) {
                let prefix = Self::shadow_name(id);
                self.store.delete_state(&prefix).await.map_err(|e| {
                    MultiShadowError::StorageError(format!(
                        "Failed to delete state: {:?}",
                        e
                    ))
                })?;
                self.shadow_ids.write().await.remove(id);
                return Ok(Some(id.to_string()));
            }
        }

        Ok(None)
    }

    // =========================================================================
    // Get Shadow
    // =========================================================================

    /// Fetch the current state of a specific shadow from the cloud.
    ///
    /// Subscribes to get-accepted/rejected, publishes a get request,
    /// applies any delta, and reports back.
    pub async fn get_shadow(&self, id: &str) -> MultiShadowResult<(T, Option<T::Delta>)> {
        let shadow_name = Self::shadow_name(id);

        let accepted_topic = Topic::GetAccepted
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some(&shadow_name))
            .map_err(|_| MultiShadowError::InvalidTopic("get accepted".into()))?;
        let rejected_topic = Topic::GetRejected
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some(&shadow_name))
            .map_err(|_| MultiShadowError::InvalidTopic("get rejected".into()))?;
        let get_topic = Topic::Get
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some(&shadow_name))
            .map_err(|_| MultiShadowError::InvalidTopic("get".into()))?;

        // Subscribe-then-publish to avoid missing the response
        let mut sub = self
            .mqtt
            .subscribe(&[
                (accepted_topic.as_str(), QoS::AtLeastOnce),
                (rejected_topic.as_str(), QoS::AtLeastOnce),
            ])
            .await
            .map_err(|e| MultiShadowError::Mqtt(format!("{:?}", e)))?;

        self.mqtt
            .publish(get_topic.as_str(), b"" as &[u8])
            .await
            .map_err(|e| MultiShadowError::Mqtt(format!("{:?}", e)))?;

        let delta_state = self.wait_for_response(&shadow_name, &mut sub, None).await?;

        let prefix = Self::shadow_name(id);
        let mut state: T =
            self.store.get_state(&prefix).await.map_err(|e| {
                MultiShadowError::StorageError(format!("Failed to get state: {:?}", e))
            })?;

        if let Some(ref delta) = delta_state.delta {
            state = self.store.apply_delta(&prefix, delta).await.map_err(|e| {
                MultiShadowError::StorageError(format!("Failed to apply delta: {:?}", e))
            })?;
            self.report(id, state.clone().into_reported()).await?;
        }

        Ok((state, delta_state.delta))
    }

    // =========================================================================
    // Update
    // =========================================================================

    /// Report state changes to the cloud for a specific shadow.
    ///
    /// Accepts anything convertible to `T::Reported` via `Into`, including
    /// the state type `T` itself (reports all fields) or a builder result.
    ///
    /// If the cloud responds with a delta (because reported differs from
    /// desired), the delta is automatically applied and persisted.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Report specific fields using builder (requires "builders" feature)
    /// manager.update_reported("pump-01",
    ///     FlowState::reported().flow_rate(42.0).build()
    /// ).await?;
    /// ```
    pub async fn update_reported(
        &self,
        id: &str,
        reported: impl Into<T::Reported>,
    ) -> MultiShadowResult<T> {
        if !self.shadow_ids.read().await.contains(id) {
            return Err(MultiShadowError::ShadowNotManaged(id.to_string()));
        }

        let reported: T::Reported = reported.into();
        let response = self.report(id, reported).await?;

        let prefix = Self::shadow_name(id);
        if let Some(ref delta) = response.delta {
            self.store.apply_delta(&prefix, delta).await.map_err(|e| {
                MultiShadowError::StorageError(format!("Failed to apply delta: {:?}", e))
            })
        } else {
            self.store.get_state(&prefix).await.map_err(|e| {
                MultiShadowError::StorageError(format!("Failed to get state: {:?}", e))
            })
        }
    }

    /// Request state changes from the cloud for a specific shadow.
    ///
    /// Accepts anything convertible to `T::Delta` via `Into`, including
    /// builder results and `DesiredFoo` types for adjacently-tagged enums.
    ///
    /// If the cloud accepts the change and returns a delta, it is
    /// automatically applied and persisted.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Request changes using builder (requires "builders" feature)
    /// manager.update_desired("pump-01",
    ///     FlowState::desired().flow_rate(42.0).build()
    /// ).await?;
    /// ```
    pub async fn update_desired(
        &self,
        id: &str,
        desired: impl Into<T::Delta>,
    ) -> MultiShadowResult<()> {
        if !self.shadow_ids.read().await.contains(id) {
            return Err(MultiShadowError::ShadowNotManaged(id.to_string()));
        }

        let desired: T::Delta = desired.into();

        let shadow_name = Self::shadow_name(id);
        let client_token = uuid::Uuid::new_v4().to_string();

        let request: Request<'_, T::Delta, T::Reported> = Request {
            state: RequestState {
                desired: Some(desired),
                reported: None,
            },
            client_token: Some(&client_token),
            version: None,
        };

        let response = self
            .publish_and_wait_response(&shadow_name, &request, Some(&client_token))
            .await?;

        if let Some(ref delta) = response.delta {
            let prefix = Self::shadow_name(id);
            self.store.apply_delta(&prefix, delta).await.map_err(|e| {
                MultiShadowError::StorageError(format!("Failed to apply delta: {:?}", e))
            })?;
        }

        Ok(())
    }

    // =========================================================================
    // Delete
    // =========================================================================

    /// Delete a specific shadow by ID.
    pub async fn delete_shadow(&self, id: &str) -> MultiShadowResult<()> {
        let shadow_name = Self::shadow_name(id);

        let accepted_topic = Topic::DeleteAccepted
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some(&shadow_name))
            .map_err(|_| MultiShadowError::InvalidTopic("delete accepted".into()))?;
        let rejected_topic = Topic::DeleteRejected
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some(&shadow_name))
            .map_err(|_| MultiShadowError::InvalidTopic("delete rejected".into()))?;
        let delete_topic = Topic::Delete
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some(&shadow_name))
            .map_err(|_| MultiShadowError::InvalidTopic("delete".into()))?;

        let mut sub = self
            .mqtt
            .subscribe(&[
                (accepted_topic.as_str(), QoS::AtLeastOnce),
                (rejected_topic.as_str(), QoS::AtLeastOnce),
            ])
            .await
            .map_err(|e| MultiShadowError::Mqtt(format!("{:?}", e)))?;

        self.mqtt
            .publish(delete_topic.as_str(), b"" as &[u8])
            .await
            .map_err(|e| MultiShadowError::Mqtt(format!("{:?}", e)))?;

        let result = tokio::time::timeout(self.operation_timeout, async {
            if let Some(msg) = sub.next_message().await {
                match Topic::from_str(T::PREFIX, msg.topic_name()) {
                    Some((Topic::DeleteAccepted, _, _)) => return Ok(()),
                    Some((Topic::DeleteRejected, _, _)) => {
                        let error_response: ErrorResponse = serde_json::from_slice(msg.payload())?;
                        return Err(MultiShadowError::ShadowRejected {
                            code: error_response.code,
                            message: error_response.message.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            Ok(())
        })
        .await;

        match result {
            Ok(Ok(())) | Err(_) => {
                let prefix = Self::shadow_name(id);
                self.store.delete_state(&prefix).await.map_err(|e| {
                    MultiShadowError::StorageError(format!(
                        "Failed to delete state: {:?}",
                        e
                    ))
                })?;
                self.shadow_ids.write().await.remove(id);
                Ok(())
            }
            Ok(Err(e)) => Err(e),
        }
    }

    // =========================================================================
    // Bulk Operations
    // =========================================================================

    /// Get the current state of all managed shadows from the local store.
    pub async fn get_all_shadows(&self) -> MultiShadowResult<HashMap<String, T>> {
        let shadow_ids = self.shadow_ids.read().await;
        let mut result = HashMap::new();

        for id in shadow_ids.iter() {
            let prefix = Self::shadow_name(id);
            if let Ok(state) = self.store.get_state(&prefix).await {
                result.insert(id.clone(), state);
            }
        }

        Ok(result)
    }

    // =========================================================================
    // Configuration
    // =========================================================================

    /// Set operation timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.operation_timeout = timeout;
    }

    /// Get current operation timeout.
    pub fn timeout(&self) -> Duration {
        self.operation_timeout
    }

    /// Get thing name.
    pub fn thing_name(&self) -> &str {
        &self.thing_name
    }

    /// Get shadow pattern.
    pub fn shadow_pattern(&self) -> &'static str {
        T::PATTERN
    }

    // =========================================================================
    // Private Helpers
    // =========================================================================

    /// Report state for a specific shadow (update reported).
    async fn report(
        &self,
        id: &str,
        reported: T::Reported,
    ) -> MultiShadowResult<DeltaState<T::Delta, T::Delta>> {
        let shadow_name = Self::shadow_name(id);
        let client_token = uuid::Uuid::new_v4().to_string();

        let request: Request<'_, T, T::Reported> = Request {
            state: RequestState {
                desired: None,
                reported: Some(reported),
            },
            client_token: Some(&client_token),
            version: None,
        };

        self.publish_and_wait_response(&shadow_name, &request, Some(&client_token))
            .await
    }

    /// Subscribe to update accepted/rejected, publish a request, and wait for response.
    async fn publish_and_wait_response<D: Serialize + Sync, R: Serialize + Sync>(
        &self,
        shadow_name: &str,
        request: &Request<'_, D, R>,
        client_token: Option<&str>,
    ) -> MultiShadowResult<DeltaState<T::Delta, T::Delta>> {
        let accepted_topic = Topic::UpdateAccepted
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some(shadow_name))
            .map_err(|_| MultiShadowError::InvalidTopic("update accepted".into()))?;
        let rejected_topic = Topic::UpdateRejected
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some(shadow_name))
            .map_err(|_| MultiShadowError::InvalidTopic("update rejected".into()))?;
        let update_topic = Topic::Update
            .format::<MAX_TOPIC_BUF>(T::PREFIX, &self.thing_name, Some(shadow_name))
            .map_err(|_| MultiShadowError::InvalidTopic("update".into()))?;

        // Subscribe before publishing to avoid missing the response
        let mut sub = self
            .mqtt
            .subscribe(&[
                (accepted_topic.as_str(), QoS::AtLeastOnce),
                (rejected_topic.as_str(), QoS::AtLeastOnce),
            ])
            .await
            .map_err(|e| MultiShadowError::Mqtt(format!("{:?}", e)))?;

        let payload = serde_json::to_vec(request)?;

        self.mqtt
            .publish(update_topic.as_str(), payload.as_slice())
            .await
            .map_err(|e| MultiShadowError::Mqtt(format!("{:?}", e)))?;

        self.wait_for_response(shadow_name, &mut sub, client_token)
            .await
    }

    /// Wait for accepted/rejected response on a merged subscription.
    async fn wait_for_response(
        &self,
        prefix: &str,
        sub: &mut impl MqttSubscription,
        client_token: Option<&str>,
    ) -> MultiShadowResult<DeltaState<T::Delta, T::Delta>> {
        let result = tokio::time::timeout(self.operation_timeout, async {
            loop {
                let msg = sub
                    .next_message()
                    .await
                    .ok_or(MultiShadowError::InvalidDocument("Stream ended".into()))?;

                let topic_name = msg.topic_name();
                let payload = msg.payload();

                match Topic::from_str(T::PREFIX, topic_name) {
                    Some((Topic::GetAccepted | Topic::UpdateAccepted, _, _)) => {
                        let resolver = self.store.resolver(prefix);
                        let response = AcceptedResponse::parse::<T, _>(payload, &resolver)
                            .await
                            .map_err(|e| {
                                MultiShadowError::InvalidDocument(format!(
                                    "Failed to parse accepted response: {:?}",
                                    e
                                ))
                            })?;

                        if let Some(token) = client_token {
                            if response.client_token.is_some()
                                && response.client_token != Some(token)
                            {
                                continue;
                            }
                        }

                        return Ok(response.state);
                    }
                    Some((Topic::GetRejected | Topic::UpdateRejected, _, _)) => {
                        let error_response: ErrorResponse = serde_json::from_slice(payload)?;

                        if let Some(token) = client_token {
                            if error_response.client_token.is_some()
                                && error_response.client_token != Some(token)
                            {
                                continue;
                            }
                        }

                        if error_response.code == 404 {
                            return Err(MultiShadowError::ShadowNotFound);
                        }

                        return Err(MultiShadowError::ShadowRejected {
                            code: error_response.code,
                            message: error_response.message.to_string(),
                        });
                    }
                    _ => continue,
                }
            }
        })
        .await;

        match result {
            Ok(Ok(state)) => Ok(state),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(MultiShadowError::Timeout),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_shadow_name_generation() {
        let pattern = "flow-";
        let id = "pump-01";
        let shadow_name = format!("{}{}", pattern, id);
        assert_eq!(shadow_name, "flow-pump-01");
    }
}
