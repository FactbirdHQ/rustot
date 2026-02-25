//! ShadowNode implementations for heapless container types.
//!
//! - `heapless::String<N>` — opaque leaf type
//! - `heapless::Vec<T, N>` — opaque leaf type
//! - `heapless::LinearMap<K, V, N>` — map collection with per-entry Patch deltas
//!
//! All implementations are strictly no_std / no_alloc.

use crate::shadows::{fnv1a_hash, ParseError, ReportedUnionFields, ShadowNode, VariantResolver};
use serde::ser::SerializeMap;

#[cfg(feature = "shadows_kv_persist")]
use crate::shadows::{KVPersist, KVStore, KvError, LoadFieldResult, MapKey, MigrationSource};
#[cfg(feature = "shadows_kv_persist")]
use postcard::experimental::max_size::MaxSize;
#[cfg(feature = "shadows_kv_persist")]
use serde::{de::DeserializeOwned, Serialize};

use crate::shadows::data_types::Patch;

// =============================================================================
// heapless::String<N>
// =============================================================================

impl<const N: usize> ShadowNode for heapless::String<N> {
    type Delta = heapless::String<N>;
    type Reported = heapless::String<N>;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"heapless::String");

    async fn parse_delta<R: VariantResolver>(
        json: &[u8],
        _path: &str,
        _resolver: &R,
    ) -> Result<Self::Delta, ParseError> {
        serde_json_core::from_slice(json)
            .map(|(v, _)| v)
            .map_err(|_| ParseError::Deserialize)
    }

    fn apply_delta(&mut self, delta: &Self::Delta) {
        *self = delta.clone();
    }

    fn into_reported(&self) -> Self::Reported {
        self.clone()
    }

    fn into_partial_reported(&self, _delta: &Self::Delta) -> Self::Reported {
        self.clone()
    }
}

impl<const N: usize> ReportedUnionFields for heapless::String<N> {
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

#[cfg(feature = "shadows_kv_persist")]
#[allow(incomplete_features)]
impl<const N: usize> KVPersist for heapless::String<N>
where
    [(); N + 5]:,
{
    const MAX_KEY_LEN: usize = 0;
    type ValueBuf = [u8; N + 5];
    fn zero_value_buf() -> Self::ValueBuf {
        [0u8; N + 5]
    }

    fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
        &[]
    }

    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    fn apply_field_default(&mut self, _field_path: &str) -> bool {
        false
    }

    async fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> Result<LoadFieldResult, KvError<K::Error>> {
        let mut result = LoadFieldResult::default();
        let mut buf = [0u8; N + 5];
        match kv.fetch(prefix, &mut buf).await.map_err(KvError::Kv)? {
            Some(data) => {
                *self = postcard::from_bytes(data).map_err(|_| KvError::Serialization)?;
                result.loaded += 1;
            }
            None => result.defaulted += 1,
        }
        Ok(result)
    }

    async fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> Result<LoadFieldResult, KvError<K::Error>> {
        self.load_from_kv::<K, KEY_LEN>(prefix, kv).await
    }

    async fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
    ) -> Result<(), KvError<K::Error>> {
        let mut buf = [0u8; N + 5];
        let bytes = postcard::to_slice(self, &mut buf).map_err(|_| KvError::Serialization)?;
        kv.store(prefix, bytes).await.map_err(KvError::Kv)
    }

    async fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        prefix: &str,
    ) -> Result<(), KvError<K::Error>> {
        let mut buf = [0u8; N + 5];
        let bytes = postcard::to_slice(delta, &mut buf).map_err(|_| KvError::Serialization)?;
        kv.store(prefix, bytes).await.map_err(KvError::Kv)
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        keys(prefix);
    }
}

// =============================================================================
// heapless::Vec<T, N>
// =============================================================================

impl<T, const N: usize> ShadowNode for heapless::Vec<T, N>
where
    T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
{
    type Delta = heapless::Vec<T, N>;
    type Reported = heapless::Vec<T, N>;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"heapless::Vec");

    async fn parse_delta<R: VariantResolver>(
        json: &[u8],
        _path: &str,
        _resolver: &R,
    ) -> Result<Self::Delta, ParseError> {
        serde_json_core::from_slice(json)
            .map(|(v, _)| v)
            .map_err(|_| ParseError::Deserialize)
    }

    fn apply_delta(&mut self, delta: &Self::Delta) {
        *self = delta.clone();
    }

    fn into_reported(&self) -> Self::Reported {
        self.clone()
    }

    fn into_partial_reported(&self, _delta: &Self::Delta) -> Self::Reported {
        self.clone()
    }
}

