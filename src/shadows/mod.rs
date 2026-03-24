pub mod data_types;
pub mod error;
pub mod tag_scanner;
pub mod topics;

// Shadow storage modules
pub mod commit;
pub mod hash;
pub mod impls;
pub mod migration;
pub mod shadow;
pub mod store;

pub use rustot_derive;

// Re-export StateStore trait and implementations
pub use store::InMemory;
pub use store::StateStore;

// Re-export KVStore (feature-gated) - KVPersist is defined in this module
#[cfg(all(feature = "std", feature = "shadows_kv_persist"))]
pub use store::FileKVStore;
#[cfg(feature = "shadows_kv_persist")]
pub use store::KVStore;
#[cfg(feature = "shadows_kv_persist")]
pub use store::SequentialKVStore;

// Re-export Shadow
pub use shadow::Shadow;

// Re-export migration types
pub use migration::{LoadResult, MigrationError, MigrationSource};

// Re-export commit types
pub use commit::CommitStats;

/// Result of loading fields from KV storage.
///
/// Returned by `load_from_kv()` to indicate how many fields were loaded,
/// defaulted, or migrated.
#[cfg(feature = "shadows_kv_persist")]
#[derive(Debug, Default, Clone)]
pub struct LoadFieldResult {
    /// Number of fields loaded from KV storage.
    pub loaded: usize,
    /// Number of fields that used default values (not in KV).
    pub defaulted: usize,
    /// Number of fields that were migrated from old keys.
    pub migrated: usize,
}

#[cfg(feature = "shadows_kv_persist")]
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

// Re-export tag scanner for adjacently-tagged enum deserialization
pub use tag_scanner::{FieldScanner, ScanError, TaggedJsonScan};

// The impl_opaque! macro is exported at crate root via #[macro_export]

// Re-export new error types
pub use error::KvError;

pub use data_types::Patch;
pub use error::{Error, ParseError};
use serde::Serialize;

use core::future::Future;

// =============================================================================
// DeltaContent — extends Option<T> for desired cleanup
// =============================================================================

/// Content wrapper for adjacently-tagged enum delta config fields.
///
/// Extends `Option<T>` with a `NullFields` variant that serializes field names
/// as null values. This is used to clean up stale desired fields in AWS IoT
/// Shadow when an adjacently-tagged enum switches variants.
///
/// Without cleanup, stale desired fields persist after variant switches and
/// cause infinite delta loops (AWS computes delta from desired keys not in
/// reported).
#[derive(Clone)]
pub enum DeltaContent<T> {
    /// No content — field skipped by `skip_serializing_if`.
    Absent,
    /// Actual delta value — delegates to T's Serialize.
    Value(T),
    /// Null cleanup — serializes as `{field1: null, field2: null, ...}`.
    ///
    /// Each inner slice is a `FIELD_NAMES` from a `ReportedUnionFields` impl,
    /// allowing multiple variants' fields to be combined.
    NullFields(&'static [&'static [&'static str]]),
}

impl<T> Default for DeltaContent<T> {
    fn default() -> Self {
        DeltaContent::Absent
    }
}

impl<T> From<Option<T>> for DeltaContent<T> {
    fn from(opt: Option<T>) -> Self {
        match opt {
            Some(v) => DeltaContent::Value(v),
            None => DeltaContent::Absent,
        }
    }
}

impl<T> DeltaContent<T> {
    /// Returns `true` if this is `Absent`.
    pub fn is_absent(&self) -> bool {
        matches!(self, DeltaContent::Absent)
    }

    /// Returns a reference to the inner value if `Value`.
    pub fn as_ref_value(&self) -> Option<&T> {
        match self {
            DeltaContent::Value(v) => Some(v),
            _ => None,
        }
    }
}

impl<T: Serialize> Serialize for DeltaContent<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            DeltaContent::Absent => serializer.serialize_none(),
            DeltaContent::Value(v) => v.serialize(serializer),
            DeltaContent::NullFields(field_slices) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(None)?;
                for fields in *field_slices {
                    serialize_null_fields(*fields, &mut map)?;
                }
                map.end()
            }
        }
    }
}

/// Serde helper for `#[serde(skip_serializing_if = "...")]` on `DeltaContent` fields.
pub fn delta_content_is_absent<T>(dc: &DeltaContent<T>) -> bool {
    dc.is_absent()
}

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

// =============================================================================
// Variant Resolution for Delta Parsing
// =============================================================================

