//! KV-based shadow persistence
//!
//! This module provides the `KvShadow` struct for shadow state persistence
//! using the `KVStore` abstraction. It handles:
//! - First boot initialization with defaults
//! - Normal boot loading from KV storage
//! - Schema change detection for migrations
//! - OTA-safe migration with commit() for schema transitions

use crate::shadows::{
    commit::CommitStats,
    error::KvError,
    kv_store::{path_to_key, KVStore, KeyPath, NoPersist},
    migration::LoadResult,
    KeySet, ShadowRoot,
};
use miniconf::{SerdeError, ValueError};

/// Suffix for schema hash keys.
const SCHEMA_HASH_SUFFIX: &str = "/__schema_hash__";

/// Static reference to NoPersist for in-memory shadows.
/// NoPersist is a ZST, so this has no runtime cost.
static NO_PERSIST: NoPersist = NoPersist;

/// A shadow instance managing state persistence via KVStore.
///
/// ## Constructors
///
/// - `new_in_memory()` - Non-persisted shadow using `NoPersist` KVStore
/// - `new_persistent(&kv)` - Persisted shadow backed by a KVStore
///
/// ## Lifetime and Sharing
///
/// KvShadow holds a borrowed reference to the KVStore (`&'a K`), not ownership.
/// This allows multiple KvShadow instances to share the same KVStore:
///
/// ```ignore
/// // KVStore uses interior mutability (Mutex inside)
/// let kv = SequentialKVStore::<_, NoopRawMutex>::new(flash, range);
///
/// // Multiple shadows share via & reference
/// let mut device = KvShadow::<DeviceShadow, _>::new_persistent(&kv);
/// let mut network = KvShadow::<NetworkShadow, _>::new_persistent(&kv);
/// ```
///
/// For embedded use, store the KVStore in a `StaticCell`:
/// ```ignore
/// static KV: StaticCell<SequentialKVStore<Flash, NoopRawMutex>> = StaticCell::new();
/// let kv = KV.init(SequentialKVStore::new(flash, range));
/// let mut shadow = KvShadow::<DeviceShadow, _>::new_persistent(kv);
/// ```
///
/// ## Non-Persisted Shadows
///
/// For testing or volatile state that shouldn't survive reboots:
/// ```ignore
/// let mut shadow = KvShadow::<DeviceShadow, NoPersist>::new_in_memory();
/// shadow.load().await?;  // Always "first boot", initializes defaults
/// ```
pub struct KvShadow<'a, S, K: KVStore = NoPersist> {
    /// The shadow state.
    pub state: S,
    /// Reference to the KVStore (interior mutability).
    kv_store: &'a K,
}

impl<S: ShadowRoot> KvShadow<'static, S, NoPersist> {
    /// Create a non-persisted shadow (in-memory only).
    ///
    /// The shadow state is initialized with defaults and not persisted to
    /// any storage. Useful for testing or volatile state that shouldn't
    /// survive reboots.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let mut shadow = KvShadow::<DeviceShadow, NoPersist>::new_in_memory();
    /// shadow.load().await?;  // Initializes with defaults
    /// ```
    pub fn new_in_memory() -> Self {
        Self {
            state: S::default(),
            kv_store: &NO_PERSIST,
        }
    }
}