impl<T, const N: usize> ReportedUnionFields for heapless::Vec<T, N>
where
    T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
{
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

/// `heapless::Vec<T, N>` is stored as an atomic blob value because AWS IoT Shadows
/// treat arrays as normal values — an update to an array replaces the whole array,
/// and it is not possible to update part of an array. This is why `Delta = Self`
/// (full replacement) rather than per-element deltas.
#[cfg(feature = "shadows_kv_persist")]
#[allow(incomplete_features)]
impl<T, const N: usize> KVPersist for heapless::Vec<T, N>
where
    T: Clone + Default + Serialize + DeserializeOwned + MaxSize,
    [(); N * T::POSTCARD_MAX_SIZE + 5]:,
{
    const MAX_KEY_LEN: usize = 0;
    type ValueBuf = [u8; N * T::POSTCARD_MAX_SIZE + 5];
    fn zero_value_buf() -> Self::ValueBuf {
        [0u8; N * T::POSTCARD_MAX_SIZE + 5]
    }

    fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
        &[]
    }

    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    fn apply_field_default(&mut self, _field_path: &str) -> bool {
        false
    }

    async fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> Result<LoadFieldResult, KvError<K::Error>> {
        let mut result = LoadFieldResult::default();
        let mut buf = [0u8; N * T::POSTCARD_MAX_SIZE + 5];
        match kv.fetch(prefix, &mut buf).await.map_err(KvError::Kv)? {
            Some(data) => {
                *self = postcard::from_bytes(data).map_err(|_| KvError::Serialization)?;
                result.loaded += 1;
            }
            None => result.defaulted += 1,
        }
        Ok(result)
    }

    async fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> Result<LoadFieldResult, KvError<K::Error>> {
        self.load_from_kv::<K, KEY_LEN>(prefix, kv).await
    }

    async fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
    ) -> Result<(), KvError<K::Error>> {
        let mut buf = [0u8; N * T::POSTCARD_MAX_SIZE + 5];
        let bytes = postcard::to_slice(self, &mut buf).map_err(|_| KvError::Serialization)?;
        kv.store(prefix, bytes).await.map_err(KvError::Kv)
    }

    async fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        prefix: &str,
    ) -> Result<(), KvError<K::Error>> {
        let mut buf = [0u8; N * T::POSTCARD_MAX_SIZE + 5];
        let bytes = postcard::to_slice(delta, &mut buf).map_err(|_| KvError::Serialization)?;
        kv.store(prefix, bytes).await.map_err(KvError::Kv)
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        keys(prefix);
    }
}

// =============================================================================
// heapless::LinearMap<K, V, N> — Map collection with per-entry Patch deltas
// =============================================================================

/// Delta type for `heapless::LinearMap`-based shadow fields.
///
/// Uses [`Patch`] per entry to support partial map updates:
/// - `Patch::Set(delta)` — update or insert an entry
/// - `Patch::Unset` — remove an entry
///
/// ## Why `Patch::Unset` instead of `null`?
///
/// AWS IoT Shadow does not generate a delta when either `reported` or
/// `desired` is `null`. This means the device never receives a "field was
/// deleted" notification via the normal delta mechanism. `Patch::Unset`
/// is an explicit "remove this entry" marker that can be sent as a desired
/// update and will propagate correctly through the shadow delta flow.
///
/// ## Outer `Option`
///
/// `None` means "no changes to the map" (the map field itself was absent
/// from the delta). `Some(map)` contains per-entry patches.
///
/// Note: This type does not derive `Deserialize` because we use `parse_delta`
/// for JSON parsing to support value types with adjacently-tagged enums
/// (which require alloc for serde Deserialize).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct LinearMapDelta<K: Eq, D, const N: usize>(
    pub Option<heapless::LinearMap<K, Patch<D>, N>>,
);

/// Reported type for `heapless::LinearMap`-based shadow fields.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct LinearMapReported<K: Eq, R, const N: usize>(pub heapless::LinearMap<K, R, N>);