/// Resolves variant names during delta parsing.
///
/// When parsing an adjacently-tagged enum delta where the tag field is missing
/// from the JSON, the resolver provides the current variant name as fallback.
/// This allows content-only updates to be applied to the existing variant.
///
/// ## Implementations
///
/// - **InMemory**: Uses `ShadowNode::variant_at_path()` on current state
/// - **KV Store**: Fetches `/_variant` key from storage
///
/// ## Example
///
/// ```ignore
/// // JSON delta without tag: {"config": {"timeout": 30}}
/// // Resolver provides current variant: "sio"
/// // Result: Update SIO variant's config.timeout to 30
/// ```
pub trait VariantResolver {
    /// Resolve the current variant name at the given path.
    ///
    /// Returns `None` if no variant is stored (first initialization).
    fn resolve(&self, path: &str) -> impl Future<Output = Option<heapless::String<32>>>;
}

/// Null resolver that never provides fallback variants.
///
/// Used when no fallback is available (e.g., fresh state with no history).
pub struct NullResolver;

impl VariantResolver for NullResolver {
    fn resolve(&self, _path: &str) -> impl Future<Output = Option<heapless::String<32>>> {
        core::future::ready(None)
    }
}

/// Core shadow type trait - clean, no KV awareness.
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
/// Only the top-level `ShadowRoot::SCHEMA_HASH` is actually stored.
pub trait ShadowNode: Default + Clone + Sized {
    /// Delta type for partial updates (fields wrapped in Option).
    ///
    /// ## Trait Bounds
    ///
    /// - `Default`: Create empty delta for internal use and builder defaults
    /// - `Serialize`: Send delta to cloud via MQTT (device→cloud desired updates)
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
    /// the Delta is a proper enum representing the variant change:
    /// ```ignore
    /// #[serde(tag = "mode", content = "config")]
    /// enum PortMode { Inactive, Sio(SioConfig) }
    /// // Delta - enum shape:
    /// enum DeltaPortMode {
    ///     Inactive,
    ///     Sio(DeltaSioConfig),
    /// }
    /// ```
    type Delta: Default + Serialize;

    /// Reported type for serialization to cloud.
    ///
    /// Used for device→cloud acknowledgment after applying deltas.
    /// Fields marked `report_only` are excluded from the state struct and only
    /// appear in this Reported type. They are `None` in partial reported.
    ///
    /// ## Trait Bound: `ReportedUnionFields`
    ///
    /// All `Reported` types must implement `ReportedUnionFields`, which provides:
    /// - `Serialize` (as a supertrait)
    /// - `serialize_into_map()` for flat union serialization
    type Reported: ReportedUnionFields;

    /// Compile-time hash of this type's schema structure.
    ///
    /// For nested `ShadowNode` fields, this includes their `SCHEMA_HASH`.
    /// For primitive fields, this includes the field name and type name.
    /// For fields with migrations, this includes the migration source keys.
    ///
    /// **Note**: This hash is used for composition. Only the top-level
    /// `ShadowRoot` hash is stored for change detection.
    const SCHEMA_HASH: u64;

    // =========================================================================
    // Core Methods
    // =========================================================================

    /// Parse JSON delta using resolver for missing variants.
    ///
    /// Uses `FieldScanner` to extract fields and recursively calls `parse_delta`
    /// on nested types. For adjacently-tagged enums, the resolver provides
    /// variant fallback when the tag field is missing.
    ///
    /// The `path` parameter is the current path in the shadow tree (e.g., "config/port").
    /// It's used to construct full paths when calling the resolver for nested enums.
    fn parse_delta<R: VariantResolver>(
        json: &[u8],
        path: &str,
        resolver: &R,
    ) -> impl Future<Output = Result<Self::Delta, ParseError>>;

    /// Apply delta to state (pure mutation, no storage, no serialization).
    ///
    /// This method updates self with delta values. It does NOT persist
    /// to storage - that's the responsibility of the `StateStore`.
    ///
    /// ## Why `&Self::Delta` (by reference)?
    ///
    /// Taking delta by reference (not by value) allows callers to retain the
    /// delta after applying, which is needed for patterns like `wait_delta()`
    /// that return the delta to the caller.
    fn apply_delta(&mut self, delta: &Self::Delta);

