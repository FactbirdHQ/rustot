pub mod data_types;
pub mod error;
pub mod topics;

// KV-based shadow storage modules
pub mod commit;
pub mod hash;
pub mod kv_shadow;
pub mod kv_store;
pub mod migration;
pub mod opaque;
pub mod scan;

pub use rustot_derive;

// Re-export KVStore trait and implementations
#[cfg(feature = "std")]
pub use kv_store::FileKVStore;
pub use kv_store::KVStore;
pub use kv_store::NoPersist;
pub use kv_store::SequentialKVStore;

// Re-export KvShadow
pub use kv_shadow::KvShadow;

// Re-export migration types
pub use migration::{LoadResult, MigrationError, MigrationSource};

// Re-export commit types
pub use commit::CommitStats;

/// Result of loading fields from KV storage.
///
/// Returned by `load_from_kv()` to indicate how many fields were loaded,
/// defaulted, or migrated.
#[derive(Debug, Default, Clone)]
pub struct LoadFieldResult {
    /// Number of fields loaded from KV storage.
    pub loaded: usize,
    /// Number of fields that used default values (not in KV).
    pub defaulted: usize,
    /// Number of fields that were migrated from old keys.
    pub migrated: usize,
}

impl LoadFieldResult {
    /// Merge another result into this one.
    pub fn merge(&mut self, other: LoadFieldResult) {
        self.loaded += other.loaded;
        self.defaulted += other.defaulted;
        self.migrated += other.migrated;
    }
}

// Re-export hash functions (for derive macro use)
pub use hash::{fnv1a_byte, fnv1a_bytes, fnv1a_hash, fnv1a_u64, FNV1A_INIT};

// The impl_opaque! macro is exported at crate root via #[macro_export]

// Re-export new error types
pub use error::{KvError, ScanError};

// Re-export JSON scanning utilities
pub use scan::TaggedJsonScan;

pub use data_types::Patch;
pub use error::Error;
use serde::Serialize;

// =============================================================================
// KV-based Shadow Storage Traits
// =============================================================================

/// Trait for types that can serialize their fields into an existing map.
///
/// Used for flat union serialization of adjacently-tagged enum Reported types.
/// When reporting an adjacently-tagged enum to AWS IoT Shadow, we need to serialize
/// all possible variant fields - active variant fields with their values, and
/// inactive variant fields as `null` to clear them from the shadow.
///
/// ## Why Universal Implementation?
///
/// All `#[shadow_node]` types generate `ReportedUnionFields` for their Reported type,
/// even if they're not used inside an adjacently-tagged enum. This is because at
/// macro expansion time, we don't know if a type will later be used as an inner
/// type of an adjacently-tagged enum.
///
/// ## Supertrait: `Serialize`
///
/// `ReportedUnionFields` requires `Serialize` as a supertrait. This means the
/// `ShadowNode::Reported` bound of `ReportedUnionFields` implies `Serialize`.
pub trait ReportedUnionFields: Serialize {
    /// Names of all fields this type contributes to the flat union.
    ///
    /// Used to null out inactive variant fields during serialization.
    const FIELD_NAMES: &'static [&'static str];

    /// Serialize this type's fields into an existing map.
    ///
    /// Unlike the standard `Serialize` impl which creates a new map, this method
    /// adds fields to an existing `SerializeMap`. This enables flat union serialization
    /// where multiple types' fields are combined into a single JSON object.
    fn serialize_into_map<S: serde::ser::SerializeMap>(&self, map: &mut S) -> Result<(), S::Error>;
}

/// Helper to serialize null values for inactive variant fields.
///
/// When serializing an adjacently-tagged enum's Reported type, inactive variants'
/// fields must be serialized as `null` to clear them from AWS IoT Shadow.
///
/// # Example
///
/// ```ignore
/// // When PortMode is Analog, Digital fields are nulled:
/// serialize_null_fields(ReportedDigitalConfig::FIELD_NAMES, &mut map)?;
/// ```
pub fn serialize_null_fields<S: serde::ser::SerializeMap>(
    field_names: &[&str],
    map: &mut S,
) -> Result<(), S::Error> {
    for name in field_names {
        map.serialize_entry(*name, &None::<()>)?;
    }
    Ok(())
}