impl<K: Eq + serde::Serialize, R: serde::Serialize, const N: usize> ReportedUnionFields
    for LinearMapReported<K, R, N>
{
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

impl<K, V, const N: usize> ShadowNode for heapless::LinearMap<K, V, N>
where
    K: Clone + Eq + Default + serde::Serialize + serde::de::DeserializeOwned + core::fmt::Display,
    V: ShadowNode,
{
    type Delta = LinearMapDelta<K, V::Delta, N>;
    type Reported = LinearMapReported<K, V::Reported, N>;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"heapless::LinearMap");

    async fn parse_delta<R: VariantResolver>(
        json: &[u8],
        path: &str,
        resolver: &R,
    ) -> Result<Self::Delta, ParseError> {
        use crate::shadows::tag_scanner::ObjectScanner;

        // Check for null (no changes)
        if ObjectScanner::is_null_or_empty(json) {
            return Ok(LinearMapDelta(None));
        }

        let mut scanner = ObjectScanner::new(json).map_err(|_| ParseError::Deserialize)?;
        let mut result: heapless::LinearMap<K, Patch<V::Delta>, N> = heapless::LinearMap::new();

        while let Some((key_bytes, value_bytes)) =
            scanner.next_entry().map_err(|_| ParseError::Deserialize)?
        {
            // Parse the key (key_bytes includes quotes)
            let key: K = serde_json_core::from_slice(key_bytes)
                .map(|(v, _)| v)
                .map_err(|_| ParseError::Deserialize)?;

            // Check for "unset" marker or null
            let trimmed = core::str::from_utf8(value_bytes)
                .map(|s| s.trim())
                .unwrap_or("");
            let patch = if trimmed == "\"unset\"" || trimmed == "null" {
                Patch::Unset
            } else {
                // Build nested path for resolver
                let mut nested_path = heapless::String::<128>::new();
                let _ = nested_path.push_str(path);
                let _ = nested_path.push_str("/");
                // Use core::fmt::Write for key display
                use core::fmt::Write;
                let _ = write!(nested_path, "{}", &key);

                let delta = V::parse_delta(value_bytes, &nested_path, resolver).await?;
                Patch::Set(delta)
            };

            let _ = result.insert(key, patch);
        }

        Ok(LinearMapDelta(Some(result)))
    }

    fn apply_delta(&mut self, delta: &Self::Delta) {
        if let Some(ref patches) = delta.0 {
            for (key, patch) in patches.iter() {
                match patch {
                    Patch::Set(d) => {
                        if let Some(existing) = self.get_mut(key) {
                            existing.apply_delta(d);
                        } else {
                            let mut new_val = V::default();
                            new_val.apply_delta(d);
                            let _ = self.insert(key.clone(), new_val);
                        }
                    }
                    Patch::Unset => {
                        self.remove(key);
                    }
                }
            }
        }
    }

    fn into_reported(&self) -> Self::Reported {
        let mut reported = heapless::LinearMap::new();
        for (key, value) in self.iter() {
            let _ = reported.insert(key.clone(), value.into_reported());
        }
        LinearMapReported(reported)
    }

    fn into_partial_reported(&self, delta: &Self::Delta) -> Self::Reported {
        let mut reported = heapless::LinearMap::new();
        if let Some(ref patches) = delta.0 {
            for (key, patch) in patches.iter() {
                match patch {
                    Patch::Set(inner_delta) => {
                        // Include the entry's partial reported (after apply_delta)
                        if let Some(v) = self.get(key) {
                            let _ =
                                reported.insert(key.clone(), v.into_partial_reported(inner_delta));
                        }
                    }
                    Patch::Unset => {
                        // Unset entries cannot be represented in LinearMapReported
                        // The user must handle explicit null reporting separately
                    }
                }
            }
        }
        LinearMapReported(reported)
    }
}

/// Build a KV key: `prefix/display(key)` into a `heapless::String<KEY_LEN>`.
#[cfg(feature = "shadows_kv_persist")]
fn build_entry_prefix<const KEY_LEN: usize>(
    prefix: &str,
    key: &impl core::fmt::Display,
) -> heapless::String<KEY_LEN> {
    use core::fmt::Write;
    let mut s = heapless::String::<KEY_LEN>::new();
    let _ = s.push_str(prefix);
    let _ = s.push_str("/");
    let _ = write!(s, "{}", key);
    s
}

