//! SequentialKVStore implementation using sequential-storage crate (v7.x)
//!
//! Uses MapStorage for flash-based key-value storage.
//! Interior mutability via embassy_sync::Mutex allows sharing between multiple Shadow instances.

use core::ops::Range;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embedded_storage_async::nor_flash::{MultiwriteNorFlash, NorFlash};
use heapless::index_set::FnvIndexSet;
use heapless::String;
use sequential_storage::cache::{KeyCacheImpl, NoCache};
use sequential_storage::map::{MapConfig, MapStorage};

use super::{ApplyJsonError, KVStore, StateStore};
use crate::shadows::commit::CommitStats;
use crate::shadows::error::KvError;
use crate::shadows::migration::LoadResult;
use crate::shadows::{KVPersist, VariantResolver};

/// Suffix for schema hash keys.
const SCHEMA_HASH_SUFFIX: &str = "/__schema_hash__";

/// KVStore error type for sequential storage
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SequentialKVStoreError<E> {
    /// Key too long for buffer
    KeyTooLong,
    /// Sequential storage error
    Storage(sequential_storage::Error<E>),
}

impl<E> From<sequential_storage::Error<E>> for SequentialKVStoreError<E> {
    fn from(e: sequential_storage::Error<E>) -> Self {
        SequentialKVStoreError::Storage(e)
    }
}

/// A KVStore implementation backed by NOR flash using sequential-storage v7.x.
///
/// This is suitable for embedded systems with limited RAM. Keys are stored
/// as `heapless::String<MAX_KEY_LEN>` to avoid allocation.
///
/// ## Interior Mutability
///
/// Uses `embassy_sync::Mutex<M, MapStorage>` for interior mutability, allowing multiple
/// `Shadow` instances to share a single `SequentialKVStore` via `&` references.
///
/// # Type Parameters
/// - `S`: The flash storage type (must implement `NorFlash` and `MultiwriteNorFlash`)
/// - `M`: The mutex type (e.g., `NoopRawMutex`, `CriticalSectionRawMutex`)
/// - `C`: The cache type (use `NoCache` for minimal RAM)
/// - `MAX_KEY_LEN`: Maximum key length (default 128 bytes)
///
/// # Mutex Type Selection
///
/// | Mutex Type | Use Case |
/// |------------|----------|
/// | `NoopRawMutex` | Single async executor, no preemption |
/// | `CriticalSectionRawMutex` | ISR/task interaction possible |
/// | `ThreadModeRawMutex` | Only accessed from thread mode |
pub struct SequentialKVStore<
    S: NorFlash,
    M: RawMutex,
    C: KeyCacheImpl<String<MAX_KEY_LEN>> = NoCache,
    const MAX_KEY_LEN: usize = 128,
> {
    inner: Mutex<M, MapStorage<String<MAX_KEY_LEN>, S, C>>,
}

impl<S: NorFlash, M: RawMutex, const MAX_KEY_LEN: usize>
    SequentialKVStore<S, M, NoCache, MAX_KEY_LEN>
{
    /// Create a new SequentialKVStore with no cache.
    ///
    /// # Arguments
    /// - `flash`: The NOR flash instance
    /// - `flash_range`: The byte range within flash to use for storage
    pub fn new(flash: S, flash_range: Range<u32>) -> Self {
        let config = MapConfig::new(flash_range);
        let map = MapStorage::new(flash, config, NoCache::new());
        Self {
            inner: Mutex::new(map),
        }
    }
}

impl<S: NorFlash, M: RawMutex, C: KeyCacheImpl<String<MAX_KEY_LEN>>, const MAX_KEY_LEN: usize>
    SequentialKVStore<S, M, C, MAX_KEY_LEN>
{
    /// Create a new SequentialKVStore with a custom cache.
    ///
    /// # Arguments
    /// - `flash`: The NOR flash instance
    /// - `flash_range`: The byte range within flash to use for storage
    /// - `cache`: The cache implementation
    pub fn new_with_cache(flash: S, flash_range: Range<u32>, cache: C) -> Self {
        let config = MapConfig::new(flash_range);
        let map = MapStorage::new(flash, config, cache);
        Self {
            inner: Mutex::new(map),
        }
    }

    /// Convert a string key to a heapless::String, returning error if too long.
    fn to_heapless_key(key: &str) -> Result<String<MAX_KEY_LEN>, SequentialKVStoreError<S::Error>> {
        String::try_from(key).map_err(|_| SequentialKVStoreError::KeyTooLong)
    }
}