/// Trait for types that can be patched from a delta and persisted to KV storage.
///
/// Implemented by both top-level shadow structs (`#[shadow_root]`) and nested
/// patchable types (`#[shadow_node]`).
///
/// ## Naming Convention
///
/// - `ShadowNode`: Any type in the shadow tree (structs, enums, nested types)
/// - `ShadowRoot`: Top-level shadow with a name (prefix for KV keys)
///
/// ## SCHEMA_HASH Composition
///
/// Each `ShadowNode` type has a compile-time `SCHEMA_HASH` that captures its
/// schema structure. For nested types, the hash is composed from child hashes:
///
/// ```text
/// Parent::SCHEMA_HASH = hash(
///     field1_name + <Field1Type as ShadowNode>::SCHEMA_HASH +
///     field2_name + hash(primitive_type_name) +
///     ...
/// )
/// ```
///
/// This ensures that changes to nested types propagate to parent hashes.
/// Only the top-level `ShadowRoot::SCHEMA_HASH` is actually stored in KV.
pub trait ShadowNode: Default + Clone + Sized {
    /// Delta type for partial updates (fields wrapped in Option).
    ///
    /// Used by the generated `apply_and_persist()` method to update state
    /// and persist changed fields in a single pass.
    ///
    /// ## Trait Bounds
    ///
    /// - `Default`: Create empty delta for `update_desired()` pattern
    /// - `Serialize`: Send delta to cloud via MQTT (device→cloud desired updates)
    /// - `DeserializeOwned`: Receive delta from cloud via MQTT (cloud→device)
    ///
    /// ## Delta Type Shapes
    ///
    /// For **structs** and **simple enums**, the Delta mirrors the type structure
    /// with fields/variants wrapped in `Option`:
    /// ```ignore
    /// struct Config { timeout: u32 }
    /// // Delta:
    /// struct DeltaConfig { timeout: Option<u32> }
    ///
    /// enum Mode { Off, On(Config) }
    /// // Delta:
    /// enum DeltaMode { Off, On(DeltaConfig) }
    /// ```
    ///
    /// For **adjacently-tagged enums** (`#[serde(tag="...", content="...")]`),
    /// the Delta is a struct with optional tag and content fields to support
    /// partial updates:
    /// ```ignore
    /// #[serde(tag = "mode", content = "config")]
    /// enum PortMode { Inactive, Sio(SioConfig) }
    /// // Delta - struct shape for partial updates:
    /// struct DeltaPortMode {
    ///     mode: Option<PortModeVariant>,
    ///     config: Option<DeltaPortModeConfig>,
    /// }
    /// ```
    /// This allows `{"config": {"polarity": "npn"}}` to update config without
    /// changing the variant.
    type Delta: Default + Serialize + serde::de::DeserializeOwned;

    /// Reported type for serialization to cloud.
    ///
    /// Used for device→cloud acknowledgment after applying deltas.
    /// Fields marked `report_only` are `None` in the Reported type.
    ///
    /// ## Trait Bound: `ReportedUnionFields`
    ///
    /// All `Reported` types must implement `ReportedUnionFields`, which provides:
    /// - `Serialize` (as a supertrait)
    /// - `serialize_into_map()` for flat union serialization
    ///
    /// **Why universal?** At macro expansion time, we don't know if a type will be
    /// used as an inner type of an adjacently-tagged enum. So all `#[shadow_node]`
    /// types generate `ReportedUnionFields` defensively.
    ///
    /// ## Generation Requirements
    ///
    /// All `Option` fields in the generated Reported type must have:
    /// ```ignore
    /// #[serde(skip_serializing_if = "Option::is_none")]
    /// ```
    /// This ensures `None` fields are omitted from JSON rather than serialized as `null`.
    type Reported: ReportedUnionFields;

    // =========================================================================
    // KV Storage Constants
    // =========================================================================

    /// Maximum nesting depth for this type's field tree.
    ///
    /// Computed at compile time per-field during codegen.
    /// For structs: 1 + max(nested field depths)
    /// For enums: 1 + max(variant inner depths)
    const MAX_DEPTH: usize;

    /// Maximum key length needed for this type's fields (excluding prefix).
    ///
    /// Computed at compile time per-field during codegen.
    /// Includes the longest field path including nested types.
    const MAX_KEY_LEN: usize;

    /// Maximum serialized value size for any field.
    ///
    /// Computed using `postcard::experimental::max_size::MaxSize::POSTCARD_MAX_SIZE`
    /// for leaf types. Opaque field types must implement `MaxSize`.
    const MAX_VALUE_LEN: usize;

    /// Compile-time hash of this type's schema structure.
    ///
    /// For nested `ShadowNode` fields, this includes their `SCHEMA_HASH`.
    /// For primitive fields, this includes the field name and type name.
    /// For fields with migrations, this includes the migration source keys.
    ///
    /// **Note**: This hash is used for composition. Only the top-level
    /// `ShadowRoot` hash is stored in KV for change detection.
    const SCHEMA_HASH: u64;

