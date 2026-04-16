//! ShadowNode implementations for std container types.
//!
//! - `String` — opaque leaf type
//! - `Vec<T>` — opaque leaf type
//! - `HashMap<K, V>` — map collection with per-entry Patch deltas

use crate::shadows::{ParseError, ReportedFields, ShadowNode, VariantResolver, fnv1a_hash};
use serde::ser::SerializeMap;
use std::collections::HashMap;
use std::string::String;
use std::vec::Vec;

#[cfg(feature = "shadows_kv_persist")]
use crate::shadows::{KVPersist, KVStore, KvError, LoadFieldResult, MapKey, MigrationSource};
#[cfg(feature = "shadows_kv_persist")]
use serde::{Serialize, de::DeserializeOwned};

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

    fn into_delta(&self) -> Self::Delta {
        self.clone()
    }

    fn into_partial_reported(&self, _delta: &Self::Delta) -> Self::Reported {
        self.clone()
    }
}

impl ReportedFields for String {
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

impl crate::shadows::ReportedDiff for String {
    fn diff(&mut self, old: &Self) -> bool {
        *self != *old
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
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K,
    ) -> Result<LoadFieldResult, KvError<K::Error>> {
        let mut result = LoadFieldResult::default();
        match kv
            .fetch_to_vec(key_buf.as_str())
            .await
            .map_err(KvError::Kv)?
        {
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
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K,
    ) -> Result<LoadFieldResult, KvError<K::Error>> {
        self.load_from_kv::<K, KEY_LEN>(key_buf, kv).await
    }

    async fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K,
    ) -> Result<(), KvError<K::Error>> {
        let bytes = postcard::to_allocvec(self).map_err(|_| KvError::Serialization)?;
        kv.store(key_buf.as_str(), &bytes)
            .await
            .map_err(KvError::Kv)
    }

    async fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        key_buf: &mut heapless::String<KEY_LEN>,
    ) -> Result<(), KvError<K::Error>> {
        let bytes = postcard::to_allocvec(delta).map_err(|_| KvError::Serialization)?;
        kv.store(key_buf.as_str(), &bytes)
            .await
            .map_err(KvError::Kv)
    }

    const FIELD_COUNT: usize = 1;