// =============================================================================
// KVStore Implementation (byte-level operations)
// =============================================================================

impl<
        S: NorFlash + MultiwriteNorFlash,
        M: RawMutex,
        C: KeyCacheImpl<String<MAX_KEY_LEN>>,
        const MAX_KEY_LEN: usize,
    > KVStore for SequentialKVStore<S, M, C, MAX_KEY_LEN>
{
    type Error = SequentialKVStoreError<S::Error>;

    async fn fetch<'a>(
        &self,
        key: &str,
        buf: &'a mut [u8],
    ) -> Result<Option<&'a [u8]>, Self::Error> {
        let key = Self::to_heapless_key(key)?;
        let mut map = self.inner.lock().await;

        match map.fetch_item::<&[u8]>(buf, &key).await {
            Ok(value) => Ok(value),
            Err(e) => Err(e.into()),
        }
    }

    async fn store(&self, key: &str, value: &[u8]) -> Result<(), Self::Error> {
        let key = Self::to_heapless_key(key)?;
        let mut map = self.inner.lock().await;
        let mut scratch = [0u8; 512]; // Scratch buffer for store operation

        map.store_item(&mut scratch, &key, &value).await?;
        Ok(())
    }

    async fn remove(&self, key: &str) -> Result<(), Self::Error> {
        let key = Self::to_heapless_key(key)?;
        let mut map = self.inner.lock().await;
        let mut scratch = [0u8; 512];

        map.remove_item(&mut scratch, &key).await?;
        Ok(())
    }

    async fn remove_if<F>(&self, prefix: &str, mut predicate: F) -> Result<usize, Self::Error>
    where
        F: FnMut(&str) -> bool,
    {
        let mut removed = 0;

        // Use 4-key buffer with loop for correctness (handles any number of orphans)
        loop {
            let mut to_remove: heapless::Vec<String<MAX_KEY_LEN>, 4> = heapless::Vec::new();

            // Scan for keys matching prefix + predicate
            {
                let mut map = self.inner.lock().await;
                let mut buf = [0u8; 512];

                // Track seen keys since iterator returns duplicates (old versions)
                let mut seen = FnvIndexSet::<String<MAX_KEY_LEN>, 64>::new();

                // Use fetch_all_items to iterate
                let mut iter = map.fetch_all_items(&mut buf).await?;

                while let Ok(Some((key, _value))) = iter.next::<&[u8]>(&mut buf).await {
                    if key.as_str().starts_with(prefix) {
                        let _ = seen.insert(key);
                    }
                }

                // Check predicate for each unique key
                for key in &seen {
                    if predicate(key.as_str()) && to_remove.push(key.clone()).is_err() {
                        break; // Buffer full, will remove these and loop again
                    }
                }
            } // map lock released here

            if to_remove.is_empty() {
                break; // No more keys to remove
            }

            // Remove collected keys (re-acquire lock)
            {
                let mut map = self.inner.lock().await;
                let mut scratch = [0u8; 512];
                for key in to_remove {
                    map.remove_item(&mut scratch, &key).await?;
                    removed += 1;
                }
            }
        }

        Ok(removed)
    }
}

// =============================================================================
// StateStore Implementation (state-level operations)
// =============================================================================