/// Build a KV key: `prefix + suffix` into a `heapless::String<KEY_LEN>`.
#[cfg(feature = "shadows_kv_persist")]
fn build_key<const KEY_LEN: usize>(prefix: &str, suffix: &str) -> heapless::String<KEY_LEN> {
    let mut key = heapless::String::<KEY_LEN>::new();
    let _ = key.push_str(prefix);
    let _ = key.push_str(suffix);
    key
}

/// LinearMap uses individual key storage to avoid large manifest buffers.
///
/// Storage format:
/// - `prefix/__n__`     → postcard u16 (entry count)
/// - `prefix/__k/0`     → postcard K (key at index 0)
/// - `prefix/__k/1`     → postcard K (key at index 1)
/// - `prefix/{key}/...` → value data (delegated to V)
///
/// This eliminates the `[(); N*(K::MAX_KEY_DISPLAY_LEN+5)+5]:` where bound
/// that previously propagated to user crates. Only `K: MapKey` (which brings
/// `K::KeyBuf`) and `V: KVPersist` are needed.
#[cfg(feature = "shadows_kv_persist")]
impl<K, V, const N: usize> KVPersist for heapless::LinearMap<K, V, N>
where
    K: MapKey + Default + Serialize + DeserializeOwned,
    V: KVPersist,
{
    // "/{key}" + sub-key length
    const MAX_KEY_LEN: usize = 1 + K::MAX_KEY_DISPLAY_LEN + V::MAX_KEY_LEN;

    // Only direct serialization is the u16 count (3 bytes max for postcard varint)
    type ValueBuf = [u8; 3];
    fn zero_value_buf() -> Self::ValueBuf {
        [0u8; 3]
    }

    fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
        &[]
    }

    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    fn apply_field_default(&mut self, _field_path: &str) -> bool {
        false
    }

    async fn load_from_kv<K2: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K2,
    ) -> Result<LoadFieldResult, KvError<K2::Error>> {
        let mut result = LoadFieldResult::default();

        // Read entry count
        let count_key = build_key::<KEY_LEN>(prefix, "/__n__");
        let mut count_buf = [0u8; 3];
        let count: u16 = match kv
            .fetch(&count_key, &mut count_buf)
            .await
            .map_err(KvError::Kv)?
        {
            Some(data) => postcard::from_bytes(data).map_err(|_| KvError::Serialization)?,
            None => {
                result.defaulted += 1;
                return Ok(result);
            }
        };

        // Read each key from individual slots
        for i in 0..count {
            let mut slot_key = heapless::String::<KEY_LEN>::new();
            let _ = slot_key.push_str(prefix);
            let _ = slot_key.push_str("/__k/");
            let _ = core::fmt::Write::write_fmt(&mut slot_key, format_args!("{}", i));

            let mut key_buf = K::zero_key_buf();
            let key: K = match kv
                .fetch(&slot_key, key_buf.as_mut())
                .await
                .map_err(KvError::Kv)?
            {
                Some(data) => postcard::from_bytes(data).map_err(|_| KvError::Serialization)?,
                None => continue, // Skip missing key slots
            };

            let entry_prefix = build_entry_prefix::<KEY_LEN>(prefix, &key);
            let mut value = V::default();
            let inner = value.load_from_kv::<K2, KEY_LEN>(&entry_prefix, kv).await?;
            result.merge(inner);

            let _ = self.insert(key, value);
        }

        Ok(result)
    }

    async fn load_from_kv_with_migration<K2: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K2,
    ) -> Result<LoadFieldResult, KvError<K2::Error>> {
        self.load_from_kv::<K2, KEY_LEN>(prefix, kv).await
    }

    async fn persist_to_kv<K2: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K2,
    ) -> Result<(), KvError<K2::Error>> {
        let mut index: u16 = 0;

        for (key, value) in self.iter() {
            // Write key to slot
            let mut slot_key = heapless::String::<KEY_LEN>::new();
            let _ = slot_key.push_str(prefix);
            let _ = slot_key.push_str("/__k/");
            let _ = core::fmt::Write::write_fmt(&mut slot_key, format_args!("{}", index));

            let mut key_buf = K::zero_key_buf();
            let bytes =
                postcard::to_slice(key, key_buf.as_mut()).map_err(|_| KvError::Serialization)?;
            kv.store(&slot_key, bytes).await.map_err(KvError::Kv)?;

            // Write value
            let entry_prefix = build_entry_prefix::<KEY_LEN>(prefix, key);
            value
                .persist_to_kv::<K2, KEY_LEN>(&entry_prefix, kv)
                .await?;

            index += 1;
        }

        // Write count
        let count_key = build_key::<KEY_LEN>(prefix, "/__n__");
        let mut count_buf = [0u8; 3];
        let bytes =
            postcard::to_slice(&index, &mut count_buf).map_err(|_| KvError::Serialization)?;
        kv.store(&count_key, bytes).await.map_err(KvError::Kv)?;

        Ok(())
    }

    async fn persist_delta<K2: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K2,
        prefix: &str,
    ) -> Result<(), KvError<K2::Error>> {
        if let Some(ref patches) = delta.0 {
            // Read current keys from individual slots
            let count_key = build_key::<KEY_LEN>(prefix, "/__n__");
            let mut count_buf = [0u8; 3];
            let current_count: u16 = match kv
                .fetch(&count_key, &mut count_buf)
                .await
                .map_err(KvError::Kv)?
            {
                Some(data) => postcard::from_bytes(data).unwrap_or(0),
                None => 0,
            };

            let mut current_keys: heapless::Vec<K, N> = heapless::Vec::new();
            for i in 0..current_count {
                let mut slot_key = heapless::String::<KEY_LEN>::new();
                let _ = slot_key.push_str(prefix);
                let _ = slot_key.push_str("/__k/");
                let _ = core::fmt::Write::write_fmt(&mut slot_key, format_args!("{}", i));

                let mut key_buf = K::zero_key_buf();
                if let Some(data) = kv
                    .fetch(&slot_key, key_buf.as_mut())
                    .await
                    .map_err(KvError::Kv)?
                {
                    if let Ok(key) = postcard::from_bytes::<K>(data) {
                        let _ = current_keys.push(key);
                    }
                }
            }

            // Apply patches
            for (key, patch) in patches.iter() {
                let entry_prefix = build_entry_prefix::<KEY_LEN>(prefix, key);

                match patch {
                    Patch::Set(d) => {
                        V::persist_delta::<K2, KEY_LEN>(d, kv, &entry_prefix).await?;

                        // Add to keys if not present
                        if !current_keys.iter().any(|k| k == key) {
                            let _ = current_keys.push(key.clone());
                        }
                    }
                    Patch::Unset => {
                        // Remove entry and any sub-keys
                        kv.remove(&entry_prefix).await.map_err(KvError::Kv)?;

                        let mut prefix_slash = build_entry_prefix::<KEY_LEN>(prefix, key);
                        let _ = prefix_slash.push_str("/");
                        let _ = kv
                            .remove_if(&prefix_slash, |_| true)
                            .await
                            .map_err(KvError::Kv)?;

                        // Remove from keys
                        current_keys.retain(|k| k != key);
                    }
                }
            }

            // Rewrite all key slots + count
            // First, remove old slots that may be beyond new count
            for i in current_keys.len()..current_count as usize {
                let mut slot_key = heapless::String::<KEY_LEN>::new();
                let _ = slot_key.push_str(prefix);
                let _ = slot_key.push_str("/__k/");
                let _ = core::fmt::Write::write_fmt(&mut slot_key, format_args!("{}", i));
                let _ = kv.remove(&slot_key).await;
            }

            // Write new key slots
            for (i, key) in current_keys.iter().enumerate() {
                let mut slot_key = heapless::String::<KEY_LEN>::new();
                let _ = slot_key.push_str(prefix);
                let _ = slot_key.push_str("/__k/");
                let _ = core::fmt::Write::write_fmt(&mut slot_key, format_args!("{}", i));

                let mut key_buf = K::zero_key_buf();
                let bytes = postcard::to_slice(key, key_buf.as_mut())
                    .map_err(|_| KvError::Serialization)?;
                kv.store(&slot_key, bytes).await.map_err(KvError::Kv)?;
            }

            // Write updated count
            let new_count = current_keys.len() as u16;
            let mut count_buf = [0u8; 3];
            let bytes = postcard::to_slice(&new_count, &mut count_buf)
                .map_err(|_| KvError::Serialization)?;
            kv.store(&count_key, bytes).await.map_err(KvError::Kv)?;
        }

        Ok(())
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        let count_key = build_key::<KEY_LEN>(prefix, "/__n__");
        keys(&count_key);
    }

    fn collect_valid_prefixes<const KEY_LEN: usize>(prefix: &str, prefixes: &mut impl FnMut(&str)) {
        // Cover key slots: prefix/__k/
        let mut key_pfx = heapless::String::<KEY_LEN>::new();
        let _ = key_pfx.push_str(prefix);
        let _ = key_pfx.push_str("/__k/");
        prefixes(&key_pfx);

        // Cover entry data: prefix/
        let mut data_pfx = heapless::String::<KEY_LEN>::new();
        let _ = data_pfx.push_str(prefix);
        let _ = data_pfx.push_str("/");
        prefixes(&data_pfx);
    }
}