    // =========================================================================
    // Core Methods
    // =========================================================================

    /// Apply delta to state AND persist changed fields to KV in one pass.
    ///
    /// This method updates self with delta values AND persists those values
    /// to KV storage in a single traversal.
    ///
    /// ## Why `&Self::Delta` (by reference)?
    ///
    /// Taking delta by reference (not by value) allows callers to retain the
    /// delta after applying, which is needed for patterns like `wait_delta()`
    /// that return the delta to the caller:
    ///
    /// ```ignore
    /// if let Some(ref delta) = received_delta {
    ///     shadow.apply_and_persist(delta, prefix, kv, &mut buf).await?;
    ///     // delta still available to return to caller
    /// }
    /// Ok((state, received_delta))
    /// ```
    ///
    /// This eliminates the need for `Clone` on Delta.
    ///
    /// ## Why `&K` (shared reference for KVStore)?
    ///
    /// KVStore uses interior mutability (`Mutex`), so methods take `&self`.
    /// This allows multiple shadows to share a single KVStore via `&'a K`
    /// references without requiring `alloc`.
    ///
    /// ## Implementation Strategy
    ///
    /// **For structs:** Uses per-field generated code. Each field in the delta is
    /// checked for `Some(value)`, and if present, both the local state and KV storage
    /// are updated:
    ///
    /// ```ignore
    /// if let Some(ref timeout) = delta.timeout {
    ///     self.timeout = *timeout;
    ///     let bytes = postcard::to_slice(&self.timeout, buf)?;
    ///     kv.store(&make_key(prefix, "/timeout"), bytes).await?;
    /// }
    /// if let Some(ref config) = delta.config {
    ///     // Nested ShadowNode: delegate to inner apply_and_persist
    ///     self.config.apply_and_persist(config, &make_key(prefix, "/config"), kv, buf).await?;
    /// }
    /// ```
    ///
    /// **For enums:** Uses custom generated code for variant switching, then
    /// delegates to inner type's `apply_and_persist`. Matching on `&Delta`
    /// yields references to inner data:
    ///
    /// ```ignore
    /// match delta {  // delta: &DeltaIpSettings
    ///     DeltaIpSettings::Static(delta_inner) => {  // delta_inner: &DeltaStaticConfig
    ///         if !matches!(self, IpSettings::Static(_)) {
    ///             *self = IpSettings::Static(StaticConfig::default());
    ///             kv.store(&make_key(prefix, "/_variant"), b"Static").await?;
    ///         }
    ///         if let IpSettings::Static(ref mut inner) = self {
    ///             inner.apply_and_persist(delta_inner, &make_key(prefix, "/Static"), kv, buf).await?;
    ///         }
    ///     }
    ///     // ... other variants
    /// }
    /// ```
    ///
    /// For adjacently-tagged enums (`#[serde(tag = "mode", content = "config")]`),
    /// the JSON `mode`/`config` structure is translated to KV `/_variant` + paths.
    fn apply_and_persist<K: KVStore>(
        &mut self,
        delta: &Self::Delta,
        prefix: &str,
        kv: &K,
        buf: &mut [u8],
    ) -> impl core::future::Future<Output = Result<(), KvError<K::Error>>>;

    /// Convert this state into its reported representation.
    fn into_reported(self) -> Self::Reported;

    /// Get migration sources for a field path.
    ///
    /// Returns empty slice if no migrations defined for this field.
    fn migration_sources(field_path: &str) -> &'static [MigrationSource];

    /// Get all migration source keys for GC safety.
    ///
    /// Returns an iterator over all migration source keys that should be
    /// preserved during garbage collection. This includes old keys that
    /// may still contain data from previous schema versions.
    ///
    /// Used by `commit()` to build the complete set of valid keys.
    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    /// Apply a custom default value to a field if one is defined.
    ///
    /// This is used during loading when a field has no stored value and no
    /// migration source. It allows setting non-trivial defaults for primitive
    /// types without needing newtype wrappers.
    ///
    /// ## Why This Exists
    ///
    /// Rust's `Default` trait always returns the type's inherent default
    /// (e.g., `0` for `u32`, `false` for `bool`). But shadow fields often
    /// need different defaults:
    ///
    /// ```ignore
    /// #[shadow_node]
    /// struct Config {
    ///     #[shadow_attr(default = 5000)]
    ///     timeout_ms: u32,  // Default::default() would give 0, but we want 5000
    ///
    ///     #[shadow_attr(default = true)]
    ///     enabled: bool,    // Default::default() would give false
    /// }
    /// ```
    ///
    /// Without `apply_field_default()`, users would need newtype wrappers:
    /// ```ignore
    /// struct Timeout(u32);
    /// impl Default for Timeout {
    ///     fn default() -> Self { Timeout(5000) }
    /// }
    /// ```
    ///
    /// ## Return Value
    ///
    /// - `true`: A custom default was applied to the field at `field_path`
    /// - `false`: No custom default defined; caller should use `Default::default()`
    fn apply_field_default(&mut self, field_path: &str) -> bool;