impl<
        St: KVPersist,
        S: NorFlash + MultiwriteNorFlash,
        M: RawMutex,
        C: KeyCacheImpl<String<MAX_KEY_LEN>>,
        const MAX_KEY_LEN: usize,
    > StateStore<St> for SequentialKVStore<S, M, C, MAX_KEY_LEN>
{
    type Error = SequentialKVStoreError<S::Error>;

    async fn get_state(&self, prefix: &str) -> Result<St, Self::Error> {
        let mut state = St::default();
        let _ = state
            .load_from_kv::<Self, MAX_KEY_LEN>(prefix, self)
            .await
            .map_err(|e| match e {
                KvError::Kv(kv_err) => kv_err,
                _ => SequentialKVStoreError::KeyTooLong,
            })?;
        Ok(state)
    }

    async fn set_state(&self, prefix: &str, state: &St) -> Result<(), Self::Error> {
        state
            .persist_to_kv::<Self, MAX_KEY_LEN>(prefix, self)
            .await
            .map_err(|e| match e {
                KvError::Kv(kv_err) => kv_err,
                _ => SequentialKVStoreError::KeyTooLong,
            })
    }

    async fn apply_delta(&self, prefix: &str, delta: &St::Delta) -> Result<St, Self::Error> {
        St::persist_delta::<Self, MAX_KEY_LEN>(delta, self, prefix)
            .await
            .map_err(|e| match e {
                KvError::Kv(kv_err) => kv_err,
                _ => SequentialKVStoreError::KeyTooLong,
            })?;
        self.get_state(prefix).await
    }

    async fn load(&self, prefix: &str, hash: u64) -> Result<LoadResult<St>, KvError<Self::Error>> {
        // Build hash key: prefix + SCHEMA_HASH_SUFFIX
        let mut hash_key: heapless::String<128> = heapless::String::new();
        hash_key.push_str(prefix).map_err(|_| KvError::KeyTooLong)?;
        hash_key
            .push_str(SCHEMA_HASH_SUFFIX)
            .map_err(|_| KvError::KeyTooLong)?;

        let mut hash_buf = [0u8; 8];
        match self
            .fetch(&hash_key, &mut hash_buf)
            .await
            .map_err(KvError::Kv)?
        {
            None => {
                // First boot - no hash exists
                let state = St::default();
                self.set_state(prefix, &state).await.map_err(KvError::Kv)?;

                // Write schema hash
                self.store(&hash_key, &hash.to_le_bytes())
                    .await
                    .map_err(KvError::Kv)?;

                // Count fields
                let mut total_fields = 0usize;
                St::collect_valid_keys::<128>(prefix, &mut |_| {
                    total_fields += 1;
                });

                Ok(LoadResult {
                    state,
                    first_boot: true,
                    schema_changed: false,
                    fields_loaded: 0,
                    fields_migrated: 0,
                    fields_defaulted: total_fields,
                })
            }
            Some(slice) if slice.len() == 8 => {
                let stored_hash = u64::from_le_bytes(slice.try_into().unwrap());
                if stored_hash == hash {
                    // Hash matches - normal load
                    let mut state = St::default();
                    let field_result = state
                        .load_from_kv::<Self, MAX_KEY_LEN>(prefix, self)
                        .await?;

                    Ok(LoadResult {
                        state,
                        first_boot: false,
                        schema_changed: false,
                        fields_loaded: field_result.loaded,
                        fields_migrated: 0,
                        fields_defaulted: field_result.defaulted,
                    })
                } else {
                    // Hash mismatch - migration needed
                    let mut state = St::default();
                    let field_result = state
                        .load_from_kv_with_migration::<Self, MAX_KEY_LEN>(prefix, self)
                        .await?;

                    Ok(LoadResult {
                        state,
                        first_boot: false,
                        schema_changed: true,
                        fields_loaded: field_result.loaded,
                        fields_migrated: field_result.migrated,
                        fields_defaulted: field_result.defaulted,
                    })
                }
            }
            Some(_) => {
                // Invalid hash length - treat as first boot
                let state = St::default();
                self.set_state(prefix, &state).await.map_err(KvError::Kv)?;

                self.store(&hash_key, &hash.to_le_bytes())
                    .await
                    .map_err(KvError::Kv)?;

                let mut total_fields = 0usize;
                St::collect_valid_keys::<128>(prefix, &mut |_| {
                    total_fields += 1;
                });

                Ok(LoadResult {
                    state,
                    first_boot: true,
                    schema_changed: false,
                    fields_loaded: 0,
                    fields_migrated: 0,
                    fields_defaulted: total_fields,
                })
            }
        }
    }

    async fn commit(&self, prefix: &str, hash: u64) -> Result<CommitStats, KvError<Self::Error>> {
        // Build set of valid keys for O(1) lookup during GC
        let mut valid: FnvIndexSet<heapless::String<128>, 128> = FnvIndexSet::new();

        // Collect all valid keys using per-field codegen
        St::collect_valid_keys::<128>(prefix, &mut |key| {
            // Strip prefix to get relative key for comparison
            let rel_key = key.strip_prefix(prefix).unwrap_or(key);
            let mut hs: heapless::String<128> = heapless::String::new();
            let _ = hs.push_str(rel_key);
            let _ = valid.insert(hs);
        });

        // Collect valid prefixes for dynamic collections (maps)
        let mut valid_prefixes: FnvIndexSet<heapless::String<128>, 16> = FnvIndexSet::new();
        St::collect_valid_prefixes::<128>(prefix, &mut |pfx| {
            let rel = pfx.strip_prefix(prefix).unwrap_or(pfx);
            let mut hs: heapless::String<128> = heapless::String::new();
            let _ = hs.push_str(rel);
            let _ = valid_prefixes.insert(hs);
        });

        // Remove orphaned keys using KVStore::remove_if
        let orphans_removed = self
            .remove_if(prefix, |key| {
                let rel_key = key.strip_prefix(prefix).unwrap_or(key);
                let mut rel_key_string: heapless::String<128> = heapless::String::new();
                let _ = rel_key_string.push_str(rel_key);
                !valid.contains(&rel_key_string)
                    && !valid_prefixes
                        .iter()
                        .any(|pfx| rel_key.starts_with(pfx.as_str()))
                    && !rel_key.starts_with("/__")
            })
            .await
            .map_err(KvError::Kv)?;

        // Update schema hash to current
        let mut hash_key: heapless::String<128> = heapless::String::new();
        hash_key.push_str(prefix).map_err(|_| KvError::KeyTooLong)?;
        hash_key
            .push_str(SCHEMA_HASH_SUFFIX)
            .map_err(|_| KvError::KeyTooLong)?;

        self.store(&hash_key, &hash.to_le_bytes())
            .await
            .map_err(KvError::Kv)?;

        Ok(CommitStats {
            orphans_removed,
            schema_hash_updated: true,
        })
    }

    fn resolver<'a>(&'a self, prefix: &'a str) -> impl VariantResolver + 'a {
        KvResolver::<Self, MAX_KEY_LEN> { kv: self, prefix }
    }

    async fn apply_json_delta(
        &self,
        prefix: &str,
        json: &[u8],
    ) -> Result<St::Delta, ApplyJsonError<Self::Error>> {
        let resolver = <Self as StateStore<St>>::resolver(self, prefix);
        let delta = St::parse_delta(json, "", &resolver)
            .await
            .map_err(ApplyJsonError::Parse)?;

        // Persist delta directly (avoids method dispatch ambiguity)
        St::persist_delta::<Self, MAX_KEY_LEN>(&delta, self, prefix)
            .await
            .map_err(|e| match e {
                KvError::Kv(kv_err) => ApplyJsonError::Store(kv_err),
                _ => ApplyJsonError::Store(SequentialKVStoreError::KeyTooLong),
            })?;

        Ok(delta)
    }
}

