//! KV-based shadow persistence
//!
//! This module provides the `KvShadow` struct for shadow state persistence
//! using the `KVStore` abstraction. It handles:
//! - First boot initialization with defaults
//! - Normal boot loading from KV storage
//! - Schema change detection for migrations

use crate::shadows::{
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
    /// See Phase 5 for full migration logic.
    async fn load_fields_with_migration(
        &mut self,
        prefix: &str,
    ) -> Result<LoadResult, KvError<K::Error>> {
        // Basic implementation - Phase 5 adds full migration support
        let mut result = self.load_fields(prefix).await?;
        result.schema_changed = true;
        Ok(result)
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
}