// =============================================================================
// [T; N] — Fixed-size arrays as atomic blob values
// =============================================================================

/// Fixed-size arrays are treated as atomic blob values because AWS IoT Shadows
/// treat arrays as normal values — an update to an array replaces the whole array,
/// and it is not possible to update part of an array. This is why `Delta = Self`
/// (full replacement) rather than per-element deltas.
///
/// Supported for sizes 1..=16 (serde only provides Serialize/Deserialize impls
/// for arrays up to size 32 via concrete impls, not generic const N).
macro_rules! impl_array_shadow_node {
    ($($n:literal),+) => { $(
        impl<T> ShadowNode for [T; $n]
        where
            T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
        {
            type Delta = [T; $n];
            type Reported = [T; $n];

            const SCHEMA_HASH: u64 = fnv1a_hash(b"[T; N]");

            async fn parse_delta<R: VariantResolver>(
                json: &[u8],
                _path: &str,
                _resolver: &R,
            ) -> Result<Self::Delta, ParseError> {
                serde_json_core::from_slice(json)
                    .map(|(v, _)| v)
                    .map_err(|_| ParseError::Deserialize)
            }

            fn apply_delta(&mut self, delta: &Self::Delta) {
                *self = delta.clone();
            }

            fn into_reported(&self) -> Self::Reported {
                self.clone()
            }

            fn into_partial_reported(&self, _delta: &Self::Delta) -> Self::Reported {
                self.clone()
            }
        }

        impl<T> ReportedUnionFields for [T; $n]
        where
            T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
        {
            const FIELD_NAMES: &'static [&'static str] = &[];

            fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
                Ok(())
            }
        }
    )+ };
}

