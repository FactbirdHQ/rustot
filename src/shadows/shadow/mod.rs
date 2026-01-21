//! Shadow state persistence with MQTT cloud connectivity
//!
//! This module provides the `Shadow` struct for shadow state persistence
//! using the `KVStore` abstraction combined with MQTT-based AWS IoT Shadow
//! communication. It handles:
//! - First boot initialization with defaults
//! - Normal boot loading from KV storage
//! - Schema change detection for migrations
//! - OTA-safe migration with commit() for schema transitions
//! - Cloud synchronization via MQTT (wait_delta, update, sync_shadow, etc.)

mod cloud;

#[cfg(all(test, feature = "std"))]
mod tests;

use embassy_sync::{
    blocking_mutex::raw::{NoopRawMutex, RawMutex},
    mutex::Mutex,
};

use crate::shadows::{
    commit::CommitStats,
    error::KvError,
    kv_store::{KVStore, NoPersist},
    migration::LoadResult,
    ShadowRoot,
};

/// Suffix for schema hash keys.
const SCHEMA_HASH_SUFFIX: &str = "/__schema_hash__";

/// Static reference to NoPersist for in-memory shadows.
/// NoPersist is a ZST, so this has no runtime cost.
static NO_PERSIST: NoPersist = NoPersist;

/// A shadow instance managing state persistence via KVStore with MQTT cloud connectivity.
///
/// `Shadow` combines KV-based field-level persistence with AWS IoT Shadow MQTT
/// communication. It provides a unified API for:
/// - Loading/persisting state from KV storage
/// - Receiving delta updates from the cloud via `wait_delta()`
/// - Reporting state changes to the cloud via `update()`
/// - Requesting state changes from the cloud via `update_desired()`
/// - Synchronizing state with the cloud via `sync_shadow()`
///
/// ## Constructors
///
/// - `new_in_memory(mqtt)` - Non-persisted shadow using `NoPersist` KVStore
/// - `new_persistent(kv, mqtt)` - Persisted shadow backed by a KVStore
///
/// ## Lifetime and Sharing
///
/// Shadow holds borrowed references to the KVStore (`&'a K`) and MQTT client
/// (`&'m MqttClient`), not ownership. This allows multiple Shadow instances
/// to share the same KVStore and MQTT client:
///
/// ```ignore
/// // KVStore uses interior mutability (Mutex inside)
/// let kv = SequentialKVStore::<_, NoopRawMutex>::new(flash, range);
///
/// // Multiple shadows share via & references
/// let mut device = Shadow::<DeviceShadow, _, _>::new_persistent(&kv, &mqtt);
/// let mut network = Shadow::<NetworkShadow, _, _>::new_persistent(&kv, &mqtt);
/// ```
///
/// For embedded use, store the KVStore in a `StaticCell`:
/// ```ignore
/// static KV: StaticCell<SequentialKVStore<Flash, NoopRawMutex>> = StaticCell::new();
/// let kv = KV.init(SequentialKVStore::new(flash, range));
/// let mut shadow = Shadow::<DeviceShadow, _, _>::new_persistent(kv, &mqtt);
/// ```
///
/// ## Non-Persisted Shadows
///
/// For testing or volatile state that shouldn't survive reboots:
/// ```ignore
/// let mut shadow = Shadow::<DeviceShadow, _, NoPersist>::new_in_memory(&mqtt);
/// shadow.load().await?;  // Always "first boot", initializes defaults
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
pub struct Shadow<'a, 'm, S, M: RawMutex, K: KVStore = NoPersist> {
    /// The shadow state (private - access via state() method).
    pub(crate) state: S,
    /// Reference to the KVStore (interior mutability).
    pub(crate) kv_store: &'a K,
    /// Reference to the MQTT client for cloud communication.
    pub(crate) mqtt: &'m embedded_mqtt::MqttClient<'a, M>,
    /// Cached subscription for delta topic.
    pub(crate) subscription: Mutex<NoopRawMutex, Option<embedded_mqtt::Subscription<'a, 'm, M, 2>>>,
}