    // =========================================================================
    // KV Persistence Methods (generated by derive macro)
    // =========================================================================

    /// Load this type's state from KV storage.
    ///
    /// For enums: reads `_variant` key first, constructs the variant, then loads inner fields.
    /// For structs: recursively calls `load_from_kv()` on each field.
    ///
    /// KEY_LEN is propagated from root to ensure buffer size matches full key paths.
    ///
    /// ## Implementation
    ///
    /// Generated per-field by the derive macro:
    /// - **Leaf fields**: Fetch from KV, deserialize via postcard, apply default if missing
    /// - **Nested ShadowNode**: Delegate to inner type's `load_from_kv()`
    /// - **Enums**: Read `_variant` key, construct variant, load inner fields
    fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
        buf: &mut [u8],
    ) -> impl core::future::Future<Output = Result<LoadFieldResult, KvError<K::Error>>>;

    /// Load this type's state from KV storage with migration support.
    ///
    /// Same as `load_from_kv()` but tries migration sources when primary key is missing.
    fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
        buf: &mut [u8],
    ) -> impl core::future::Future<Output = Result<LoadFieldResult, KvError<K::Error>>>;

    /// Persist this type's state to KV storage.
    ///
    /// For enums: writes `_variant` key, then inner fields.
    /// For structs: recursively calls `persist_to_kv()` on each field.
    ///
    /// KEY_LEN is propagated from root to ensure buffer size matches full key paths.
    ///
    /// ## Implementation
    ///
    /// Generated per-field by the derive macro:
    /// - **Leaf fields**: Serialize via postcard, store to KV
    /// - **Nested ShadowNode**: Delegate to inner type's `persist_to_kv()`
    /// - **Enums**: Write `_variant` key as UTF-8, persist active variant's fields
    fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
        buf: &mut [u8],
    ) -> impl core::future::Future<Output = Result<(), KvError<K::Error>>>;

    /// Collect all valid key paths for this type.
    ///
    /// Used by `commit()` for garbage collection. Enums report ALL variant fields
    /// (not just active), since inactive variant fields are valid keys that should
    /// not be removed.
    ///
    /// ## Implementation
    ///
    /// Generated per-field by the derive macro:
    /// - **Leaf fields**: Add `prefix + /field_name` to set
    /// - **Nested ShadowNode**: Delegate to inner type's `collect_valid_keys()`
    /// - **Enums**: Add `_variant` key, then collect from ALL variants
    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str));
}

/// Trait for top-level shadow state types.
///
/// This is only implemented by types marked with `#[shadow_root]`, not `#[shadow_node]`.
/// It provides the shadow name and is the type whose `SCHEMA_HASH` gets stored.
///
/// ## Example
///
/// ```ignore
/// // Top-level shadow - implements both ShadowRoot and ShadowNode
/// #[shadow_root(name = "device")]
/// struct DeviceShadow {
///     config: Config,      // nested ShadowNode
///     version: u32,        // primitive
/// }
///
/// // Nested type - implements only ShadowNode
/// #[shadow_node]
/// struct Config {
///     timeout: u32,
///     retries: u8,
/// }
/// ```
///
/// Only `DeviceShadow::SCHEMA_HASH` is stored in KV at `"device/__schema_hash__"`.
pub trait ShadowRoot: ShadowNode {
    /// The shadow name used as KV key prefix (e.g., "device").
    ///
    /// Returns `None` for classic/unnamed shadows, which use "classic" as prefix.
    const NAME: Option<&'static str>;

    /// The AWS IoT topic prefix (default: "$aws").
    ///
    /// This is used for MQTT topic formatting in cloud communication.
    const PREFIX: &'static str = "$aws";

    /// Maximum payload size for shadow updates (default: 512 bytes).
    ///
    /// This should be set based on the maximum size of your shadow state
    /// when serialized to JSON.
    const MAX_PAYLOAD_SIZE: usize = 512;

    // Note: SCHEMA_HASH comes from ShadowNode, but only ShadowRoot's hash
    // is stored in KV at `{prefix}/__schema_hash__`
}