impl_array_shadow_node!(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);

#[cfg(feature = "shadows_kv_persist")]
macro_rules! impl_array_kv_persist {
    ($($n:literal),+) => { $(
        #[allow(incomplete_features)]
        impl<T> KVPersist for [T; $n]
        where
            T: Clone + Default + Serialize + DeserializeOwned + MaxSize,
            [(); $n * T::POSTCARD_MAX_SIZE]:,
        {
            const MAX_KEY_LEN: usize = 0;
            type ValueBuf = [u8; $n * T::POSTCARD_MAX_SIZE];
            fn zero_value_buf() -> Self::ValueBuf { [0u8; $n * T::POSTCARD_MAX_SIZE] }

            fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
                &[]
            }

            fn all_migration_keys() -> impl Iterator<Item = &'static str> {
                core::iter::empty()
            }

            fn apply_field_default(&mut self, _field_path: &str) -> bool {
                false
            }

            async fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
                &mut self,
                prefix: &str,
                kv: &K,
            ) -> Result<LoadFieldResult, KvError<K::Error>> {
                let mut result = LoadFieldResult::default();
                let mut buf = [0u8; $n * T::POSTCARD_MAX_SIZE];
                match kv.fetch(prefix, buf.as_mut()).await.map_err(KvError::Kv)? {
                    Some(data) => {
                        *self = postcard::from_bytes(data).map_err(|_| KvError::Serialization)?;
                        result.loaded += 1;
                    }
                    None => result.defaulted += 1,
                }
                Ok(result)
            }

            async fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
                &mut self,
                prefix: &str,
                kv: &K,
            ) -> Result<LoadFieldResult, KvError<K::Error>> {
                self.load_from_kv::<K, KEY_LEN>(prefix, kv).await
            }

            async fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
                &self,
                prefix: &str,
                kv: &K,
            ) -> Result<(), KvError<K::Error>> {
                let mut buf = [0u8; $n * T::POSTCARD_MAX_SIZE];
                let bytes = postcard::to_slice(self, buf.as_mut()).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, bytes).await.map_err(KvError::Kv)
            }

            async fn persist_delta<K: KVStore, const KEY_LEN: usize>(
                delta: &Self::Delta,
                kv: &K,
                prefix: &str,
            ) -> Result<(), KvError<K::Error>> {
                let mut buf = [0u8; $n * T::POSTCARD_MAX_SIZE];
                let bytes = postcard::to_slice(delta, buf.as_mut()).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, bytes).await.map_err(KvError::Kv)
            }

            fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
                keys(prefix);
            }
        }
    )+ };
}