impl<'a, 'm, S: ShadowRoot, M: RawMutex> Shadow<'a, 'm, S, M, NoPersist> {
    /// Create a non-persisted shadow (in-memory only) with MQTT connection.
    ///
    /// The shadow state is initialized with defaults and not persisted to
    /// any storage. Useful for testing or volatile state that shouldn't
    /// survive reboots. Cloud communication is still available via MQTT.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let mut shadow = Shadow::<DeviceShadow, _, NoPersist>::new_in_memory(&mqtt);
    /// shadow.load().await?;  // Initializes with defaults
    ///
    /// // Cloud communication works normally
    /// let (state, delta) = shadow.wait_delta().await?;
    /// ```
    pub fn new_in_memory(mqtt: &'m embedded_mqtt::MqttClient<'a, M>) -> Self {
        Self {
            state: S::default(),
            kv_store: &NO_PERSIST,
            mqtt,
            subscription: Mutex::new(None),
        }
    }
}

impl<'a, 'm, S, M, K> Shadow<'a, 'm, S, M, K>
where
    S: ShadowRoot,
    M: RawMutex,
    K: KVStore,
{
    /// Create a persisted shadow backed by a KVStore with MQTT connection.
    ///
    /// Multiple Shadow instances can share the same KVStore and MQTT client
    /// since both use interior mutability (`&self` methods).
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let kv = SequentialKVStore::new(flash, range);
    /// let mut shadow = Shadow::<DeviceShadow, _, _>::new_persistent(&kv, &mqtt);
    /// shadow.load().await?;  // Loads from KV or initializes on first boot
    ///
    /// // Then use cloud methods
    /// let (state, delta) = shadow.wait_delta().await?;
    /// ```
    pub fn new_persistent(
        kv_store: &'a K,
        mqtt: &'m embedded_mqtt::MqttClient<'a, M>,
    ) -> Self {
        Self {
            state: S::default(),
            kv_store,
            mqtt,
            subscription: Mutex::new(None),
        }
    }

    /// Get an immutable reference to the shadow state.
    ///
    /// This is the accessor for the private `state` field.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let timeout = shadow.state().timeout;
    /// ```
    pub fn state(&self) -> &S {
        &self.state
    }

    /// Get the shadow name prefix for KV keys.
    pub(crate) fn prefix() -> &'static str {
        S::NAME.unwrap_or("classic")
    }

    /// Load state from KV store, or initialize on first boot.
    ///
    /// ## Behavior
    ///
    /// | Scenario | Action |
    /// |----------|--------|
    /// | First boot (no hash) | Initialize defaults, persist all, write hash |
    /// | Hash matches | Load fields from KV |
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
    pub async fn load(&mut self) -> Result<LoadResult, KvError<K::Error>> {
        let prefix = Self::prefix();

        // Build hash key: prefix + SCHEMA_HASH_SUFFIX (e.g., "device/__schema_hash__")
        let mut hash_key: heapless::String<128> = heapless::String::new();
        hash_key.push_str(prefix).map_err(|_| KvError::KeyTooLong)?;
        hash_key
            .push_str(SCHEMA_HASH_SUFFIX)
            .map_err(|_| KvError::KeyTooLong)?;

        let mut hash_buf = [0u8; 8];
        match self
            .kv_store
            .fetch(&hash_key, &mut hash_buf)
            .await
            .map_err(KvError::Kv)?
        {
            None => {
                // First boot - no hash exists
                let result = self.initialize_first_boot(prefix).await?;
                Ok(result)
            }
            Some(slice) if slice.len() == 8 => {
                let stored_hash = u64::from_le_bytes(slice.try_into().unwrap());
                if stored_hash == S::SCHEMA_HASH {
                    // Hash matches - normal load
                    let result = self.load_fields(prefix).await?;
                    Ok(result)
                } else {
                    // Hash mismatch - migration needed
                    let result = self.load_fields_with_migration(prefix).await?;
                    Ok(result)
                }
            }
            Some(_) => {
                // Invalid hash length - treat as first boot
                let result = self.initialize_first_boot(prefix).await?;
                Ok(result)
            }
        }
    }

    /// Initialize on first boot: persist defaults and write schema hash.
    async fn initialize_first_boot(
        &mut self,
        prefix: &str,
    ) -> Result<LoadResult, KvError<K::Error>> {
        // state is already Default::default() from constructor
        let mut buf = [0u8; 512];

        // Persist all fields using per-field codegen
        // Use 128 as key buffer size - sufficient for most shadow paths
        self.state
            .persist_to_kv::<K, 128>(prefix, self.kv_store, &mut buf)
            .await?;

        // Write schema hash
        let mut hash_key: heapless::String<128> = heapless::String::new();
        hash_key.push_str(prefix).map_err(|_| KvError::KeyTooLong)?;
        hash_key
            .push_str(SCHEMA_HASH_SUFFIX)
            .map_err(|_| KvError::KeyTooLong)?;

        self.kv_store
            .store(&hash_key, &S::SCHEMA_HASH.to_le_bytes())
            .await
            .map_err(KvError::Kv)?;

        // Count fields by collecting valid keys
        let mut total_fields = 0usize;
        S::collect_valid_keys::<128>(prefix, &mut |_| {
            total_fields += 1;
        });

        Ok(LoadResult {
            first_boot: true,
            schema_changed: false,
            fields_loaded: 0,
            fields_migrated: 0,
            fields_defaulted: total_fields,
        })
    }

    /// Load fields from KV (normal boot, hash matches).
    ///
    /// Uses per-field codegen for path-based deserialization.
    /// Each type loads itself: enums read `_variant` first, then inner fields.
    async fn load_fields(&mut self, prefix: &str) -> Result<LoadResult, KvError<K::Error>> {
        let mut buf = [0u8; 512];
        let field_result = self
            .state
            .load_from_kv::<K, 128>(prefix, self.kv_store, &mut buf)
            .await?;

        Ok(LoadResult {
            first_boot: false,
            schema_changed: false,
            fields_loaded: field_result.loaded,
            fields_migrated: 0,
            fields_defaulted: field_result.defaulted,
        })
    }

    /// Load fields with migration (hash mismatch).
    ///
    /// Uses per-field codegen for path-based deserialization with migration support.
    /// Each type loads itself, trying migration sources when primary key fails.
    ///
    /// ## OTA Safety
    ///
    /// When migration occurs, the schema hash is NOT updated. Call `commit()`
    /// after boot is confirmed successful to finalize the migration.
    async fn load_fields_with_migration(
        &mut self,
        prefix: &str,
    ) -> Result<LoadResult, KvError<K::Error>> {
        let mut buf = [0u8; 512];
        let field_result = self
            .state
            .load_from_kv_with_migration::<K, 128>(prefix, self.kv_store, &mut buf)
            .await?;

        Ok(LoadResult {
            first_boot: false,
            schema_changed: true,
            fields_loaded: field_result.loaded,
            fields_migrated: field_result.migrated,
            fields_defaulted: field_result.defaulted,
        })
    }

    /// Apply a delta and save only the changed fields to KV storage.
    ///
    /// This is an internal helper method used by the cloud methods (`wait_delta()`,
    /// `update()`, `sync_shadow()`, etc.) to persist delta changes.
    ///
    /// ## Why Private?
    ///
    /// Users should use the cloud methods instead, which handle both persistence
    /// and cloud acknowledgment in a single operation.
    ///
    /// ## Why `&S::Delta` (by reference)?
    ///
    /// Taking delta by reference allows callers to retain the delta after
    /// applying, which is needed for patterns like `wait_delta()` that return
    /// the delta to the caller:
    ///
    /// ```ignore
    /// if let Some(ref delta) = received_delta {
    ///     shadow.apply_and_save(delta).await?;
    /// }
    /// Ok((state, received_delta))  // Can still return delta!
    /// ```
    ///
    /// This eliminates the need for `Clone` on Delta.
    ///
    /// ## How It Works
    ///
    /// Delegates to `ShadowNode::apply_and_persist()` which is generated by
    /// the derive macro. The generated code handles:
    ///
    /// 1. Checking each delta field for `Some` values
    /// 2. Updating the corresponding state field
    /// 3. Serializing and persisting to KV storage
    /// 4. Enum variant changes (writes `/_variant` key)
    pub(crate) async fn apply_and_save(&mut self, delta: &S::Delta) -> Result<(), KvError<K::Error>> {
        let prefix = Self::prefix();
        let mut buf = [0u8; 512];

        self.state
            .apply_and_persist(delta, prefix, self.kv_store, &mut buf)
            .await
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
    /// ## Orphan Detection
    ///
    /// Identifies orphaned keys by comparing stored keys against valid keys:
    /// - All current field keys (including ALL enum variant fields)
    /// - All `_variant` keys for enum fields
    ///
    /// **Important**: Inactive enum variant fields are NOT removed - they are
    /// valid keys. Only truly orphaned keys (removed fields, removed variants,
    /// and migration source keys) are cleaned up after commit.
    ///
    /// ## no_std Compatibility
    ///
    /// Uses `remove_if()` on KVStore which handles batching internally.
    /// Each implementation optimizes for its backend:
    /// - SequentialKVStore: 4-key buffer with loop
    /// - FileKVStore: single-pass with Vec (has alloc)
    pub async fn commit(&mut self) -> Result<CommitStats, KvError<K::Error>> {
        let prefix = Self::prefix();

        // Build set of valid keys for O(1) lookup during GC
        // Using heapless::FnvIndexSet for no_std compatibility
        //
        // Valid keys come from collect_valid_keys() which reports:
        // - All current field keys (including ALL enum variant fields)
        // - All `_variant` keys for enum fields
        let mut valid: heapless::FnvIndexSet<heapless::String<128>, 128> =
            heapless::FnvIndexSet::new();

        // Collect all valid keys using per-field codegen
        S::collect_valid_keys::<128>(prefix, &mut |key| {
            // Strip prefix to get relative key for comparison
            let rel_key = key.strip_prefix(prefix).unwrap_or(key);
            let mut hs: heapless::String<128> = heapless::String::new();
            let _ = hs.push_str(rel_key);
            let _ = valid.insert(hs);
        });

        // NOTE: Migration source keys are NOT added to valid set.
        // After commit(), migration is finalized and old keys should be removed.
        // This is OTA-safe because commit() is only called after boot is confirmed.

        // Remove orphaned keys using KVStore::remove_if
        // The implementation handles batching internally
        let orphans_removed = self
            .kv_store
            .remove_if(prefix, |key| {
                // Strip prefix to get relative key for comparison
                // Key format: "device/config/timeout" -> "/config/timeout"
                let rel_key = key.strip_prefix(prefix).unwrap_or(key);

                // Remove if not in valid set and not a system key
                let mut rel_key_string: heapless::String<128> = heapless::String::new();
                let _ = rel_key_string.push_str(rel_key);
                !valid.contains(&rel_key_string) && !rel_key.starts_with("/__")
            })
            .await
            .map_err(KvError::Kv)?;

        // Update schema hash to current
        let mut hash_key: heapless::String<128> = heapless::String::new();
        hash_key.push_str(prefix).map_err(|_| KvError::KeyTooLong)?;
        hash_key
            .push_str(SCHEMA_HASH_SUFFIX)
            .map_err(|_| KvError::KeyTooLong)?;

        self.kv_store
            .store(&hash_key, &S::SCHEMA_HASH.to_le_bytes())
            .await
            .map_err(KvError::Kv)?;

        Ok(CommitStats {
            orphans_removed,
            schema_hash_updated: true,
        })
    }
}