    fn is_valid_key(rel_key: &str) -> bool {
        rel_key.is_empty()
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

    fn into_delta(&self) -> Self::Delta {
        self.clone()
    }

    fn into_partial_reported(&self, _delta: &Self::Delta) -> Self::Reported {
        self.clone()
    }
}

impl<T> ReportedFields for Vec<T>
where
    T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
{
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

impl<T: PartialEq> crate::shadows::ReportedDiff for Vec<T> {
    fn diff(&mut self, old: &Self) -> bool {
        *self != *old
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
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K,
    ) -> Result<LoadFieldResult, KvError<K::Error>> {
        let mut result = LoadFieldResult::default();
        match kv
            .fetch_to_vec(key_buf.as_str())
            .await
            .map_err(KvError::Kv)?
        {
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
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K,
    ) -> Result<LoadFieldResult, KvError<K::Error>> {
        self.load_from_kv::<K, KEY_LEN>(key_buf, kv).await
    }

    async fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K,
    ) -> Result<(), KvError<K::Error>> {
        let bytes = postcard::to_allocvec(self).map_err(|_| KvError::Serialization)?;
        kv.store(key_buf.as_str(), &bytes)
            .await
            .map_err(KvError::Kv)
    }

    async fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        key_buf: &mut heapless::String<KEY_LEN>,
    ) -> Result<(), KvError<K::Error>> {
        let bytes = postcard::to_allocvec(delta).map_err(|_| KvError::Serialization)?;
        kv.store(key_buf.as_str(), &bytes)
            .await
            .map_err(KvError::Kv)
    }

    const FIELD_COUNT: usize = 1;

    fn is_valid_key(rel_key: &str) -> bool {
        rel_key.is_empty()
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
#[serde(transparent)]
pub struct DeltaHashMap<K: Eq + Hash, D>(pub Option<HashMap<K, Patch<D>>>);

/// Reported type for `HashMap`-based shadow fields.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct ReportedHashMap<K: Eq + Hash, R>(pub HashMap<K, R>);

impl<K: Eq + Hash + serde::Serialize, R: serde::Serialize> ReportedFields
    for ReportedHashMap<K, R>
{
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error> {
        for (key, value) in self.0.iter() {
            map.serialize_entry(key, value)?;
        }
        Ok(())
    }
}

impl<K: Eq + Hash, R: crate::shadows::ReportedDiff> crate::shadows::ReportedDiff
    for ReportedHashMap<K, R>
{
    fn diff(&mut self, old: &Self) -> bool {
        self.0.retain(|k, v| match old.0.get(k) {
            Some(old_v) => v.diff(old_v),
            None => true,
        });
        !self.0.is_empty()
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
    V: ShadowNode + Default,
{
    type Delta = DeltaHashMap<K, V::Delta>;
    type Reported = ReportedHashMap<K, V::Reported>;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"HashMap");

    async fn parse_delta<R: VariantResolver>(
        json: &[u8],
        path: &str,
        resolver: &R,
    ) -> Result<Self::Delta, ParseError> {
        use crate::shadows::tag_scanner::ObjectScanner;

        // Check for null (no changes)
        if ObjectScanner::is_null_or_empty(json) {
            return Ok(DeltaHashMap(None));
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

        Ok(DeltaHashMap(Some(result)))
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
        ReportedHashMap(reported)
    }

    fn into_delta(&self) -> Self::Delta {
        let mut map = HashMap::new();
        for (key, value) in self.iter() {
            map.insert(key.clone(), Patch::Set(value.into_delta()));
        }
        DeltaHashMap(Some(map))
    }

    fn into_partial_reported(&self, delta: &Self::Delta) -> Self::Reported {
        let mut reported = HashMap::new();
        if let Some(ref patches) = delta.0 {
            for (key, patch) in patches.iter() {
                match patch {
                    Patch::Set(inner_delta) => {
                        if let Some(v) = self.get(key) {
                            reported.insert(key.clone(), v.into_partial_reported(inner_delta));
                        } else {
                            // New key not yet in state — use default with delta
                            // applied, mirroring the apply_delta pattern.
                            let mut new_val = V::default();
                            new_val.apply_delta(inner_delta);
                            reported
                                .insert(key.clone(), new_val.into_partial_reported(inner_delta));
                        }
                    }
                    Patch::Unset => {
                        // Unset entries cannot be represented in ReportedHashMap
                        // The user must handle explicit null reporting separately
                    }
                }
            }
        }
        ReportedHashMap(reported)
    }

    fn desired_cleanup(&self, delta: &Self::Delta) -> Option<Self::Delta> {
        if let Some(ref patches) = delta.0 {
            let mut result: HashMap<K, Patch<V::Delta>> = HashMap::new();
            let mut has_cleanup = false;

            for (key, patch) in patches.iter() {
                if let Patch::Set(inner_delta) = patch {
                    if let Some(v) = self.get(key) {
                        if let Some(inner_cleanup) = v.desired_cleanup(inner_delta) {
                            result.insert(key.clone(), Patch::Set(inner_cleanup));
                            has_cleanup = true;
                        }
                    }
                }
            }

            if has_cleanup {
                Some(DeltaHashMap(Some(result)))
            } else {
                None
            }
        } else {
            None
        }
    }
}

#[cfg(feature = "shadows_kv_persist")]
impl<K, V> KVPersist for HashMap<K, V>
where
    K: MapKey + Default + Hash,
    V: KVPersist + Default,
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
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K2,
    ) -> Result<LoadFieldResult, KvError<K2::Error>> {
        let mut result = LoadFieldResult::default();

        // Read manifest: prefix/__keys__
        let __saved_len = key_buf.len();
        let _ = key_buf.push_str("/__keys__");
        let manifest_key = key_buf.as_str().to_string();
        key_buf.truncate(__saved_len);

        let key_strings: Vec<String> =
            match kv.fetch_to_vec(&manifest_key).await.map_err(KvError::Kv)? {
                Some(data) => postcard::from_bytes(&data).map_err(|_| KvError::Serialization)?,
                None => {
                    result.defaulted += 1;
                    return Ok(result);
                }
            };

        for key_str in key_strings.iter() {
            let key: K = serde_json_core::from_str::<K>(&format!("\"{}\"", key_str))
                .map(|(k, _)| k)
                .map_err(|_| KvError::Serialization)?;

            let __saved_len = key_buf.len();
            let _ = key_buf.push_str("/");
            let _ = key_buf.push_str(key_str);

            let mut value = V::default();
            let inner = value.load_from_kv::<K2, KEY_LEN>(key_buf, kv).await?;
            result.merge(inner);
            key_buf.truncate(__saved_len);

            self.insert(key, value);
        }

        Ok(result)
    }

    async fn load_from_kv_with_migration<K2: KVStore, const KEY_LEN: usize>(
        &mut self,
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K2,
    ) -> Result<LoadFieldResult, KvError<K2::Error>> {
        self.load_from_kv::<K2, KEY_LEN>(key_buf, kv).await
    }

    async fn persist_to_kv<K2: KVStore, const KEY_LEN: usize>(
        &self,
        key_buf: &mut heapless::String<KEY_LEN>,
        kv: &K2,
    ) -> Result<(), KvError<K2::Error>> {
        let mut key_strings: Vec<String> = Vec::new();

        for (key, value) in self.iter() {
            let key_str = format!("{}", key);
            key_strings.push(key_str.clone());

            let __saved_len = key_buf.len();
            let _ = key_buf.push_str("/");
            let _ = key_buf.push_str(&key_str);
            value.persist_to_kv::<K2, KEY_LEN>(key_buf, kv).await?;
            key_buf.truncate(__saved_len);
        }

        // Write manifest
        let __saved_len = key_buf.len();
        let _ = key_buf.push_str("/__keys__");
        let bytes = postcard::to_allocvec(&key_strings).map_err(|_| KvError::Serialization)?;
        kv.store(key_buf.as_str(), &bytes)
            .await
            .map_err(KvError::Kv)?;
        key_buf.truncate(__saved_len);

        Ok(())
    }

    async fn persist_delta<K2: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K2,
        key_buf: &mut heapless::String<KEY_LEN>,
    ) -> Result<(), KvError<K2::Error>> {
        if let Some(ref patches) = delta.0 {
            // Read manifest
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str("/__keys__");
            let mut key_strings: Vec<String> = match kv
                .fetch_to_vec(key_buf.as_str())
                .await
                .map_err(KvError::Kv)?
            {
                Some(data) => postcard::from_bytes(&data).unwrap_or_default(),
                None => Vec::new(),
            };
            key_buf.truncate(__saved_len);

            for (key, patch) in patches.iter() {
                let key_str = format!("{}", key);

                match patch {
                    Patch::Set(d) => {
                        let __saved_len = key_buf.len();
                        let _ = key_buf.push_str("/");
                        let _ = key_buf.push_str(&key_str);
                        V::persist_delta::<K2, KEY_LEN>(d, kv, key_buf).await?;
                        key_buf.truncate(__saved_len);

                        if !key_strings.iter().any(|k| k == &key_str) {
                            key_strings.push(key_str);
                        }
                    }
                    Patch::Unset => {
                        let __saved_len = key_buf.len();
                        let _ = key_buf.push_str("/");
                        let _ = key_buf.push_str(&key_str);
                        kv.remove(key_buf.as_str()).await.map_err(KvError::Kv)?;

                        let _ = key_buf.push_str("/");
                        let _ = kv
                            .remove_if(key_buf.as_str(), |_| true)
                            .await
                            .map_err(KvError::Kv)?;
                        key_buf.truncate(__saved_len);

                        key_strings.retain(|k| k != &key_str);
                    }
                }
            }

            // Write updated manifest
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str("/__keys__");
            let bytes = postcard::to_allocvec(&key_strings).map_err(|_| KvError::Serialization)?;
            kv.store(key_buf.as_str(), &bytes)
                .await
                .map_err(KvError::Kv)?;
            key_buf.truncate(__saved_len);
        }

        Ok(())
    }

    const FIELD_COUNT: usize = 1; // the __keys__ manifest; map entries are dynamic

    fn is_valid_key(rel_key: &str) -> bool {
        rel_key == "/__keys__"
    }

    fn is_valid_prefix(rel_key: &str) -> bool {
        // Entry data: /...
        rel_key.starts_with("/")
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
        let delta = DeltaHashMap(Some(patches));

        map.apply_delta(&delta);
        assert_eq!(map.get("a"), Some(&42));
    }

    #[test]
    fn test_hashmap_apply_delta_unset() {
        let mut map: HashMap<String, u32> = HashMap::new();
        map.insert("a".to_string(), 42);

        let mut patches = HashMap::new();
        patches.insert("a".to_string(), Patch::<u32>::Unset);
        let delta = DeltaHashMap(Some(patches));

        map.apply_delta(&delta);
        assert!(map.get("a").is_none());
    }

    #[test]
    fn test_hashmap_apply_delta_none() {
        let mut map: HashMap<String, u32> = HashMap::new();
        map.insert("a".to_string(), 42);

        let delta = DeltaHashMap::<String, u32>(None);
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
            let mut key_buf = heapless::String::<128>::new();
            let _ = key_buf.push_str("test");
            map.persist_to_kv::<FileKVStore, 128>(&mut key_buf, &kv)
                .await
                .unwrap();

            // Load
            let mut loaded: HashMap<String, u32> = HashMap::new();
            let mut key_buf = heapless::String::<128>::new();
            let _ = key_buf.push_str("test");
            let result = loaded
                .load_from_kv::<FileKVStore, 128>(&mut key_buf, &kv)
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
            let mut key_buf = heapless::String::<128>::new();
            let _ = key_buf.push_str("test");
            map.persist_to_kv::<FileKVStore, 128>(&mut key_buf, &kv)
                .await
                .unwrap();

            // Delta: add "b", remove "a"
            let mut patches = HashMap::new();
            patches.insert("b".to_string(), Patch::Set(2u32));
            patches.insert("a".to_string(), Patch::<u32>::Unset);
            let delta = DeltaHashMap(Some(patches));

            let mut key_buf = heapless::String::<128>::new();
            let _ = key_buf.push_str("test");
            <HashMap<String, u32> as KVPersist>::persist_delta::<FileKVStore, 128>(
                &delta,
                &kv,
                &mut key_buf,
            )
            .await
            .unwrap();

            // Load and verify
            let mut loaded: HashMap<String, u32> = HashMap::new();
            let mut key_buf = heapless::String::<128>::new();
            let _ = key_buf.push_str("test");
            loaded
                .load_from_kv::<FileKVStore, 128>(&mut key_buf, &kv)
                .await
                .unwrap();

            assert!(loaded.get("a").is_none());
            assert_eq!(loaded.get("b"), Some(&2));
        }
    }
}

// =============================================================================
// Flatten Tests
// =============================================================================

#[cfg(test)]
mod flatten_tests {
    use crate::shadows::{NullResolver, ShadowNode};
    use rustot_derive::{shadow_node, shadow_root};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    #[shadow_node]
    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
    struct ConnectorConfig {
        pub enabled: bool,
    }