impl<'a, S, K> KvShadow<'a, S, K>
where
    S: ShadowRoot,
    K: KVStore,
{
    /// Create a persisted shadow backed by a KVStore.
    ///
    /// Multiple KvShadow instances can share the same KVStore since
    /// KVStore uses interior mutability (`&self` methods).
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let kv = SequentialKVStore::new(flash, range);
    /// let mut shadow = KvShadow::<DeviceShadow, _>::new_persistent(&kv);
    /// shadow.load().await?;  // Loads from KV or initializes on first boot
    /// ```
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

        // Apply any custom field defaults
        for field_path in S::keys(KeySet::Schema) {
            S::apply_field_default(&mut self.state, field_path);
        }

        // Count total fields for result
        let total_fields = S::keys(KeySet::Schema).count();

        // Persist all fields
        self.persist_all_fields(prefix).await?;

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

        Ok(LoadResult {
            first_boot: true,
            schema_changed: false,
            fields_loaded: 0,
            fields_migrated: 0,
            fields_defaulted: total_fields,
        })
    }

    /// Persist all fields to KV storage.
    ///
    /// Uses miniconf's postcard integration for path-based serialization.
    /// Iterates using miniconf's SCHEMA.nodes() which returns `Path` objects.
    async fn persist_all_fields(&mut self, prefix: &str) -> Result<(), KvError<K::Error>> {
        // Use fixed-size buffer - MAX_VALUE_LEN is compile-time checked by implementations
        let mut buf = [0u8; 512];

        // Iterate all leaf nodes using miniconf's Path type
        // Use depth 8 as a reasonable maximum nesting depth
        for path_result in S::SCHEMA.nodes::<KeyPath<128>, 8>() {
            let path = path_result.map_err(|_| KvError::PathNotFound)?;

            // Skip system keys (e.g., /__schema_hash__)
            if path.as_ref().starts_with("/__") {
                continue;
            }

            // Build full KV key: prefix + path
            let full_key = path_to_key::<256, 128>(prefix, &path);

            // Serialize field value using miniconf's path-based API
            match miniconf::postcard::get_by_key(
                &self.state,
                &path,
                postcard::ser_flavors::Slice::new(&mut buf),
            ) {
                Ok(slice) => {
                    self.kv_store
                        .store(&full_key, slice)
                        .await
                        .map_err(KvError::Kv)?;
                }
                Err(SerdeError::Value(ValueError::Absent)) => {
                    // Field belongs to inactive enum variant - skip
                    continue;
                }
                Err(_) => return Err(KvError::Serialization),
            }
        }

        Ok(())
    }

    /// Load fields from KV (normal boot, hash matches).
    ///
    /// Uses miniconf's postcard integration for path-based deserialization.
    ///
    /// ## Implementation Strategy
    ///
    /// **For structs:** Generic miniconf traversal - iterate schema nodes, fetch
    /// each field from KV, deserialize via miniconf. `ValueError::Absent` indicates
    /// inactive enum variant fields (expected, skip them).
    ///
    /// **For enums:** Handled in Phase 6's two-phase loading - `_variant` keys are
    /// read first to set up variants, then field loading proceeds.
    async fn load_fields(&mut self, prefix: &str) -> Result<LoadResult, KvError<K::Error>> {
        // Use fixed-size buffer
        let mut buf = [0u8; 512];
        let mut fields_loaded = 0;
        let mut fields_defaulted = 0;

        // Iterate all leaf nodes using miniconf's Path type
        // Use depth 8 as a reasonable maximum nesting depth
        for path_result in S::SCHEMA.nodes::<KeyPath<128>, 8>() {
            let path = path_result.map_err(|_| KvError::PathNotFound)?;

            // Skip system keys
            if path.as_ref().starts_with("/__") {
                continue;
            }

            // Build full KV key: prefix + path
            let full_key = path_to_key::<256, 128>(prefix, &path);

            if let Some(data) = self
                .kv_store
                .fetch(&full_key, &mut buf)
                .await
                .map_err(KvError::Kv)?
            {
                // Deserialize field using miniconf's path-based API
                match miniconf::postcard::set_by_key(
                    &mut self.state,
                    &path,
                    postcard::de_flavors::Slice::new(data),
                ) {
                    Ok(_) => {
                        fields_loaded += 1;
                    }
                    Err(SerdeError::Value(ValueError::Absent)) => {
                        // Field belongs to inactive enum variant - skip
                        // This is expected for enum variant fields when a different variant is active
                        continue;
                    }
                    Err(_) => return Err(KvError::Serialization),
                }
            } else {
                // Field not in KV - apply default
                S::apply_field_default(&mut self.state, path.as_ref());
                fields_defaulted += 1;
            }
        }

        Ok(LoadResult {
            first_boot: false,
            schema_changed: false,
            fields_loaded,
            fields_migrated: 0,
            fields_defaulted,
        })
    }

    /// Load fields with migration (hash mismatch).
    ///
    /// This method handles schema changes by:
    /// 1. Trying to load from the primary key first
    /// 2. If not found (or deserialization fails), trying migration sources
    /// 3. Performing "soft migration" - writing to new key while preserving old
    ///
    /// ## OTA Safety
    ///
    /// When migration occurs, the schema hash is NOT updated. Call `commit()`
    /// after boot is confirmed successful to finalize the migration.
    async fn load_fields_with_migration(
        &mut self,
        prefix: &str,
    ) -> Result<LoadResult, KvError<K::Error>> {
        // Use fixed-size buffer
        let mut buf = [0u8; 512];
        let mut fields_loaded = 0;
        let mut fields_migrated = 0;
        let mut fields_defaulted = 0;

        // Iterate all leaf nodes using miniconf's Path type
        // Use depth 8 as a reasonable maximum nesting depth
        for path_result in S::SCHEMA.nodes::<KeyPath<128>, 8>() {
            let path = path_result.map_err(|_| KvError::PathNotFound)?;
            let field_path = path.as_ref();

            // Skip system keys
            if field_path.starts_with("/__") {
                continue;
            }

            // Build full KV key: prefix + path
            let full_key = path_to_key::<256, 128>(prefix, &path);

            // Try primary key first
            if let Some(data) = self
                .kv_store
                .fetch(&full_key, &mut buf)
                .await
                .map_err(KvError::Kv)?
            {
                // Try to deserialize with current type
                match miniconf::postcard::set_by_key(
                    &mut self.state,
                    &path,
                    postcard::de_flavors::Slice::new(data),
                ) {
                    Ok(_) => {
                        fields_loaded += 1;
                        continue;
                    }
                    Err(SerdeError::Value(ValueError::Absent)) => {
                        // Field belongs to inactive enum variant - skip
                        continue;
                    }
                    Err(_) => {
                        // Deserialization failed - try migration with type conversion
                        if self
                            .try_migrations(prefix, field_path, &full_key, &mut buf)
                            .await?
                        {
                            fields_migrated += 1;
                            continue;
                        }
                        // Migration failed too, apply default
                        S::apply_field_default(&mut self.state, field_path);
                        fields_defaulted += 1;
                    }
                }
            } else {
                // Key not found - try migration sources
                if self
                    .try_migrations(prefix, field_path, &full_key, &mut buf)
                    .await?
                {
                    fields_migrated += 1;
                } else {
                    // No migration source - apply default
                    S::apply_field_default(&mut self.state, field_path);
                    fields_defaulted += 1;
                }
            }
        }

        Ok(LoadResult {
            first_boot: false,
            schema_changed: true,
            fields_loaded,
            fields_migrated,
            fields_defaulted,
        })
    }

    /// Try to migrate a field from old keys.
    ///
    /// Iterates through migration sources for this field, trying each in order.
    /// On success, performs "soft migration": writes to new key while preserving old.
    ///
    /// # Arguments
    ///
    /// * `prefix` - Shadow prefix (e.g., "device")
    /// * `field_path` - Field path (e.g., "/config/timeout")
    /// * `new_key` - Full key for the new location (e.g., "device/config/timeout")
    /// * `buf` - Buffer for reading/converting values
    /// * `is_type_conversion` - If true, the primary key exists but deserialization failed
    ///
    /// # Returns
    ///
    /// `Ok(true)` if migration succeeded, `Ok(false)` if no migration source found.
    async fn try_migrations(
        &mut self,
        prefix: &str,
        field_path: &str,
        new_key: &str,
        buf: &mut [u8],
    ) -> Result<bool, KvError<K::Error>> {
        for source in S::migration_sources(field_path) {
            // Build old key from migration source
            let mut old_key: heapless::String<256> = heapless::String::new();
            old_key.push_str(prefix).map_err(|_| KvError::KeyTooLong)?;
            old_key
                .push_str(source.key)
                .map_err(|_| KvError::KeyTooLong)?;

            // Use a separate buffer for fetch to avoid borrow conflicts
            let mut fetch_buf = [0u8; 512];
            if let Some(data) = self
                .kv_store
                .fetch(&old_key, &mut fetch_buf)
                .await
                .map_err(KvError::Kv)?
            {
                // Copy data to main buffer for processing
                let data_len = data.len();
                buf[..data_len].copy_from_slice(data);

                // Apply type conversion if specified
                let value_len = if let Some(convert) = source.convert {
                    let mut convert_buf = [0u8; 512];
                    let new_len =
                        convert(&buf[..data_len], &mut convert_buf).map_err(KvError::Migration)?;
                    buf[..new_len].copy_from_slice(&convert_buf[..new_len]);
                    new_len
                } else {
                    data_len
                };

                let value_bytes = &buf[..value_len];

                // Deserialize into state
                match miniconf::postcard::set_by_key(
                    &mut self.state,
                    field_path.as_bytes(),
                    postcard::de_flavors::Slice::new(value_bytes),
                ) {
                    Ok(_) => {}
                    Err(SerdeError::Value(ValueError::Absent)) => {
                        // Field belongs to inactive enum variant - skip
                        return Ok(false);
                    }
                    Err(_) => return Err(KvError::Serialization),
                }

                // Soft migrate: write to new key (preserve old for rollback)
                self.kv_store
                    .store(new_key, value_bytes)
                    .await
                    .map_err(KvError::Kv)?;

                return Ok(true);
            }
        }

        Ok(false)
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
    /// Identifies orphaned keys by comparing stored keys against
    /// `keys(KeySet::WithMigrations)` which includes:
    /// - All current field keys (including ALL enum variant fields)
    /// - All migration source keys
    /// - All `_variant` keys for enum fields
    ///
    /// **Important**: Inactive enum variant fields are NOT removed - they are
    /// valid keys. Only truly orphaned keys (removed fields, removed variants)
    /// are cleaned up.
    ///
    /// ## no_std Compatibility
    ///
    /// Uses `remove_if()` on KVStore which handles batching internally.
    /// Each implementation optimizes for its backend:
    /// - SequentialKVStore: 4-key buffer with loop
    /// - FileKVStore: single-pass with Vec (has alloc)
    pub async fn commit(&mut self) -> Result<CommitStats, KvError<K::Error>> {
        let prefix = Self::prefix();

        // Collect valid keys into a set for O(1) lookup
        // Using heapless::FnvIndexSet for no_std compatibility
        // Note: keys() returns Iter<&'static str>, so we need .copied() to get &str
        let valid: heapless::FnvIndexSet<&'static str, 64> =
            S::keys(KeySet::WithMigrations).copied().collect();

        // Remove orphaned keys using KVStore::remove_if
        // The implementation handles batching internally
        let orphans_removed = self
            .kv_store
            .remove_if(prefix, |key| {
                // Strip prefix to get relative key for comparison
                // Key format: "device/config/timeout" -> "/config/timeout"
                let rel_key = if key.starts_with(prefix) {
                    &key[prefix.len()..]
                } else {
                    key
                };

                // Remove if not in valid set and not a system key
                !valid.contains(rel_key) && !rel_key.starts_with("/__")
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

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::shadows::kv_store::FileKVStore;

    // =========================================================================
    // Infrastructure Tests (run now)
    // =========================================================================

    #[tokio::test]
    async fn test_no_persist_always_returns_none() {
        // This test validates the NoPersist KVStore behavior
        let kv = NoPersist;
        let mut buf = [0u8; 8];

        // fetch always returns None
        let result = kv.fetch("test/__schema_hash__", &mut buf).await.unwrap();
        assert!(result.is_none());

        // store is a no-op
        kv.store("test/__schema_hash__", &[1, 2, 3, 4, 5, 6, 7, 8])
            .await
            .unwrap();

        // still returns None after store
        let result = kv.fetch("test/__schema_hash__", &mut buf).await.unwrap();
        assert!(result.is_none());
    }

    // =========================================================================
    // Phase 8 Tests - Require derive macro support
    // These tests are ignored until Phase 8 provides #[shadow_root] and
    // #[shadow_node] derive macros that generate ShadowRoot implementations.
    // =========================================================================

    // Test fixtures - will be replaced with actual derived types in Phase 8
    // For now, these are placeholders to document the expected test structure.

    /*
    // Example of what the test types will look like after Phase 8:
    #[shadow_root(name = "device")]
    struct SimpleConfig {
        value: u32,
    }

    #[shadow_root(name = "device")]
    struct DeviceShadow {
        value: u32,
    }

    #[shadow_root(name = "network")]
    struct NetworkShadow {
        value: u32,
    }
    */

    // Helper functions for tests - will be used when Phase 8 tests are enabled
    #[allow(dead_code)]
    fn encode<T: serde::Serialize>(val: T) -> Vec<u8> {
        let mut buf = [0u8; 256];
        let slice = postcard::to_slice(&val, &mut buf).unwrap();
        slice.to_vec()
    }

    #[allow(dead_code)]
    async fn setup_kv(entries: &[(&str, &[u8])]) -> FileKVStore {
        let kv = FileKVStore::temp().unwrap();
        for (key, value) in entries {
            kv.store(key, value).await.unwrap();
        }
        kv
    }

    #[allow(dead_code)]
    fn empty_kv() -> FileKVStore {
        FileKVStore::temp().unwrap()
    }

    #[allow(dead_code)]
    async fn kv_has_key(kv: &FileKVStore, key: &str) -> bool {
        let mut buf = [0u8; 256];
        kv.fetch(key, &mut buf).await.unwrap().is_some()
    }

    #[allow(dead_code)]
    async fn kv_fetch_decode<T: serde::de::DeserializeOwned>(kv: &FileKVStore, key: &str) -> T {
        let mut buf = [0u8; 256];
        let data = kv.fetch(key, &mut buf).await.unwrap().unwrap();
        postcard::from_bytes(data).unwrap()
    }

    // =========================================================================
    // Schema Hash Change Detection Tests
    // =========================================================================

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for SimpleConfig"]
    async fn test_load_detects_schema_change() {
        // Store a different schema hash (simulates old firmware)
        let _kv = setup_kv(&[
            ("device/__schema_hash__", &0xDEADBEEFu64.to_le_bytes()),
            ("device/value", &encode(42u32)),
        ])
        .await;

        // Shadow borrows KVStore via & reference (interior mutability)
        // let mut shadow = KvShadow::<SimpleConfig, _>::new_persistent(&kv);
        // let result = shadow.load().await.unwrap();

        // assert!(!result.first_boot);      // Not first boot - hash existed
        // assert!(result.schema_changed);   // But hash didn't match
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for SimpleConfig"]
    async fn test_load_no_schema_change_when_hash_matches() {
        // let kv = setup_kv(&[
        //     ("device/__schema_hash__", &SimpleConfig::SCHEMA_HASH.to_le_bytes()),
        //     ("device/value", &encode(42u32)),
        // ]).await;

        // let mut shadow = KvShadow::<SimpleConfig, _>::new_persistent(&kv);
        // let result = shadow.load().await.unwrap();

        // assert!(!result.first_boot);
        // assert!(!result.schema_changed);
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for SimpleConfig"]
    async fn test_first_boot_initializes_and_persists() {
        // Empty store = first boot
        let _kv = empty_kv();

        // let mut shadow = KvShadow::<SimpleConfig, _>::new_persistent(&kv);
        // let result = shadow.load().await.unwrap();

        // assert!(result.first_boot);       // First boot detected
        // assert!(!result.schema_changed);  // Not a schema change - it's first boot

        // // Verify hash was written
        // let hash_key = "device/__schema_hash__";
        // assert!(kv_has_key(&kv, hash_key).await);

        // // Verify fields were persisted with defaults
        // let value: u32 = kv_fetch_decode(&kv, "device/value").await;
        // assert_eq!(value, SimpleConfig::default().value);
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for SimpleConfig"]
    async fn test_first_boot_then_normal_boot() {
        let _kv = empty_kv();

        // First boot
        // {
        //     let mut shadow1 = KvShadow::<SimpleConfig, _>::new_persistent(&kv);
        //     let result1 = shadow1.load().await.unwrap();
        //     assert!(result1.first_boot);
        // }

        // Second boot - should be normal load
        // {
        //     let mut shadow2 = KvShadow::<SimpleConfig, _>::new_persistent(&kv);
        //     let result2 = shadow2.load().await.unwrap();
        //     assert!(!result2.first_boot);
        //     assert!(!result2.schema_changed);
        // }
    }

    // =========================================================================
    // Multiple Shadows Sharing Same KVStore Tests
    // =========================================================================

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for DeviceShadow and NetworkShadow"]
    async fn test_multiple_shadows_share_kvstore() {
        let _kv = empty_kv();

        // Multiple shadows can share the same KVStore via & references
        // This is the key benefit of interior mutability
        // let mut device = KvShadow::<DeviceShadow, _>::new_persistent(&kv);
        // let mut network = KvShadow::<NetworkShadow, _>::new_persistent(&kv);

        // Both initialize on first boot (different prefixes, same KVStore)
        // device.load().await.unwrap();
        // network.load().await.unwrap();

        // Modify via apply_and_save (Phase 7)
        // let delta = DeltaDeviceShadow { value: Some(42) };
        // device.apply_and_save(&delta).await.unwrap();

        // let delta = DeltaNetworkShadow { value: Some(99) };
        // network.apply_and_save(&delta).await.unwrap();

        // Reload fresh - still sharing the same KVStore
        // let mut device2 = KvShadow::<DeviceShadow, _>::new_persistent(&kv);
        // let mut network2 = KvShadow::<NetworkShadow, _>::new_persistent(&kv);

        // device2.load().await.unwrap();
        // network2.load().await.unwrap();

        // assert_eq!(device2.state.value, 42);
        // assert_eq!(network2.state.value, 99);
    }

    // =========================================================================
    // Non-Persisted Shadow (NoPersist) Tests
    // =========================================================================

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for SimpleConfig"]
    async fn test_in_memory_shadow_initializes_defaults() {
        // Non-persisted shadow using NoPersist KVStore
        // let mut shadow = KvShadow::<SimpleConfig, NoPersist>::new_in_memory();
        // let result = shadow.load().await.unwrap();

        // Always "first boot" since nothing is persisted
        // assert!(result.first_boot);
        // assert_eq!(shadow.state, SimpleConfig::default());
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for SimpleConfig and Phase 7 apply_and_save"]
    async fn test_in_memory_shadow_apply_and_save_updates_state() {
        // let mut shadow = KvShadow::<SimpleConfig, NoPersist>::new_in_memory();
        // shadow.load().await.unwrap();

        // let delta = DeltaSimpleConfig { value: Some(42) };
        // shadow.apply_and_save(&delta).await.unwrap();

        // State updated (but not persisted anywhere)
        // assert_eq!(shadow.state.value, 42);
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for SimpleConfig and Phase 7 apply_and_save"]
    async fn test_in_memory_shadow_reload_resets_to_defaults() {
        // let mut shadow = KvShadow::<SimpleConfig, NoPersist>::new_in_memory();
        // shadow.load().await.unwrap();

        // let delta = DeltaSimpleConfig { value: Some(42) };
        // shadow.apply_and_save(&delta).await.unwrap();

        // Create new shadow - state should be default (not persisted)
        // let mut shadow2 = KvShadow::<SimpleConfig, NoPersist>::new_in_memory();
        // shadow2.load().await.unwrap();

        // assert_eq!(shadow2.state.value, SimpleConfig::default().value);
    }

    // =========================================================================
    // Phase 5 Tests: Migration Logic
    // =========================================================================

    /*
    // Phase 5 test fixtures - these require Phase 8 derive macros
    // Example types for migration testing:

    // Config with a field that was migrated from an old key
    #[shadow_root(name = "device")]
    struct MigratedConfig {
        #[shadow_attr(migrate(from = "/old_timeout"))]
        timeout: u32,
    }

    // Config with multiple migration sources
    #[shadow_root(name = "device")]
    struct MultiSourceConfig {
        #[shadow_attr(migrate(from = "/old_name"), migrate(from = "/legacy_name"))]
        current_name: heapless::String<32>,
    }

    // Config with type conversion
    #[shadow_root(name = "device")]
    struct TypeMigratedConfig {
        #[shadow_attr(migrate(from = "/precision", convert = u8_to_u16))]
        precision: u16,
    }

    // Old config that doesn't know about the new field
    #[shadow_root(name = "device")]
    struct OldConfig {
        old_timeout: u32,
    }

    // Current config with a simple field
    #[shadow_root(name = "device")]
    struct CurrentConfig {
        timeout: u32,
    }

    // Config for milliseconds to seconds conversion test
    #[shadow_root(name = "device")]
    struct MsToSecsConfig {
        #[shadow_attr(migrate(from = "/timeout_ms", convert = ms_to_secs))]
        timeout_secs: u32,
    }

    // Config that fails conversion
    #[shadow_root(name = "device")]
    struct FailingConversionConfig {
        #[shadow_attr(migrate(from = "/value", convert = failing_convert))]
        value: u16,
    }

    // Wifi config for enum preservation tests
    #[shadow_root(name = "wifi")]
    struct WifiConfig {
        ip: IpSettings,
    }

    #[shadow_node]
    struct StaticConfig {
        address: [u8; 4],
        gateway: [u8; 4],
    }

    #[shadow_node]
    enum IpSettings {
        #[default]
        Dhcp,
        Static(StaticConfig),
    }
    */

    // =========================================================================
    // 5.1 Migration Source Resolution
    // =========================================================================

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for MigratedConfig"]
    async fn test_migration_prefers_primary_key_over_old() {
        // Both old and new keys exist - must use new key
        let _kv = setup_kv(&[
            ("device/timeout", &encode(5000u32)),
            ("device/old_timeout", &encode(9999u32)),
        ])
        .await;

        // let mut shadow = KvShadow::<MigratedConfig, _>::new_persistent(&kv);
        // shadow.load().await.unwrap();

        // assert_eq!(shadow.state.timeout, 5000); // NOT 9999
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for MultiSourceConfig"]
    async fn test_migration_multiple_sources_tried_in_order() {
        // Only second source exists
        let _kv = setup_kv(&[
            ("device/legacy_name", &encode("from_legacy")),
            // "old_name" doesn't exist
            // "current_name" doesn't exist
        ])
        .await;

        // let mut shadow = KvShadow::<MultiSourceConfig, _>::new_persistent(&kv);
        // shadow.load().await.unwrap();

        // assert_eq!(shadow.state.current_name.as_str(), "from_legacy");
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for TypeMigratedConfig"]
    async fn test_migration_type_conversion_when_new_type_fails_deserialize() {
        // Key exists but contains old type (u8), field expects new type (u16)
        // Should try conversion function as fallback
        let _kv = setup_kv(&[
            ("device/precision", &encode(42u8)), // old type
        ])
        .await;

        // let mut shadow = KvShadow::<TypeMigratedConfig, _>::new_persistent(&kv);
        // shadow.load().await.unwrap();

        // assert_eq!(shadow.state.precision, 42u16); // converted
    }

    // =========================================================================
    // 5.2 OTA-Safe Two-Phase Behavior
    // =========================================================================

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for MigratedConfig"]
    async fn test_load_writes_new_key_but_preserves_old() {
        let _kv = setup_kv(&[("device/old_timeout", &encode(5000u32))]).await;

        // let mut shadow = KvShadow::<MigratedConfig, _>::new_persistent(&kv.clone());
        // let result = shadow.load().await.unwrap();

        // assert_eq!(result.fields_migrated, 1);

        // // New key written
        // assert!(kv_has_key(&kv, "device/timeout").await);
        // // Old key STILL exists (for rollback)
        // assert!(kv_has_key(&kv, "device/old_timeout").await);
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for MigratedConfig"]
    async fn test_commit_removes_old_keys_only_after_explicit_call() {
        let _kv = setup_kv(&[("device/old_timeout", &encode(5000u32))]).await;

        // let mut shadow = KvShadow::<MigratedConfig, _>::new_persistent(&kv.clone());
        // shadow.load().await.unwrap();

        // // Old key still there
        // assert!(kv_has_key(&kv, "device/old_timeout").await);

        // // Now commit (after boot marked successful)
        // shadow.commit().await.unwrap();

        // // Old key gone
        // assert!(!kv_has_key(&kv, "device/old_timeout").await);
        // // New key still there
        // assert!(kv_has_key(&kv, "device/timeout").await);
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for OldConfig"]
    async fn test_rollback_scenario_old_firmware_reads_old_keys() {
        // Simulate: new firmware migrated, then rollback to old firmware
        let _kv = setup_kv(&[
            ("device/timeout", &encode(5000u32)), // new key (from migration)
            ("device/old_timeout", &encode(5000u32)), // old key (preserved)
        ])
        .await;

        // "Old firmware" only knows about old_timeout
        // let mut old_shadow = KvShadow::<OldConfig, _>::new_persistent(&kv);
        // old_shadow.load().await.unwrap();

        // assert_eq!(old_shadow.state.old_timeout, 5000); // Works!
    }

    // =========================================================================
    // 5.3 Type Conversion Edge Cases
    // =========================================================================

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for MsToSecsConfig"]
    async fn test_conversion_function_receives_correct_bytes() {
        // Verify the conversion wrapper correctly deserializes old type,
        // calls user function, and serializes new type
        let _kv = setup_kv(&[("device/timeout_ms", &encode(5000u32))]).await;

        // let mut shadow = KvShadow::<MsToSecsConfig, _>::new_persistent(&kv);
        // shadow.load().await.unwrap();

        // assert_eq!(shadow.state.timeout_secs, 5); // 5000ms -> 5s
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for FailingConversionConfig"]
    async fn test_conversion_failure_propagates_error() {
        // Conversion function can fail (e.g., value out of range)
        let _kv = setup_kv(&[
            ("device/value", &encode(u32::MAX)), // Too large for conversion
        ])
        .await;

        // let mut shadow = KvShadow::<FailingConversionConfig, _>::new_persistent(&kv);
        // let result = shadow.load().await;

        // assert!(result.is_err());
    }

    // =========================================================================
    // 5.4 Commit Orphan Detection
    // =========================================================================

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for CurrentConfig"]
    async fn test_commit_removes_truly_orphaned_keys() {
        // Simulate: old schema had "device/removed_field", new schema doesn't
        let _kv = setup_kv(&[
            ("device/timeout", &encode(5000u32)),      // current field
            ("device/removed_field", &encode(123u32)), // orphaned - not in schema
            ("device/also_removed", &encode(456u32)),  // orphaned - not in schema
        ])
        .await;

        // let mut shadow = KvShadow::<CurrentConfig, _>::new_persistent(&kv.clone());
        // shadow.load().await.unwrap();
        // shadow.commit().await.unwrap();

        // // Current field preserved
        // assert!(kv_has_key(&kv, "device/timeout").await);
        // // Orphaned keys removed
        // assert!(!kv_has_key(&kv, "device/removed_field").await);
        // assert!(!kv_has_key(&kv, "device/also_removed").await);
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for WifiConfig"]
    async fn test_commit_preserves_inactive_enum_variant_fields() {
        // Inactive variant fields are NOT orphans
        let _kv = setup_kv(&[
            ("wifi/ip/_variant", b"Dhcp"),
            ("wifi/ip/Static/address", &encode([192u8, 168, 1, 100])),
            ("wifi/ip/Static/gateway", &encode([192u8, 168, 1, 1])),
        ])
        .await;

        // let mut shadow = KvShadow::<WifiConfig, _>::new_persistent(&kv.clone());
        // shadow.load().await.unwrap();

        // // Current variant is Dhcp, but Static fields are valid keys
        // shadow.commit().await.unwrap();

        // // Static fields preserved (not orphans)
        // assert!(kv_has_key(&kv, "wifi/ip/Static/address").await);
        // assert!(kv_has_key(&kv, "wifi/ip/Static/gateway").await);
    }

    #[tokio::test]
    #[ignore = "Requires Phase 8 derive macro support for CurrentConfig"]
    async fn test_commit_does_not_affect_other_shadow_prefixes() {
        // GC for "device" should not touch "network" keys
        let _kv = setup_kv(&[
            ("device/timeout", &encode(5000u32)),
            ("network/some_key", &encode(123u32)),
        ])
        .await;

        // let mut shadow = KvShadow::<CurrentConfig, _>::new_persistent(&kv.clone());
        // shadow.load().await.unwrap();
        // shadow.commit().await.unwrap();

        // // Network keys untouched
        // assert!(kv_has_key(&kv, "network/some_key").await);
    }
}