/// Resolver for KV stores that fetches `_variant` keys on demand.
struct KvResolver<'a, K, const MAX_KEY_LEN: usize> {
    kv: &'a K,
    prefix: &'a str,
}

impl<K: KVStore, const MAX_KEY_LEN: usize> VariantResolver for KvResolver<'_, K, MAX_KEY_LEN> {
    async fn resolve(&self, path: &str) -> Option<heapless::String<32>> {
        // Build the variant key: prefix/path/_variant
        // KV keys always have format {prefix}/{field_path} where field_path starts with /
        // So the full key is {prefix}/{path}/_variant (slash always present between components)
        let mut key: heapless::String<MAX_KEY_LEN> = heapless::String::new();
        if key.push_str(self.prefix).is_err() {
            return None;
        }
        if key.push('/').is_err() {
            return None;
        }
        if key.push_str(path).is_err() {
            return None;
        }
        if key.push_str("/_variant").is_err() {
            return None;
        }

        // Fetch the variant name from storage
        let mut buf = [0u8; 64];
        match self.kv.fetch(&key, &mut buf).await {
            Ok(Some(bytes)) => {
                // Try to parse as UTF-8 string
                core::str::from_utf8(bytes)
                    .ok()
                    .and_then(|s| heapless::String::try_from(s).ok())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use embedded_storage_async::nor_flash::{ErrorType, ReadNorFlash};

    /// Mock NorFlash implementation for testing.
    /// Uses a simple Vec<u8> as backing storage.
    struct MockFlash {
        data: Vec<u8>,
    }

    impl MockFlash {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0xFF; size], // Erased state
            }
        }

        const SECTOR_SIZE: usize = 4096;
    }

    #[derive(Debug)]
    struct MockFlashError;

    impl embedded_storage_async::nor_flash::NorFlashError for MockFlashError {
        fn kind(&self) -> embedded_storage_async::nor_flash::NorFlashErrorKind {
            embedded_storage_async::nor_flash::NorFlashErrorKind::Other
        }
    }

    impl ErrorType for MockFlash {
        type Error = MockFlashError;
    }

    impl ReadNorFlash for MockFlash {
        const READ_SIZE: usize = 1;

        async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            let end = start + bytes.len();
            if end > self.data.len() {
                return Err(MockFlashError);
            }
            bytes.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.data.len()
        }
    }

    impl NorFlash for MockFlash {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = Self::SECTOR_SIZE;

        async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            for (i, byte) in bytes.iter().enumerate() {
                // NOR flash can only clear bits (AND operation)
                self.data[start + i] &= byte;
            }
            Ok(())
        }

        async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            let start = from as usize;
            let end = to as usize;
            for byte in &mut self.data[start..end] {
                *byte = 0xFF; // Erased state
            }
            Ok(())
        }
    }

    impl MultiwriteNorFlash for MockFlash {}

    #[tokio::test]
    async fn test_sequential_kv_store_basic_operations() {
        let flash = MockFlash::new(16 * 1024); // 16KB
        let flash_range = 0..flash.capacity() as u32;
        let kv: SequentialKVStore<_, NoopRawMutex> = SequentialKVStore::new(flash, flash_range);

        // Store a value
        kv.store("device/timeout", &[0x88, 0x13, 0x00, 0x00])
            .await
            .unwrap();

        // Fetch it back
        let mut buf = [0u8; 128];
        let value = kv.fetch("device/timeout", &mut buf).await.unwrap();
        assert_eq!(value, Some([0x88, 0x13, 0x00, 0x00].as_slice()));
    }

    #[tokio::test]
    async fn test_sequential_kv_store_overwrite() {
        let flash = MockFlash::new(16 * 1024);
        let flash_range = 0..flash.capacity() as u32;
        let kv: SequentialKVStore<_, NoopRawMutex> = SequentialKVStore::new(flash, flash_range);

        // Store initial value
        kv.store("device/timeout", &[0x88, 0x13, 0x00, 0x00])
            .await
            .unwrap();

        // Overwrite with new value
        kv.store("device/timeout", &[0x10, 0x27, 0x00, 0x00])
            .await
            .unwrap();

        // Fetch should return the new value
        let mut buf = [0u8; 128];
        let value = kv.fetch("device/timeout", &mut buf).await.unwrap();
        assert_eq!(value, Some([0x10, 0x27, 0x00, 0x00].as_slice()));
    }

    #[tokio::test]
    async fn test_sequential_kv_store_remove() {
        let flash = MockFlash::new(16 * 1024);
        let flash_range = 0..flash.capacity() as u32;
        let kv: SequentialKVStore<_, NoopRawMutex> = SequentialKVStore::new(flash, flash_range);

        // Store and then remove
        kv.store("device/timeout", &[0x88, 0x13, 0x00, 0x00])
            .await
            .unwrap();
        kv.remove("device/timeout").await.unwrap();

        // Fetch should return None
        let mut buf = [0u8; 128];
        let value = kv.fetch("device/timeout", &mut buf).await.unwrap();
        assert!(value.is_none());
    }

    #[tokio::test]
    async fn test_sequential_kv_store_fetch_nonexistent() {
        let flash = MockFlash::new(16 * 1024);
        let flash_range = 0..flash.capacity() as u32;
        let kv: SequentialKVStore<_, NoopRawMutex> = SequentialKVStore::new(flash, flash_range);

        // Fetch a key that doesn't exist
        let mut buf = [0u8; 128];
        let value = kv.fetch("device/nonexistent", &mut buf).await.unwrap();
        assert!(value.is_none());
    }

    #[tokio::test]
    async fn test_sequential_kv_store_remove_if() {
        let flash = MockFlash::new(16 * 1024);
        let flash_range = 0..flash.capacity() as u32;
        let kv: SequentialKVStore<_, NoopRawMutex> = SequentialKVStore::new(flash, flash_range);

        // Store multiple keys
        kv.store("device/a", &[1]).await.unwrap();
        kv.store("device/b", &[2]).await.unwrap();
        kv.store("device/c", &[3]).await.unwrap();
        kv.store("network/x", &[4]).await.unwrap();

        // Remove device/a and device/b
        let removed = kv
            .remove_if("device", |key| key.ends_with("/a") || key.ends_with("/b"))
            .await
            .unwrap();

        assert_eq!(removed, 2);

        // Verify removals
        let mut buf = [0u8; 128];
        assert!(kv.fetch("device/a", &mut buf).await.unwrap().is_none());
        assert!(kv.fetch("device/b", &mut buf).await.unwrap().is_none());
        assert!(kv.fetch("device/c", &mut buf).await.unwrap().is_some());
        assert!(kv.fetch("network/x", &mut buf).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_sequential_kv_store_remove_if_none_match() {
        let flash = MockFlash::new(16 * 1024);
        let flash_range = 0..flash.capacity() as u32;
        let kv: SequentialKVStore<_, NoopRawMutex> = SequentialKVStore::new(flash, flash_range);

        kv.store("device/a", &[1]).await.unwrap();
        kv.store("device/b", &[2]).await.unwrap();

        // Predicate matches nothing
        let removed = kv.remove_if("device", |_key| false).await.unwrap();

        assert_eq!(removed, 0);

        // All keys should remain
        let mut buf = [0u8; 128];
        assert!(kv.fetch("device/a", &mut buf).await.unwrap().is_some());
        assert!(kv.fetch("device/b", &mut buf).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_sequential_kv_store_remove_if_all_match() {
        let flash = MockFlash::new(16 * 1024);
        let flash_range = 0..flash.capacity() as u32;
        let kv: SequentialKVStore<_, NoopRawMutex> = SequentialKVStore::new(flash, flash_range);

        kv.store("device/a", &[1]).await.unwrap();
        kv.store("device/b", &[2]).await.unwrap();

        // Remove all device keys
        let removed = kv.remove_if("device", |_key| true).await.unwrap();

        assert_eq!(removed, 2);

        // All device keys should be gone
        let mut buf = [0u8; 128];
        assert!(kv.fetch("device/a", &mut buf).await.unwrap().is_none());
        assert!(kv.fetch("device/b", &mut buf).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_sequential_kv_store_remove_if_many_keys() {
        let flash = MockFlash::new(32 * 1024); // Larger flash for more keys
        let flash_range = 0..flash.capacity() as u32;
        let kv: SequentialKVStore<_, NoopRawMutex> = SequentialKVStore::new(flash, flash_range);

        // Store more than 4 keys to test the loop behavior in remove_if
        for i in 0..8u8 {
            let key = format!("device/key{}", i);
            kv.store(&key, &[i]).await.unwrap();
        }

        // Remove all keys (tests the 4-key buffer loop)
        let removed = kv.remove_if("device", |_key| true).await.unwrap();

        assert_eq!(removed, 8);

        // Verify all are gone
        let mut buf = [0u8; 128];
        for i in 0..8 {
            let key = format!("device/key{}", i);
            assert!(kv.fetch(&key, &mut buf).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn test_sequential_kv_store_multiple_prefixes() {
        let flash = MockFlash::new(16 * 1024);
        let flash_range = 0..flash.capacity() as u32;
        let kv: SequentialKVStore<_, NoopRawMutex> = SequentialKVStore::new(flash, flash_range);

        // Store keys with different prefixes (simulating multiple shadows)
        kv.store("device/value", &[1]).await.unwrap();
        kv.store("network/value", &[2]).await.unwrap();
        kv.store("config/value", &[3]).await.unwrap();

        // Fetch each
        let mut buf = [0u8; 128];

        let value = kv.fetch("device/value", &mut buf).await.unwrap();
        assert_eq!(value, Some([1].as_slice()));

        let value = kv.fetch("network/value", &mut buf).await.unwrap();
        assert_eq!(value, Some([2].as_slice()));

        let value = kv.fetch("config/value", &mut buf).await.unwrap();
        assert_eq!(value, Some([3].as_slice()));
    }
}