    #[shadow_root(name = "displays")]
    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
    struct DisplaysShadow {
        pub brightness: u8,
        #[shadow_attr(flatten)]
        pub connectors: HashMap<String, ConnectorConfig>,
    }

    #[tokio::test]
    async fn test_flatten_parse_delta_mixed_fields() {
        let json =
            br#"{"brightness": 50, "HDMI-A-1": {"enabled": true}, "DP-1": {"enabled": false}}"#;
        let delta = DisplaysShadow::parse_delta(json, "", &NullResolver)
            .await
            .unwrap();

        // Normal field parsed
        assert_eq!(delta.brightness, Some(50));

        // Flattened entries collected into connectors delta
        let patches = delta.connectors.as_ref().unwrap().0.as_ref().unwrap();
        assert_eq!(patches.len(), 2);
    }

    #[tokio::test]
    async fn test_flatten_parse_delta_only_normal() {
        let json = br#"{"brightness": 80}"#;
        let delta = DisplaysShadow::parse_delta(json, "", &NullResolver)
            .await
            .unwrap();

        assert_eq!(delta.brightness, Some(80));
        // No remaining keys → connectors delta is None
        assert!(delta.connectors.is_none());
    }

    #[tokio::test]
    async fn test_flatten_parse_delta_only_flattened() {
        let json = br#"{"HDMI-A-1": {"enabled": true}}"#;
        let delta = DisplaysShadow::parse_delta(json, "", &NullResolver)
            .await
            .unwrap();

        assert!(delta.brightness.is_none());
        let patches = delta.connectors.as_ref().unwrap().0.as_ref().unwrap();
        assert_eq!(patches.len(), 1);
    }

