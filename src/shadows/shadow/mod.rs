//! Shadow state persistence with MQTT cloud connectivity
//!
//! This module provides the `Shadow` struct for shadow state persistence
//! using the `StateStore` abstraction combined with MQTT-based AWS IoT Shadow
//! communication. It handles:
//! - First boot initialization with defaults
//! - Normal boot loading from storage
//! - Schema change detection for migrations (with KV stores)
//! - OTA-safe migration with commit() for schema transitions
//! - Cloud synchronization via MQTT (wait_delta, update, sync_shadow, etc.)

mod cloud;

#[cfg(all(test, feature = "std", feature = "shadows_kv_persist"))]
mod tests;

use core::marker::PhantomData;

use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

use crate::mqtt::MqttClient;

use crate::shadows::{
    commit::CommitStats, error::KvError, migration::LoadResult, store::StateStore, ShadowRoot,
};

/// A shadow instance managing state persistence via StateStore with MQTT cloud connectivity.
///
/// `Shadow` combines state persistence with AWS IoT Shadow MQTT communication.
/// It provides a unified API for:
/// - Loading/persisting state from storage
/// - Receiving delta updates from the cloud via `wait_delta()`
/// - Reporting state changes to the cloud via `update_reported()`
/// - Requesting state changes from the cloud via `update_desired()`
/// - Synchronizing state with the cloud via `sync_shadow()`
///
/// ## Stateless Design
///
/// Shadow does NOT hold state internally. State is stored in the StateStore:
/// - `InMemory<S>`: Holds state in a Mutex, returns clones on access
/// - `SequentialKVStore`/`FileKVStore`: Reconstructs state from storage on access
///
/// This enables memory-efficient operation for flash-backed shadows during idle periods.
///
/// ## Lifetime and Sharing
///
/// Shadow holds borrowed references to the StateStore (`&'a K`) and MQTT client
/// (`&'m MqttClient`), not ownership. This allows multiple Shadow instances
/// to share the same StateStore and MQTT client:
///
/// ```ignore
/// // StateStore uses interior mutability (Mutex inside)
/// let store = InMemory::<DeviceShadow>::new();
///
/// // Multiple shadows share via & references
/// let device = Shadow::<DeviceShadow, _, _>::new(&store, &mqtt);
/// let network = Shadow::<NetworkShadow, _, _>::new(&store, &mqtt);
/// ```
///
/// For embedded use, store the StateStore in a `StaticCell`:
/// ```ignore
/// static STORE: StaticCell<SequentialKVStore<Flash, NoopRawMutex>> = StaticCell::new();
/// let store = STORE.init(SequentialKVStore::new(flash, range));
/// let shadow = Shadow::<DeviceShadow, _, _>::new(store, &mqtt);
/// ```
///
/// ## Cloud Communication
///
/// The typical usage pattern is to call `wait_delta()` in a loop to receive
/// cloud updates, which automatically applies deltas and persists them:
///
/// ```ignore
/// loop {
///     let (state, delta) = shadow.wait_delta().await?;
///     if delta.is_some() {
///         // State was updated from cloud
///     }
/// }
/// ```
pub struct Shadow<'a, 'm, S: ShadowRoot, C: MqttClient, K: StateStore<S>> {
    /// Reference to the StateStore (interior mutability).
    pub(crate) store: &'a K,
    /// Reference to the MQTT client for cloud communication.
    pub(crate) mqtt: &'m C,
    /// Cached subscription for delta topic.
    pub(crate) subscription: Mutex<NoopRawMutex, Option<C::Subscription<'m, 1>>>,
    /// Marker for the shadow state type.
    _marker: PhantomData<S>,
}

