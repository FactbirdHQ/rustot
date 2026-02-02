//! ShadowNode implementations for heapless container types.
//!
//! - `heapless::String<N>` — opaque leaf type
//! - `heapless::Vec<T, N>` — opaque leaf type
//! - `heapless::LinearMap<K, V, N>` — map collection with per-entry Patch deltas
//!
//! All implementations are strictly no_std / no_alloc.

use crate::shadows::{fnv1a_hash, ReportedUnionFields, ShadowNode};
use serde::ser::SerializeMap;

#[cfg(feature = "shadows_kv_persist")]
use crate::shadows::{KVPersist, KVStore, KvError, LoadFieldResult, MapKey, MigrationSource};
#[cfg(feature = "shadows_kv_persist")]
use core::future::Future;
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

    fn apply_delta(&mut self, delta: &Self::Delta) {
        *self = delta.clone();
    }

    fn into_reported(self) -> Self::Reported {
        self
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
    const MAX_VALUE_LEN: usize = N + 5;

    fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
        &[]
    }

    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    fn apply_field_default(&mut self, _field_path: &str) -> bool {
        false
    }

    fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        async move {
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
    }

    fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        self.load_from_kv::<K, KEY_LEN>(prefix, kv)
    }

    fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let mut buf = [0u8; N + 5];
            let bytes = postcard::to_slice(self, &mut buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        prefix: &str,
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let mut buf = [0u8; N + 5];
            let bytes = postcard::to_slice(delta, &mut buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
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

    fn apply_delta(&mut self, delta: &Self::Delta) {
        *self = delta.clone();
    }

    fn into_reported(self) -> Self::Reported {
        self
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

#[cfg(feature = "shadows_kv_persist")]
#[allow(incomplete_features)]
impl<T, const N: usize> KVPersist for heapless::Vec<T, N>
where
    T: Clone + Default + Serialize + DeserializeOwned + MaxSize,
    [(); N * T::POSTCARD_MAX_SIZE + 5]:,
{
    const MAX_KEY_LEN: usize = 0;
    const MAX_VALUE_LEN: usize = N * T::POSTCARD_MAX_SIZE + 5;

    fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
        &[]
    }

    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    fn apply_field_default(&mut self, _field_path: &str) -> bool {
        false
    }

    fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        async move {
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
    }

    fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        self.load_from_kv::<K, KEY_LEN>(prefix, kv)
    }

    fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let mut buf = [0u8; N * T::POSTCARD_MAX_SIZE + 5];
            let bytes = postcard::to_slice(self, &mut buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        prefix: &str,
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let mut buf = [0u8; N * T::POSTCARD_MAX_SIZE + 5];
            let bytes = postcard::to_slice(delta, &mut buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
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
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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
    K: Clone + Eq + Default + serde::Serialize + serde::de::DeserializeOwned,
    V: ShadowNode,
{
    type Delta = LinearMapDelta<K, V::Delta, N>;
    type Reported = LinearMapReported<K, V::Reported, N>;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"heapless::LinearMap");

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

    fn into_reported(self) -> Self::Reported {
        let mut reported = heapless::LinearMap::new();
        for (k, v) in self.into_iter() {
            let _ = reported.insert(k, v.into_reported());
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

/// Manifest for a LinearMap: a postcard-serialized `heapless::Vec<K, N>` of the active keys.
///
/// Stored at `prefix/__keys__`.
///
/// Using the actual key type K directly avoids Display/parse round-trips and
/// keeps everything fully no_std/no_alloc via postcard + heapless.
#[cfg(feature = "shadows_kv_persist")]
#[allow(incomplete_features)]
impl<K, V, const N: usize> KVPersist for heapless::LinearMap<K, V, N>
where
    K: MapKey + Default + Serialize + DeserializeOwned,
    V: KVPersist,
    [(); N * (K::MAX_KEY_DISPLAY_LEN + 5) + 5]:,
{
    // "/{key}" + sub-key length
    const MAX_KEY_LEN: usize = 1 + K::MAX_KEY_DISPLAY_LEN + V::MAX_KEY_LEN;

    // Max of manifest size or value size.
    //
    // Manifest upper bound: postcard-serialized Vec<K, N>.
    // Each K serializes to at most MAX_KEY_DISPLAY_LEN + 5 bytes (postcard string).
    // Vec overhead: varint length prefix (≤5 bytes).
    const MAX_VALUE_LEN: usize = {
        const fn const_max(a: usize, b: usize) -> usize {
            if a > b {
                a
            } else {
                b
            }
        }
        const_max(N * (K::MAX_KEY_DISPLAY_LEN + 5) + 5, V::MAX_VALUE_LEN)
    };

    fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
        &[]
    }

    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    fn apply_field_default(&mut self, _field_path: &str) -> bool {
        false
    }

    fn load_from_kv<K2: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K2,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K2::Error>>> {
        async move {
            let mut result = LoadFieldResult::default();

            let manifest_key = build_key::<KEY_LEN>(prefix, "/__keys__");

            let mut buf = [0u8; N * (K::MAX_KEY_DISPLAY_LEN + 5) + 5];
            let keys: heapless::Vec<K, N> = match kv
                .fetch(&manifest_key, &mut buf)
                .await
                .map_err(KvError::Kv)?
            {
                Some(data) => postcard::from_bytes(data).map_err(|_| KvError::Serialization)?,
                None => {
                    result.defaulted += 1;
                    return Ok(result);
                }
            };

            for key in keys.iter() {
                let entry_prefix = build_entry_prefix::<KEY_LEN>(prefix, key);

                let mut value = V::default();
                let inner = value.load_from_kv::<K2, KEY_LEN>(&entry_prefix, kv).await?;
                result.merge(inner);

                let _ = self.insert(key.clone(), value);
            }

            Ok(result)
        }
    }

    fn load_from_kv_with_migration<K2: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K2,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K2::Error>>> {
        self.load_from_kv::<K2, KEY_LEN>(prefix, kv)
    }

    fn persist_to_kv<K2: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K2,
    ) -> impl Future<Output = Result<(), KvError<K2::Error>>> {
        async move {
            let mut manifest_keys: heapless::Vec<K, N> = heapless::Vec::new();

            for (key, value) in self.iter() {
                let entry_prefix = build_entry_prefix::<KEY_LEN>(prefix, key);
                value
                    .persist_to_kv::<K2, KEY_LEN>(&entry_prefix, kv)
                    .await?;
                let _ = manifest_keys.push(key.clone());
            }

            // Write manifest
            let manifest_key = build_key::<KEY_LEN>(prefix, "/__keys__");
            let mut buf = [0u8; N * (K::MAX_KEY_DISPLAY_LEN + 5) + 5];
            let bytes =
                postcard::to_slice(&manifest_keys, &mut buf).map_err(|_| KvError::Serialization)?;
            kv.store(&manifest_key, bytes).await.map_err(KvError::Kv)?;

            Ok(())
        }
    }

    fn persist_delta<K2: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K2,
        prefix: &str,
    ) -> impl Future<Output = Result<(), KvError<K2::Error>>> {
        async move {
            if let Some(ref patches) = delta.0 {
                let manifest_key = build_key::<KEY_LEN>(prefix, "/__keys__");

                // Load existing manifest
                let mut buf = [0u8; N * (K::MAX_KEY_DISPLAY_LEN + 5) + 5];
                let mut manifest_keys: heapless::Vec<K, N> = match kv
                    .fetch(&manifest_key, &mut buf)
                    .await
                    .map_err(KvError::Kv)?
                {
                    Some(data) => postcard::from_bytes(data).unwrap_or_default(),
                    None => heapless::Vec::new(),
                };

                for (key, patch) in patches.iter() {
                    let entry_prefix = build_entry_prefix::<KEY_LEN>(prefix, key);

                    match patch {
                        Patch::Set(d) => {
                            V::persist_delta::<K2, KEY_LEN>(d, kv, &entry_prefix).await?;

                            // Add to manifest if not present
                            if !manifest_keys.iter().any(|k| k == key) {
                                let _ = manifest_keys.push(key.clone());
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

                            // Remove from manifest
                            manifest_keys.retain(|k| k != key);
                        }
                    }
                }

                // Write updated manifest
                let mut wbuf = [0u8; N * (K::MAX_KEY_DISPLAY_LEN + 5) + 5];
                let bytes = postcard::to_slice(&manifest_keys, &mut wbuf)
                    .map_err(|_| KvError::Serialization)?;
                kv.store(&manifest_key, bytes).await.map_err(KvError::Kv)?;
            }

            Ok(())
        }
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        let manifest_key = build_key::<KEY_LEN>(prefix, "/__keys__");
        keys(&manifest_key);
    }

    fn collect_valid_prefixes<const KEY_LEN: usize>(prefix: &str, prefixes: &mut impl FnMut(&str)) {
        let mut pfx = heapless::String::<KEY_LEN>::new();
        let _ = pfx.push_str(prefix);
        let _ = pfx.push_str("/");
        prefixes(&pfx);
    }
}

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

    #[test]
    fn test_linear_map_into_reported() {
        let mut map: heapless::LinearMap<heapless::String<4>, u32, 4> = heapless::LinearMap::new();
        let _ = map.insert(heapless::String::try_from("a").unwrap(), 42);
        let _ = map.insert(heapless::String::try_from("b").unwrap(), 99);

        let reported = map.into_reported();
        assert_eq!(
            reported
                .0
                .get(&heapless::String::<4>::try_from("a").unwrap()),
            Some(&42)
        );
        assert_eq!(
            reported
                .0
                .get(&heapless::String::<4>::try_from("b").unwrap()),
            Some(&99)
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
}