    /// Get the variant name at a given path.
    ///
    /// Returns `Some(variant_name)` if the path points to an adjacently-tagged enum.
    /// Returns `None` for other types or if path doesn't match.
    ///
    /// This is used by the InMemory resolver to provide variant fallback
    /// when delta JSON is missing the tag field.
    fn variant_at_path(&self, _path: &str) -> Option<heapless::String<32>> {
        None
    }

    /// Convert to full reported representation (all fields set).
    ///
    /// This is used by the generated `From<State> for Reported` impl
    /// to convert the entire state into a reported type.
    #[allow(clippy::wrong_self_convention)]
    fn into_reported(&self) -> Self::Reported;

    /// Convert to reported representation containing only fields present in delta.
    ///
    /// Used for efficient acknowledgment - reports only changed fields.
    /// AWS IoT Shadow merges partial updates, so unchanged fields keep
    /// their cloud values.
    #[allow(clippy::wrong_self_convention)]
    fn into_partial_reported(&self, delta: &Self::Delta) -> Self::Reported;

    /// Generate a cleanup delta to null stale desired fields after a variant switch.
    ///
    /// For adjacently-tagged enums: when the variant changes, the old variant's
    /// content fields remain in `desired` (AWS only removes null keys from
    /// `reported`). This method returns a delta with `NullFields` for the
    /// stale content, which is sent as `desired` in the same shadow update.
    ///
    /// For structs: delegates to nested fields and aggregates their cleanups.
    ///
    /// Returns `None` if no cleanup is needed (no variant switch, or leaf type).
    fn desired_cleanup(&self, delta: &Self::Delta) -> Option<Self::Delta> {
        let _ = delta;
        None
    }
}

// =============================================================================
// KV Persistence Trait (feature-gated)
// =============================================================================

/// Field-level persistence operations for KV storage.
///
/// This trait is only available with the `shadows_kv_persist` feature and
/// provides methods for field-level KV persistence with migration support.
///
/// Types implementing `KVPersist` can be stored in field-level KV stores
/// like `SequentialKVStore` and `FileKVStore`.
#[cfg(feature = "shadows_kv_persist")]
pub trait KVPersist: ShadowNode {
    // =========================================================================
    // KV Storage Constants
    // =========================================================================

    /// Maximum key length needed for this type's fields (excluding prefix).
    ///
    /// Computed at compile time per-field during codegen.
    /// Includes the longest field path including nested types.
    const MAX_KEY_LEN: usize;

    /// Buffer type for serializing/deserializing values.
    ///
    /// Each type provides its own buffer sized for its worst-case serialized value.
    /// This seals `[(); EXPR]:` bounds inside the library — user crates only see
    /// the associated type, never the const expression.
    ///
    /// - no-std leaf types: `[u8; POSTCARD_MAX_SIZE]`
    /// - std types: `[u8; 0]` (unused — std uses `to_allocvec`/`fetch_to_vec`)
    /// - structs: `[u8; max(leaf_field_sizes)]` (nested fields bring their own)
    type ValueBuf: AsMut<[u8]>;

    /// Create a zeroed value buffer for serialization/deserialization.
    fn zero_value_buf() -> Self::ValueBuf;

    // =========================================================================
    // Migration Support
    // =========================================================================

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
    /// ## Return Value
    ///
    /// - `true`: A custom default was applied to the field at `field_path`
    /// - `false`: No custom default defined; caller should use `Default::default()`
    fn apply_field_default(&mut self, field_path: &str) -> bool;

    // =========================================================================
    // KV Persistence Methods
    // =========================================================================