impl<'a, 'm, S, C, K> Shadow<'a, 'm, S, C, K>
where
    S: ShadowRoot,
    C: MqttClient,
    K: StateStore<S>,
{
    /// Create a shadow backed by a StateStore with MQTT connection.
    ///
    /// Multiple Shadow instances can share the same StateStore and MQTT client
    /// since both use interior mutability (`&self` methods).
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // For KV-based persistent storage (requires shadows_kv_persist feature):
    /// let store = SequentialKVStore::new(flash, range);
    /// let shadow = Shadow::<DeviceShadow, _, _>::new(&store, &mqtt);
    ///
    /// // For in-memory (non-persistent) storage:
    /// let store = InMemory::<DeviceShadow>::new();
    /// let shadow = Shadow::<DeviceShadow, _, _>::new(&store, &mqtt);
    ///
    /// shadow.load().await?;  // Loads from storage or initializes on first boot
    /// let state = shadow.state().await?;  // Get current state
    /// ```
    pub fn new(store: &'a K, mqtt: &'m C) -> Self {
        Self {
            store,
            mqtt,
            subscription: Mutex::new(None),
            _marker: PhantomData,
        }
    }

    /// Get the current shadow state.
    ///
    /// Returns an owned copy of the state. For `InMemory<S>`, this clones
    /// from the internal Mutex. For persistent stores, this reconstructs
    /// the state by loading all fields from storage.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let state = shadow.state().await?;
    /// let timeout = state.timeout;
    /// ```
    pub async fn state(&self) -> Result<S, K::Error> {
        self.store.get_state(Self::prefix()).await
    }

    /// Get the shadow name prefix for storage keys.
    pub(crate) fn prefix() -> &'static str {
        S::NAME.unwrap_or("classic")
    }

    /// Load state from storage, or initialize on first boot.
    ///
    /// ## Behavior
    ///
    /// | Scenario | Action |
    /// |----------|--------|
    /// | First boot (no hash) | Initialize defaults, persist all, write hash |
    /// | Hash matches | Load fields from storage |
    /// | Hash mismatch | Load with migration, don't write hash |
    ///
    /// ## OTA Safety
    ///
    /// When `schema_changed` is true, the new schema hash is NOT written.
    /// Call `commit()` after boot is confirmed successful to:
    /// 1. Write the new schema hash
    /// 2. Clean up orphaned keys from old schema
    ///
    /// This ensures OTA rollback works - old firmware will still see its hash.
    pub async fn load(&self) -> Result<LoadResult<S>, KvError<K::Error>> {
        self.store.load(Self::prefix(), S::SCHEMA_HASH).await
    }

    /// Apply a delta and return the updated state.
    ///
    /// This is an internal helper method used by the cloud methods (`wait_delta()`,
    /// `update()`, `sync_shadow()`, etc.) to persist delta changes.
    ///
    /// ## Why Private?
    ///
    /// Users should use the cloud methods instead, which handle both persistence
    /// and cloud acknowledgment in a single operation.
    pub(crate) async fn apply_and_save(&self, delta: &S::Delta) -> Result<S, KvError<K::Error>> {
        self.store
            .apply_delta(Self::prefix(), delta)
            .await
            .map_err(KvError::Kv)
    }

    /// Commit schema changes and clean up orphaned keys.
    ///
    /// Call this method after boot is confirmed successful (e.g., after OTA
    /// image is marked as valid). This method:
    ///
    /// 1. **Writes the new schema hash** - confirms the schema migration
    /// 2. **Removes orphaned keys** - cleans up old migration source keys
    ///    and fields removed from the schema
    ///
    /// ## OTA Safety
    ///
    /// Until `commit()` is called, the old schema hash remains in storage.
    /// If the device reboots before `commit()` (e.g., OTA rollback), the old
    /// firmware will see its original hash and work correctly.
    ///
    /// ## In-Memory Stores
    ///
    /// For `InMemory<S>`, this is a no-op since there's no persistent storage.
    pub async fn commit(&self) -> Result<CommitStats, KvError<K::Error>> {
        self.store.commit(Self::prefix(), S::SCHEMA_HASH).await
    }
}
