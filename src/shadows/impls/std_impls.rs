//! ShadowNode implementations for std container types.
//!
//! - `String` — opaque leaf type
//! - `Vec<T>` — opaque leaf type
//! - `HashMap<K, V>` — map collection with per-entry Patch deltas

use crate::shadows::{fnv1a_hash, ParseError, ReportedUnionFields, ShadowNode, VariantResolver};
use serde::ser::SerializeMap;
use std::collections::HashMap;
use std::string::String;
use std::vec::Vec;

#[cfg(feature = "shadows_kv_persist")]
use crate::shadows::{KVPersist, KVStore, KvError, LoadFieldResult, MapKey, MigrationSource};
#[cfg(feature = "shadows_kv_persist")]
use serde::{de::DeserializeOwned, Serialize};

use crate::shadows::data_types::Patch;
use std::hash::Hash;

// =============================================================================
// String
// =============================================================================

impl ShadowNode for String {
    type Delta = String;
    type Reported = String;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"String");

    async fn parse_delta<R: VariantResolver>(
        json: &[u8],
        _path: &str,
        _resolver: &R,
    ) -> Result<Self::Delta, ParseError> {
        serde_json::from_slice(json).map_err(|_| ParseError::Deserialize)
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

impl ReportedUnionFields for String {
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

#[cfg(feature = "shadows_kv_persist")]
impl KVPersist for String {
    const MAX_KEY_LEN: usize = 0;
    // std uses allocating path (to_allocvec/fetch_to_vec), buffer unused
    type ValueBuf = [u8; 0];
    fn zero_value_buf() -> Self::ValueBuf {
        []
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
        match kv.fetch_to_vec(prefix).await.map_err(KvError::Kv)? {
            Some(data) => {
                *self = postcard::from_bytes(&data).map_err(|_| KvError::Serialization)?;
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
        let bytes = postcard::to_allocvec(self).map_err(|_| KvError::Serialization)?;
        kv.store(prefix, &bytes).await.map_err(KvError::Kv)
    }

    async fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        prefix: &str,
    ) -> Result<(), KvError<K::Error>> {
        let bytes = postcard::to_allocvec(delta).map_err(|_| KvError::Serialization)?;
        kv.store(prefix, &bytes).await.map_err(KvError::Kv)
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        keys(prefix);
    }
}

// =============================================================================
// Vec<T>
// =============================================================================

impl<T> ShadowNode for Vec<T>
where
    T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
{
    type Delta = Vec<T>;
    type Reported = Vec<T>;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"Vec");

    async fn parse_delta<R: VariantResolver>(
        json: &[u8],
        _path: &str,
        _resolver: &R,
    ) -> Result<Self::Delta, ParseError> {
        serde_json::from_slice(json).map_err(|_| ParseError::Deserialize)
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

impl<T> ReportedUnionFields for Vec<T>
where
    T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
{
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

/// `std::Vec<T>` is stored as an atomic blob value because AWS IoT Shadows
/// treat arrays as normal values — an update to an array replaces the whole array,
/// and it is not possible to update part of an array. This is why `Delta = Self`
/// (full replacement) rather than per-element deltas.
#[cfg(feature = "shadows_kv_persist")]
impl<T> KVPersist for Vec<T>
where
    T: Clone + Default + Serialize + DeserializeOwned,
{
    const MAX_KEY_LEN: usize = 0;
    // std uses allocating path (to_allocvec/fetch_to_vec), buffer unused
    type ValueBuf = [u8; 0];
    fn zero_value_buf() -> Self::ValueBuf {
        []
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
        match kv.fetch_to_vec(prefix).await.map_err(KvError::Kv)? {
            Some(data) => {
                *self = postcard::from_bytes(&data).map_err(|_| KvError::Serialization)?;
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
        let bytes = postcard::to_allocvec(self).map_err(|_| KvError::Serialization)?;
        kv.store(prefix, &bytes).await.map_err(KvError::Kv)
    }

    async fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        prefix: &str,
    ) -> Result<(), KvError<K::Error>> {
        let bytes = postcard::to_allocvec(delta).map_err(|_| KvError::Serialization)?;
        kv.store(prefix, &bytes).await.map_err(KvError::Kv)
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        keys(prefix);
    }
}

// =============================================================================
// HashMap<K, V> — Map collection with per-entry Patch deltas
// =============================================================================

/// Delta type for `HashMap`-based shadow fields.
///
/// Uses [`Patch`] per entry to support partial map updates:
/// - `Patch::Set(delta)` — update or insert an entry
/// - `Patch::Unset` — remove an entry
///
/// ## Outer `Option`
///
/// `None` means "no changes to the map" (the map field itself was absent
/// from the delta). `Some(map)` contains per-entry patches.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HashMapDelta<K: Eq + Hash, D>(pub Option<HashMap<K, Patch<D>>>);

/// Reported type for `HashMap`-based shadow fields.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct HashMapReported<K: Eq + Hash, R>(pub HashMap<K, R>);

impl<K: Eq + Hash + serde::Serialize, R: serde::Serialize> ReportedUnionFields
    for HashMapReported<K, R>
{
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

impl<K, V> ShadowNode for HashMap<K, V>
where
    K: Clone
        + Eq
        + Hash
        + Default
        + serde::Serialize
        + serde::de::DeserializeOwned
        + core::fmt::Display,
    V: ShadowNode,
{
    type Delta = HashMapDelta<K, V::Delta>;
    type Reported = HashMapReported<K, V::Reported>;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"HashMap");

    async fn parse_delta<R: VariantResolver>(
        json: &[u8],
        path: &str,
        resolver: &R,
    ) -> Result<Self::Delta, ParseError> {
        use crate::shadows::tag_scanner::ObjectScanner;

        // Check for null (no changes)
        if ObjectScanner::is_null_or_empty(json) {
            return Ok(HashMapDelta(None));
        }

        let mut scanner = ObjectScanner::new(json).map_err(|_| ParseError::Deserialize)?;
        let mut result: HashMap<K, Patch<V::Delta>> = HashMap::new();

        while let Some((key_bytes, value_bytes)) =
            scanner.next_entry().map_err(|_| ParseError::Deserialize)?
        {
            // Parse the key (key_bytes includes quotes)
            let key: K = serde_json::from_slice(key_bytes).map_err(|_| ParseError::Deserialize)?;

            // Check for "unset" marker or null
            let trimmed = core::str::from_utf8(value_bytes)
                .map(|s| s.trim())
                .unwrap_or("");
            let patch = if trimmed == "\"unset\"" || trimmed == "null" {
                Patch::Unset
            } else {
                // Build nested path for resolver
                let nested_path = format!("{}/{}", path, key);

                let delta = V::parse_delta(value_bytes, &nested_path, resolver).await?;
                Patch::Set(delta)
            };

            result.insert(key, patch);
        }

        Ok(HashMapDelta(Some(result)))
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
                            self.insert(key.clone(), new_val);
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
        let mut reported = HashMap::new();
        for (key, value) in self.iter() {
            reported.insert(key.clone(), value.into_reported());
        }
        HashMapReported(reported)
    }

    fn into_partial_reported(&self, delta: &Self::Delta) -> Self::Reported {
        let mut reported = HashMap::new();
        if let Some(ref patches) = delta.0 {
            for (key, patch) in patches.iter() {
                match patch {
                    Patch::Set(inner_delta) => {
                        // Include the entry's partial reported (after apply_delta)
                        if let Some(v) = self.get(key) {
                            reported.insert(key.clone(), v.into_partial_reported(inner_delta));
                        }
                    }
                    Patch::Unset => {
                        // Unset entries cannot be represented in HashMapReported
                        // The user must handle explicit null reporting separately
                    }
                }
            }
        }
        HashMapReported(reported)
    }
}

#[cfg(feature = "shadows_kv_persist")]
impl<K, V> KVPersist for HashMap<K, V>
where
    K: MapKey + Default + Hash,
    V: KVPersist,
{
    // "/{key}" + sub-key length
    const MAX_KEY_LEN: usize = 1 + K::MAX_KEY_DISPLAY_LEN + V::MAX_KEY_LEN;

    // std uses allocating path (to_allocvec/fetch_to_vec), buffer unused
    type ValueBuf = [u8; 0];
    fn zero_value_buf() -> Self::ValueBuf {
        []
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

        // Read manifest: prefix/__keys__
        let manifest_key = format!("{}/__keys__", prefix);

        let key_strings: Vec<String> =
            match kv.fetch_to_vec(&manifest_key).await.map_err(KvError::Kv)? {
                Some(data) => postcard::from_bytes(&data).map_err(|_| KvError::Serialization)?,
                None => {
                    result.defaulted += 1;
                    return Ok(result);
                }
            };

        for key_str in key_strings.iter() {
            let entry_prefix = format!("{}/{}", prefix, key_str);

            let key: K = serde_json_core::from_str::<K>(&format!("\"{}\"", key_str))
                .map(|(k, _)| k)
                .map_err(|_| KvError::Serialization)?;

            let mut value = V::default();
            let inner = value.load_from_kv::<K2, KEY_LEN>(&entry_prefix, kv).await?;
            result.merge(inner);

            self.insert(key, value);
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
        let mut key_strings: Vec<String> = Vec::new();

        for (key, value) in self.iter() {
            let key_str = format!("{}", key);
            key_strings.push(key_str.clone());

            let entry_prefix = format!("{}/{}", prefix, key_str);
            value
                .persist_to_kv::<K2, KEY_LEN>(&entry_prefix, kv)
                .await?;
        }

        // Write manifest
        let manifest_key = format!("{}/__keys__", prefix);
        let bytes = postcard::to_allocvec(&key_strings).map_err(|_| KvError::Serialization)?;
        kv.store(&manifest_key, &bytes).await.map_err(KvError::Kv)?;

        Ok(())
    }

    async fn persist_delta<K2: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K2,
        prefix: &str,
    ) -> Result<(), KvError<K2::Error>> {
        if let Some(ref patches) = delta.0 {
            let manifest_key = format!("{}/__keys__", prefix);

            let mut key_strings: Vec<String> =
                match kv.fetch_to_vec(&manifest_key).await.map_err(KvError::Kv)? {
                    Some(data) => postcard::from_bytes(&data).unwrap_or_default(),
                    None => Vec::new(),
                };

            for (key, patch) in patches.iter() {
                let key_str = format!("{}", key);
                let entry_prefix = format!("{}/{}", prefix, key_str);

                match patch {
                    Patch::Set(d) => {
                        V::persist_delta::<K2, KEY_LEN>(d, kv, &entry_prefix).await?;

                        if !key_strings.iter().any(|k| k == &key_str) {
                            key_strings.push(key_str);
                        }
                    }
                    Patch::Unset => {
                        kv.remove(&entry_prefix).await.map_err(KvError::Kv)?;

                        let entry_prefix_slash = format!("{}/", entry_prefix);
                        let _ = kv
                            .remove_if(&entry_prefix_slash, |_| true)
                            .await
                            .map_err(KvError::Kv)?;

                        key_strings.retain(|k| k != &key_str);
                    }
                }
            }

            // Write updated manifest
            let bytes = postcard::to_allocvec(&key_strings).map_err(|_| KvError::Serialization)?;
            kv.store(&manifest_key, &bytes).await.map_err(KvError::Kv)?;
        }

        Ok(())
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        let manifest_key = format!("{}/__keys__", prefix);
        keys(&manifest_key);
    }

    fn collect_valid_prefixes<const KEY_LEN: usize>(prefix: &str, prefixes: &mut impl FnMut(&str)) {
        let mut pfx = format!("{}/", prefix);
        prefixes(&pfx);
        pfx.clear(); // just to suppress unused warning
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hashmap_apply_delta_set() {
        let mut map: HashMap<String, u32> = HashMap::new();

        let mut patches = HashMap::new();
        patches.insert("a".to_string(), Patch::Set(42u32));
        let delta = HashMapDelta(Some(patches));

        map.apply_delta(&delta);
        assert_eq!(map.get("a"), Some(&42));
    }

    #[test]
    fn test_hashmap_apply_delta_unset() {
        let mut map: HashMap<String, u32> = HashMap::new();
        map.insert("a".to_string(), 42);

        let mut patches = HashMap::new();
        patches.insert("a".to_string(), Patch::<u32>::Unset);
        let delta = HashMapDelta(Some(patches));

        map.apply_delta(&delta);
        assert!(map.get("a").is_none());
    }

    #[test]
    fn test_hashmap_apply_delta_none() {
        let mut map: HashMap<String, u32> = HashMap::new();
        map.insert("a".to_string(), 42);

        let delta = HashMapDelta::<String, u32>(None);
        map.apply_delta(&delta);
        assert_eq!(map.get("a"), Some(&42));
    }

    #[cfg(feature = "shadows_kv_persist")]
    mod kv_tests {
        use super::*;

        #[tokio::test]
        async fn test_hashmap_kv_roundtrip() {
            use crate::shadows::store::FileKVStore;

            let kv = FileKVStore::temp().unwrap();
            kv.init().await.unwrap();

            let mut map: HashMap<String, u32> = HashMap::new();
            map.insert("x".to_string(), 10);
            map.insert("y".to_string(), 20);

            // Persist
            map.persist_to_kv::<FileKVStore, 128>("test", &kv)
                .await
                .unwrap();

            // Load
            let mut loaded: HashMap<String, u32> = HashMap::new();
            let result = loaded
                .load_from_kv::<FileKVStore, 128>("test", &kv)
                .await
                .unwrap();

            assert_eq!(result.loaded, 2);
            assert_eq!(loaded.get("x"), Some(&10));
            assert_eq!(loaded.get("y"), Some(&20));
        }

        #[tokio::test]
        async fn test_hashmap_persist_delta() {
            use crate::shadows::store::FileKVStore;

            let kv = FileKVStore::temp().unwrap();
            kv.init().await.unwrap();

            let mut map: HashMap<String, u32> = HashMap::new();
            map.insert("a".to_string(), 1);
            map.persist_to_kv::<FileKVStore, 128>("test", &kv)
                .await
                .unwrap();

            // Delta: add "b", remove "a"
            let mut patches = HashMap::new();
            patches.insert("b".to_string(), Patch::Set(2u32));
            patches.insert("a".to_string(), Patch::<u32>::Unset);
            let delta = HashMapDelta(Some(patches));

            <HashMap<String, u32> as KVPersist>::persist_delta::<FileKVStore, 128>(
                &delta, &kv, "test",
            )
            .await
            .unwrap();

            // Load and verify
            let mut loaded: HashMap<String, u32> = HashMap::new();
            loaded
                .load_from_kv::<FileKVStore, 128>("test", &kv)
                .await
                .unwrap();

            assert!(loaded.get("a").is_none());
            assert_eq!(loaded.get("b"), Some(&2));
        }
    }
}