// =============================================================================
// Test-Only Shadow (no MQTT)
// =============================================================================

/// Test-only shadow for KV persistence testing without MQTT.
///
/// This struct provides KV-only functionality for testing purposes.
/// It mirrors the KV-related methods of `Shadow` but doesn't require MQTT.
#[cfg(test)]
pub struct ShadowKvOnly<'a, S, K: KVStore = NoPersist> {
    /// The shadow state.
    pub state: S,
    /// Reference to the KVStore (interior mutability).
    kv_store: &'a K,
}

#[cfg(test)]
impl<S: ShadowRoot> ShadowKvOnly<'static, S, NoPersist> {
    /// Create a non-persisted shadow (in-memory only).
    pub fn new_in_memory() -> Self {
        Self {
            state: S::default(),
            kv_store: &NO_PERSIST,
        }
    }
}

#[cfg(test)]
impl<'a, S, K> ShadowKvOnly<'a, S, K>
where
    S: ShadowRoot,
    K: KVStore,
{
    /// Create a persisted shadow backed by a KVStore.
    pub fn new_persistent(kv_store: &'a K) -> Self {
        Self {
            state: S::default(),
            kv_store,
        }
    }

    /// Get the shadow name prefix for KV keys.
    fn prefix() -> &'static str {
        S::NAME.unwrap_or("classic")
    }

    /// Load state from KV store, or initialize on first boot.
    pub async fn load(&mut self) -> Result<LoadResult, KvError<K::Error>> {
        let prefix = Self::prefix();

        let mut hash_key: heapless::String<128> = heapless::String::new();
        hash_key.push_str(prefix).map_err(|_| KvError::KeyTooLong)?;
        hash_key
            .push_str(SCHEMA_HASH_SUFFIX)
            .map_err(|_| KvError::KeyTooLong)?;

        let mut hash_buf = [0u8; 8];
        match self
            .kv_store
            .fetch(&hash_key, &mut hash_buf)
            .await
            .map_err(KvError::Kv)?
        {
            None => {
                let result = self.initialize_first_boot(prefix).await?;
                Ok(result)
            }
            Some(slice) if slice.len() == 8 => {
                let stored_hash = u64::from_le_bytes(slice.try_into().unwrap());
                if stored_hash == S::SCHEMA_HASH {
                    let result = self.load_fields(prefix).await?;
                    Ok(result)
                } else {
                    let result = self.load_fields_with_migration(prefix).await?;
                    Ok(result)
                }
            }
            Some(_) => {
                let result = self.initialize_first_boot(prefix).await?;
                Ok(result)
            }
        }
    }

    async fn initialize_first_boot(
        &mut self,
        prefix: &str,
    ) -> Result<LoadResult, KvError<K::Error>> {
        let mut buf = [0u8; 512];
        self.state
            .persist_to_kv::<K, 128>(prefix, self.kv_store, &mut buf)
            .await?;

        let mut hash_key: heapless::String<128> = heapless::String::new();
        hash_key.push_str(prefix).map_err(|_| KvError::KeyTooLong)?;
        hash_key
            .push_str(SCHEMA_HASH_SUFFIX)
            .map_err(|_| KvError::KeyTooLong)?;

        self.kv_store
            .store(&hash_key, &S::SCHEMA_HASH.to_le_bytes())
            .await
            .map_err(KvError::Kv)?;

        let mut total_fields = 0usize;
        S::collect_valid_keys::<128>(prefix, &mut |_| {
            total_fields += 1;
        });

        Ok(LoadResult {
            first_boot: true,
            schema_changed: false,
            fields_loaded: 0,
            fields_migrated: 0,
            fields_defaulted: total_fields,
        })
    }

    async fn load_fields(&mut self, prefix: &str) -> Result<LoadResult, KvError<K::Error>> {
        let mut buf = [0u8; 512];
        let field_result = self
            .state
            .load_from_kv::<K, 128>(prefix, self.kv_store, &mut buf)
            .await?;

        Ok(LoadResult {
            first_boot: false,
            schema_changed: false,
            fields_loaded: field_result.loaded,
            fields_migrated: 0,
            fields_defaulted: field_result.defaulted,
        })
    }

    async fn load_fields_with_migration(
        &mut self,
        prefix: &str,
    ) -> Result<LoadResult, KvError<K::Error>> {
        let mut buf = [0u8; 512];
        let field_result = self
            .state
            .load_from_kv_with_migration::<K, 128>(prefix, self.kv_store, &mut buf)
            .await?;

        Ok(LoadResult {
            first_boot: false,
            schema_changed: true,
            fields_loaded: field_result.loaded,
            fields_migrated: field_result.migrated,
            fields_defaulted: field_result.defaulted,
        })
    }

    /// Apply a delta and save only the changed fields to KV storage.
    pub async fn apply_and_save(&mut self, delta: &S::Delta) -> Result<(), KvError<K::Error>> {
        let prefix = Self::prefix();
        let mut buf = [0u8; 512];

        self.state
            .apply_and_persist(delta, prefix, self.kv_store, &mut buf)
            .await
    }

    /// Commit schema changes and clean up orphaned keys.
    pub async fn commit(&mut self) -> Result<CommitStats, KvError<K::Error>> {
        let prefix = Self::prefix();

        let mut valid: heapless::FnvIndexSet<heapless::String<128>, 128> =
            heapless::FnvIndexSet::new();

        S::collect_valid_keys::<128>(prefix, &mut |key| {
            let rel_key = key.strip_prefix(prefix).unwrap_or(key);
            let mut hs: heapless::String<128> = heapless::String::new();
            let _ = hs.push_str(rel_key);
            let _ = valid.insert(hs);
        });

        let orphans_removed = self
            .kv_store
            .remove_if(prefix, |key| {
                let rel_key = key.strip_prefix(prefix).unwrap_or(key);
                let mut rel_key_string: heapless::String<128> = heapless::String::new();
                let _ = rel_key_string.push_str(rel_key);
                !valid.contains(&rel_key_string) && !rel_key.starts_with("/__")
            })
            .await
            .map_err(KvError::Kv)?;

        let mut hash_key: heapless::String<128> = heapless::String::new();
        hash_key.push_str(prefix).map_err(|_| KvError::KeyTooLong)?;
        hash_key
            .push_str(SCHEMA_HASH_SUFFIX)
            .map_err(|_| KvError::KeyTooLong)?;

        self.kv_store
            .store(&hash_key, &S::SCHEMA_HASH.to_le_bytes())
            .await
            .map_err(KvError::Kv)?;

        Ok(CommitStats {
            orphans_removed,
            schema_hash_updated: true,
        })
    }
}
