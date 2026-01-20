//! Key-value store abstraction for shadow persistence
//!
//! This module provides the `KVStore` trait and implementations:
//! - `SequentialKVStore`: For embedded systems using NOR flash
//! - `FileKVStore`: For std environments (testing, desktop) - requires `std` feature
//! - `NoPersist`: Zero-cost no-op for non-persisted shadows

mod no_persist;
mod sequential;

#[cfg(feature = "std")]
mod file;

pub use no_persist::NoPersist;
pub use sequential::SequentialKVStore;

#[cfg(feature = "std")]
pub use file::FileKVStore;

use core::fmt::Debug;

/// A key-value store for persisting shadow state.
///
/// Keys are strings in the format `"prefix/path/to/field"` where:
/// - `prefix` is the shadow name (e.g., "device", "network")
/// - `/path/to/field` is the field path within the shadow (always starts with `/`)
///
/// Full key example: `"device/config/timeout"` = prefix `"device"` + path `"/config/timeout"`
///
/// Values are opaque byte slices (typically postcard-encoded).
///
/// ## Design Note
///
/// This trait is intentionally "dumb" - it knows nothing about shadows, schemas, or
/// migrations. All shadow-aware logic belongs on the `Shadow` struct, not here.
///
/// ## Interior Mutability
///
/// All methods take `&self` (not `&mut self`) because implementations provide interior
/// mutability via `Mutex`. This allows multiple `Shadow` instances to share a single
/// `KVStore` via `&'a K` references without requiring `Arc` or `alloc`.
///
/// Example usage:
/// ```ignore
/// static KV: StaticCell<SequentialKVStore<Flash, NoopRawMutex>> = StaticCell::new();
/// let kv = KV.init(SequentialKVStore::new(flash, range));
///
/// // Multiple shadows share the same KVStore
/// let mut device_shadow = Shadow::<DeviceShadow, _>::new(kv);
/// let mut network_shadow = Shadow::<NetworkShadow, _>::new(kv);
/// ```
pub trait KVStore {
    /// Error type for KV operations
    type Error: Debug;

    /// Fetch a value by key.
    ///
    /// Returns `Ok(Some(slice))` if found, where `slice` is the value data within `buf`.
    /// Returns `Ok(None)` if the key doesn't exist.
    /// Returns `Err` on I/O or other errors.
    ///
    /// Note: The returned slice borrows from `buf` but may not start at index 0.
    /// Always use the returned slice directly rather than assuming the data location.
    async fn fetch<'a>(
        &self,
        key: &str,
        buf: &'a mut [u8],
    ) -> Result<Option<&'a [u8]>, Self::Error>;

    /// Store a value by key.
    ///
    /// Overwrites any existing value for this key.
    async fn store(&self, key: &str, value: &[u8]) -> Result<(), Self::Error>;

    /// Remove a key-value pair.
    ///
    /// Does nothing if the key doesn't exist.
    async fn remove(&self, key: &str) -> Result<(), Self::Error>;

    /// Remove all keys with given prefix that match the predicate.
    ///
    /// Returns the number of keys removed.
    ///
    /// Each implementation optimizes for its storage backend:
    /// - `FileKVStore`: Uses `Vec` to collect keys, removes in single pass (has alloc)
    /// - `SequentialKVStore`: Uses 4-key buffer with loop (handles flash constraints)
    ///
    /// Used by `Shadow::commit()` to clean up orphaned keys.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Remove all keys except those in valid_keys
    /// let removed = kv.remove_if("device", |key| {
    ///     !valid_keys.contains(key)
    /// }).await?;
    /// ```
    async fn remove_if<F>(&self, prefix: &str, predicate: F) -> Result<usize, Self::Error>
    where
        F: FnMut(&str) -> bool;
}