    /// Load this type's state from KV storage.
    ///
    /// `key_buf` is a mutable key buffer shared across the entire call chain.
    /// Each level appends its field segment, does the KV operation, then truncates back.
    /// This avoids allocating a new `String<KEY_LEN>` per field in the async state machine.
    ///
    /// KEY_LEN is propagated from root to ensure buffer size matches full key paths.
    fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K,
    ) -> impl core::future::Future<Output = Result<LoadFieldResult, KvError<K::Error>>>;

    /// Load this type's state from KV storage with migration support.
    ///
    /// Same as `load_from_kv()` but tries migration sources when primary key is missing.
    fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K,
    ) -> impl core::future::Future<Output = Result<LoadFieldResult, KvError<K::Error>>>;

    /// Persist this type's entire state to KV storage (all fields).
    ///
    /// `key_buf` is a mutable key buffer shared across the entire call chain.
    fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K,
    ) -> impl core::future::Future<Output = Result<(), KvError<K::Error>>>;

    /// Persist only delta fields to KV storage (efficient partial update).
    ///
    /// - Structs: NO LOAD NEEDED - direct write of changed fields
    /// - Regular enums: NO LOAD NEEDED - unconditionally write `_variant`
    /// - Adjacently-tagged enums: NO LOAD NEEDED - only write `_variant` if `mode.is_some()`
    fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        key_buf: &mut heapless::String<KEY_LEN>,
    ) -> impl core::future::Future<Output = Result<(), KvError<K::Error>>>;

    /// Number of statically-known leaf fields in this type.
    ///
    /// Used by `load()` for the `fields_defaulted` statistic on first boot.
    /// For structs: sum of leaf fields + sum of nested FIELD_COUNTs.
    /// For enums: 1 (variant key) + sum of all variant FIELD_COUNTs.
    /// For leaf types: 1.
    const FIELD_COUNT: usize;

    /// Check if a relative key path is a valid field for this type.
    ///
    /// `rel_key` is relative to this type's prefix (e.g., `"/timeout"`, `"/config/ssid"`).
    /// Used by `commit()` for garbage collection instead of materializing key sets.
    ///
    /// Enums check ALL variant fields (not just active), since inactive variant
    /// fields are valid keys that should not be removed.
    fn is_valid_key(rel_key: &str) -> bool;

    /// Check if a relative key path belongs to a dynamic collection of this type.
    ///
    /// Map types return true if the key belongs to their dynamic key/entry space.
    /// Returns false by default (no dynamic children).
    fn is_valid_prefix(rel_key: &str) -> bool {
        let _ = rel_key;
        false
    }
}

/// Trait for types that can serve as map keys in shadow collections.
///
/// Map keys must be displayable (for KV key construction), serializable,
/// and have a known maximum display length for compile-time buffer sizing.
pub trait MapKey:
    Clone + Eq + core::fmt::Display + serde::Serialize + serde::de::DeserializeOwned
{
    /// Maximum length of `Display` output for this key type.
    ///
    /// Used to compute `MAX_KEY_LEN` for map-based `KVPersist` implementations.
    const MAX_KEY_DISPLAY_LEN: usize;

    /// Buffer type for serializing a single key via postcard.
    ///
    /// Seals `[(); EXPR]:` bounds inside the library — user crates only see
    /// the associated type. Sized for worst-case postcard serialization:
    /// - `u8` → `[u8; 2]` (varint)
    /// - `u16` → `[u8; 3]`
    /// - `u32` → `[u8; 5]`
    /// - `heapless::String<N>` → `[u8; N + 5]`
    /// - `std::String` → `[u8; 0]` (std uses allocating path)
    type KeyBuf: AsMut<[u8]> + AsRef<[u8]>;

    /// Create a zeroed key buffer for serialization.
    fn zero_key_buf() -> Self::KeyBuf;
}

impl MapKey for u8 {
    const MAX_KEY_DISPLAY_LEN: usize = 3; // "255"
    type KeyBuf = [u8; 2]; // postcard varint max
    fn zero_key_buf() -> Self::KeyBuf {
        [0u8; 2]
    }
}

impl MapKey for u16 {
    const MAX_KEY_DISPLAY_LEN: usize = 5; // "65535"
    type KeyBuf = [u8; 3]; // postcard varint max
    fn zero_key_buf() -> Self::KeyBuf {
        [0u8; 3]
    }
}

impl MapKey for u32 {
    const MAX_KEY_DISPLAY_LEN: usize = 10; // "4294967295"
    type KeyBuf = [u8; 5]; // postcard varint max
    fn zero_key_buf() -> Self::KeyBuf {
        [0u8; 5]
    }
}

#[allow(incomplete_features)]
impl<const N: usize> MapKey for heapless::String<N>
where
    [(); N + 5]:,
{
    const MAX_KEY_DISPLAY_LEN: usize = N;
    type KeyBuf = [u8; N + 5]; // postcard string: varint len + bytes
    fn zero_key_buf() -> Self::KeyBuf {
        [0u8; N + 5]
    }
}

#[cfg(feature = "std")]
impl MapKey for std::string::String {
    const MAX_KEY_DISPLAY_LEN: usize = 64;
    type KeyBuf = [u8; 0]; // std uses allocating path
    fn zero_key_buf() -> Self::KeyBuf {
        []
    }
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