    #[tokio::test]
    async fn test_flatten_parse_delta_null() {
        let json = b"null";
        let delta = DisplaysShadow::parse_delta(json, "", &NullResolver)
            .await
            .unwrap();

        assert!(delta.brightness.is_none());
        assert!(delta.connectors.is_none());
    }

    #[test]
    fn test_flatten_apply_delta() {
        let mut state = DisplaysShadow {
            brightness: 50,
            connectors: HashMap::new(),
        };

        // Build a delta with both normal and flattened updates
        let mut patches = HashMap::new();
        patches.insert(
            "HDMI-A-1".to_string(),
            crate::shadows::data_types::Patch::Set(DeltaConnectorConfig {
                enabled: Some(true),
            }),
        );
        let delta = DeltaDisplaysShadow {
            brightness: Some(80),
            connectors: Some(super::DeltaHashMap(Some(patches))),
        };

        state.apply_delta(&delta);
        assert_eq!(state.brightness, 80);
        assert_eq!(
            state.connectors.get("HDMI-A-1"),
            Some(&ConnectorConfig { enabled: true })
        );
    }

    #[test]
    fn test_flatten_into_reported() {
        let state = DisplaysShadow {
            brightness: 50,
            connectors: {
                let mut m = HashMap::new();
                m.insert("HDMI-A-1".to_string(), ConnectorConfig { enabled: true });
                m
            },
        };

        let reported = state.into_reported();
        assert_eq!(reported.brightness, Some(50));
        let connector_reported = &reported.connectors.as_ref().unwrap().0["HDMI-A-1"];
        assert_eq!(connector_reported.enabled, Some(true));
    }