#[cfg(feature = "shadows_kv_persist")]
impl_array_kv_persist!(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linear_map_apply_delta_set() {
        let mut map: heapless::LinearMap<heapless::String<4>, u32, 4> = heapless::LinearMap::new();

        let mut patches = heapless::LinearMap::new();
        let _ = patches.insert(heapless::String::try_from("a").unwrap(), Patch::Set(42u32));
        let delta = LinearMapDelta(Some(patches));

        map.apply_delta(&delta);
        assert_eq!(
            map.get(&heapless::String::<4>::try_from("a").unwrap()),
            Some(&42)
        );
    }

    #[test]
    fn test_linear_map_apply_delta_unset() {
        let mut map: heapless::LinearMap<heapless::String<4>, u32, 4> = heapless::LinearMap::new();
        let _ = map.insert(heapless::String::try_from("a").unwrap(), 42);

        let mut patches = heapless::LinearMap::new();
        let _ = patches.insert(
            heapless::String::try_from("a").unwrap(),
            Patch::<u32>::Unset,
        );
        let delta = LinearMapDelta(Some(patches));

        map.apply_delta(&delta);
        assert!(map
            .get(&heapless::String::<4>::try_from("a").unwrap())
            .is_none());
    }

    #[test]
    fn test_linear_map_apply_delta_none() {
        let mut map: heapless::LinearMap<heapless::String<4>, u32, 4> = heapless::LinearMap::new();
        let _ = map.insert(heapless::String::try_from("a").unwrap(), 42);

        let delta = LinearMapDelta::<heapless::String<4>, u32, 4>(None);
        map.apply_delta(&delta);
        assert_eq!(
            map.get(&heapless::String::<4>::try_from("a").unwrap()),
            Some(&42)
        );
    }

    #[cfg(all(test, feature = "shadows_kv_persist", feature = "std"))]
    mod kv_tests {
        use super::*;

        #[tokio::test]
        async fn test_linear_map_kv_roundtrip() {
            use crate::shadows::store::FileKVStore;

            let kv = FileKVStore::temp().unwrap();
            kv.init().await.unwrap();

            let mut map: heapless::LinearMap<heapless::String<4>, u32, 4> =
                heapless::LinearMap::new();
            let _ = map.insert(heapless::String::try_from("x").unwrap(), 10);
            let _ = map.insert(heapless::String::try_from("y").unwrap(), 20);

            map.persist_to_kv::<FileKVStore, 128>("test", &kv)
                .await
                .unwrap();

            let mut loaded: heapless::LinearMap<heapless::String<4>, u32, 4> =
                heapless::LinearMap::new();
            let result = loaded
                .load_from_kv::<FileKVStore, 128>("test", &kv)
                .await
                .unwrap();

            assert_eq!(result.loaded, 2);
            assert_eq!(
                loaded.get(&heapless::String::<4>::try_from("x").unwrap()),
                Some(&10)
            );
            assert_eq!(
                loaded.get(&heapless::String::<4>::try_from("y").unwrap()),
                Some(&20)
            );
        }

        #[tokio::test]
        async fn test_linear_map_persist_delta() {
            use crate::shadows::store::FileKVStore;

            let kv = FileKVStore::temp().unwrap();
            kv.init().await.unwrap();

            let mut map: heapless::LinearMap<heapless::String<4>, u32, 4> =
                heapless::LinearMap::new();
            let _ = map.insert(heapless::String::try_from("a").unwrap(), 1);
            map.persist_to_kv::<FileKVStore, 128>("test", &kv)
                .await
                .unwrap();

            let mut patches = heapless::LinearMap::new();
            let _ = patches.insert(heapless::String::try_from("b").unwrap(), Patch::Set(2u32));
            let _ = patches.insert(
                heapless::String::try_from("a").unwrap(),
                Patch::<u32>::Unset,
            );
            let delta = LinearMapDelta(Some(patches));

            <heapless::LinearMap<heapless::String<4>, u32, 4> as KVPersist>::persist_delta::<
                FileKVStore,
                128,
            >(&delta, &kv, "test")
            .await
            .unwrap();

            let mut loaded: heapless::LinearMap<heapless::String<4>, u32, 4> =
                heapless::LinearMap::new();
            loaded
                .load_from_kv::<FileKVStore, 128>("test", &kv)
                .await
                .unwrap();

            assert!(loaded
                .get(&heapless::String::<4>::try_from("a").unwrap())
                .is_none());
            assert_eq!(
                loaded.get(&heapless::String::<4>::try_from("b").unwrap()),
                Some(&2)
            );
        }
    }

    #[cfg(all(test, feature = "shadows_kv_persist", feature = "std"))]
    mod array_kv_tests {
        use super::*;
        use crate::shadows::store::FileKVStore;

        #[tokio::test]
        async fn test_array_kv_roundtrip() {
            let kv = FileKVStore::temp().unwrap();
            kv.init().await.unwrap();

            let arr: [u32; 4] = [10, 20, 30, 40];
            arr.persist_to_kv::<FileKVStore, 128>("arr", &kv)
                .await
                .unwrap();

            let mut loaded = [0u32; 4];
            let result = loaded
                .load_from_kv::<FileKVStore, 128>("arr", &kv)
                .await
                .unwrap();

            assert_eq!(result.loaded, 1);
            assert_eq!(loaded, [10, 20, 30, 40]);
        }

        #[tokio::test]
        async fn test_array_persist_delta() {
            let kv = FileKVStore::temp().unwrap();
            kv.init().await.unwrap();

            let arr: [u32; 3] = [1, 2, 3];
            arr.persist_to_kv::<FileKVStore, 128>("arr", &kv)
                .await
                .unwrap();

            // Delta replaces entire array
            let new_arr: [u32; 3] = [4, 5, 6];
            <[u32; 3] as KVPersist>::persist_delta::<FileKVStore, 128>(&new_arr, &kv, "arr")
                .await
                .unwrap();

            let mut loaded = [0u32; 3];
            loaded
                .load_from_kv::<FileKVStore, 128>("arr", &kv)
                .await
                .unwrap();

            assert_eq!(loaded, [4, 5, 6]);
        }
    }

    #[cfg(all(test, feature = "shadows_kv_persist", feature = "std"))]
    mod linear_map_format_tests {
        use super::*;
        use crate::shadows::store::FileKVStore;

        #[tokio::test]
        async fn test_linear_map_individual_key_format() {
            let kv = FileKVStore::temp().unwrap();
            kv.init().await.unwrap();

            let mut map: heapless::LinearMap<heapless::String<4>, u32, 4> =
                heapless::LinearMap::new();
            let _ = map.insert(heapless::String::try_from("a").unwrap(), 10);
            let _ = map.insert(heapless::String::try_from("b").unwrap(), 20);

            map.persist_to_kv::<FileKVStore, 128>("m", &kv)
                .await
                .unwrap();

            // Verify __n__ key exists with count = 2
            let mut buf = [0u8; 3];
            let data = kv.fetch("m/__n__", &mut buf).await.unwrap().unwrap();
            let count: u16 = postcard::from_bytes(data).unwrap();
            assert_eq!(count, 2);

            // Verify individual key slots exist
            let mut key_buf = [0u8; 10];
            let data = kv.fetch("m/__k/0", &mut key_buf).await.unwrap().unwrap();
            let _key0: heapless::String<4> = postcard::from_bytes(data).unwrap();
            let data = kv.fetch("m/__k/1", &mut key_buf).await.unwrap().unwrap();
            let _key1: heapless::String<4> = postcard::from_bytes(data).unwrap();
        }

        #[tokio::test]
        async fn test_linear_map_collect_valid_keys_format() {
            let mut keys = std::vec::Vec::new();
            <heapless::LinearMap<heapless::String<4>, u32, 4> as KVPersist>::collect_valid_keys::<
                128,
            >("m", &mut |k| keys.push(k.to_string()));

            assert_eq!(keys, vec!["m/__n__"]);
        }
    }
}
