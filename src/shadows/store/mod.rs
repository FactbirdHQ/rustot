//! Shadow state storage abstraction
//!
//! This module provides storage traits and implementations for shadow state:
//!
//! ## Always Available
//! - `StateStore<S>`: State-level operations (get/set/apply_delta) - what Shadow uses
//! - `InMemory<S>`: Holds state in memory, pure Rust mutations, no serialization
//!
//! ## Feature: `shadows_kv_persist`
//! - `KVStore`: Byte-level KV operations (fetch/store/remove)
//! - `KVPersist`: Field-level persistence trait (implemented on shadow types)
//! - `SequentialKVStore`: Flash-based field-level storage
//! - `FileKVStore`: File-based field-level storage (also requires `std`)

mod in_memory;

#[cfg(feature = "shadows_kv_persist")]
mod sequential;

#[cfg(all(feature = "std", feature = "shadows_kv_persist"))]
mod file;

pub use in_memory::InMemory;

#[cfg(feature = "shadows_kv_persist")]
pub use sequential::SequentialKVStore;

#[cfg(all(feature = "std", feature = "shadows_kv_persist"))]
pub use file::FileKVStore;

use crate::shadows::{commit::CommitStats, error::KvError, migration::LoadResult, ShadowNode};
use core::fmt::Debug;

// =============================================================================
// StateStore Trait (always available)
// =============================================================================

/// State-level operations for shadow storage.
///
/// This is the primary storage trait that `Shadow` uses. It provides
/// state-level operations without exposing the underlying storage mechanism.
///
/// ## Storage Strategies
///
/// Different implementations offer different trade-offs:
///
/// | Store | Serialization | Best For |
/// |-------|--------------|----------|
/// | `InMemory<S>` | None | Testing, volatile state |
/// | `FlashBlobStore` | Postcard (whole state) | Simple embedded persistence |
/// | `SequentialKVStore` | Postcard (per-field) | Field-level updates, migrations |
///
/// ## Interior Mutability
///
/// All methods take `&self` (not `&mut self`) because implementations provide
/// interior mutability via `Mutex`. This allows multiple `Shadow` instances to
/// share a single store via `&'a K` references without requiring `Arc` or `alloc`.
pub trait StateStore<S: ShadowNode> {
    /// Error type for storage operations.
    type Error: Debug;

    /// Get the complete shadow state.
    ///
    /// - `InMemory<S>`: Returns a clone of the held state
    /// - Persistent stores: Reconstructs state from storage
    async fn get_state(&self, prefix: &str) -> Result<S, Self::Error>;

    /// Set the complete shadow state.
    ///
    /// - `InMemory<S>`: Replaces the held state with a clone
    /// - Persistent stores: Persists all fields to storage
    async fn set_state(&self, prefix: &str, state: &S) -> Result<(), Self::Error>;

    /// Apply a delta and return the updated state.
    ///
    /// This is the efficient path for delta updates:
    /// - `InMemory<S>`: Direct mutation via `apply_delta()`, no serialization
    /// - `SequentialKVStore`: Persists only changed fields, then reconstructs state
    ///
    /// Returns the updated state after applying the delta.
    async fn apply_delta(&self, prefix: &str, delta: &S::Delta) -> Result<S, Self::Error>;

    /// Load state from storage, handling first boot and schema migrations.
    ///
    /// ## Behavior
    ///
    /// | Scenario | Action |
    /// |----------|--------|
    /// | First boot (no hash) | Initialize defaults, persist all, write hash |
    /// | Hash matches | Load state from storage |
    /// | Hash mismatch | Load with migration, don't write hash |
    ///
    /// ## OTA Safety
    ///
    /// When `schema_changed` is true in the result, the new schema hash is NOT
    /// written. Call `commit()` after boot is confirmed successful to finalize.
    async fn load(&self, prefix: &str, hash: u64) -> Result<LoadResult<S>, KvError<Self::Error>>;

    /// Commit schema changes and clean up orphaned keys.
    ///
    /// Call this after boot is confirmed successful (e.g., after OTA image
    /// is marked as valid). This method:
    /// 1. Writes the new schema hash
    /// 2. Removes orphaned keys from old schema
    ///
    /// ## OTA Safety
    ///
    /// Until `commit()` is called, the old schema hash remains in storage.
    /// If the device reboots before `commit()` (e.g., OTA rollback), the old
    /// firmware will see its original hash and work correctly.
    async fn commit(&self, prefix: &str, hash: u64) -> Result<CommitStats, KvError<Self::Error>>;
}

// =============================================================================
// KVStore Trait (feature-gated)
// =============================================================================

/// A key-value store for persisting shadow state (byte-level operations).
///
/// This trait is only available with the `shadows_kv_persist` feature.
///
/// Keys are strings in the format `"prefix/path/to/field"` where:
/// - `prefix` is the shadow name (e.g., "device", "network")
/// - `/path/to/field` is the field path within the shadow (always starts with `/`)
///
/// Values are opaque byte slices (typically postcard-encoded).
///
/// ## Interior Mutability
///
/// All methods take `&self` (not `&mut self`) because implementations provide
/// interior mutability via `Mutex`. This allows multiple `Shadow` instances to
/// share a single `KVStore` via `&'a K` references without requiring `Arc`.
#[cfg(feature = "shadows_kv_persist")]
pub trait KVStore {
    /// Error type for KV operations
    type Error: Debug;

    /// Fetch a value by key.
    ///
    /// Returns `Ok(Some(slice))` if found, where `slice` is the value data within `buf`.
    /// Returns `Ok(None)` if the key doesn't exist.
    /// Returns `Err` on I/O or other errors.
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
    /// Used by `Shadow::commit()` to clean up orphaned keys.
    async fn remove_if<F>(&self, prefix: &str, predicate: F) -> Result<usize, Self::Error>
    where
        F: FnMut(&str) -> bool;
}