    #[test]
    fn test_flatten_reported_serializes_flat() {
        let state = DisplaysShadow {
            brightness: 50,
            connectors: {
                let mut m = HashMap::new();
                m.insert("HDMI-A-1".to_string(), ConnectorConfig { enabled: true });
                m
            },
        };

        let reported = state.into_reported();
        let json = serde_json::to_string(&reported).unwrap();

        // Should NOT contain "connectors" as a nested key
        assert!(!json.contains("\"connectors\""));
        // Should contain flat entries
        assert!(json.contains("\"brightness\""));
        assert!(json.contains("\"HDMI-A-1\""));
        assert!(json.contains("\"enabled\""));
    }

    #[test]
    fn test_flatten_delta_serializes_flat() {
        let mut patches = HashMap::new();
        patches.insert(
            "DP-1".to_string(),
            crate::shadows::data_types::Patch::Set(DeltaConnectorConfig {
                enabled: Some(false),
            }),
        );

        let delta = DeltaDisplaysShadow {
            brightness: Some(60),
            connectors: Some(super::DeltaHashMap(Some(patches))),
        };

        let json = serde_json::to_string(&delta).unwrap();
        // Should NOT contain "connectors" as a nested key
        assert!(!json.contains("\"connectors\""));
        // Should contain flat entries
        assert!(json.contains("\"brightness\""));
        assert!(json.contains("\"DP-1\""));
    }

    #[test]
    fn test_flatten_into_partial_reported() {
        let state = DisplaysShadow {
            brightness: 50,
            connectors: {
                let mut m = HashMap::new();
                m.insert("HDMI-A-1".to_string(), ConnectorConfig { enabled: true });
                m.insert("DP-1".to_string(), ConnectorConfig { enabled: false });
                m
            },
        };

        // Delta only changes brightness — connectors should be None in partial reported
        let delta = DeltaDisplaysShadow {
            brightness: Some(80),
            connectors: None,
        };

        let partial = state.into_partial_reported(&delta);
        assert_eq!(partial.brightness, Some(50));
        assert!(partial.connectors.is_none());
    }
}
