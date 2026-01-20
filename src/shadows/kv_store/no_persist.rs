//! No-op KVStore implementation for non-persisted shadows.
//!
//! Provides a zero-cost implementation for shadows that don't need persistence,
//! useful for testing or volatile state that shouldn't survive reboots.

use super::KVStore;

/// A no-op KVStore for non-persisted shadows.
///
/// All operations are no-ops that return success. This enables the same Shadow
/// code path to work for both persisted and non-persisted use cases with zero
/// runtime overhead (NoPersist is a zero-sized type).
///
/// ## Usage
///
/// ```ignore
/// // Non-persisted shadow for testing or volatile state
/// let mut shadow = KvShadow::<DeviceShadow>::new_in_memory();
/// shadow.load().await?;  // Initializes with defaults, nothing persisted
/// ```
pub struct NoPersist;

impl KVStore for NoPersist {
    type Error = core::convert::Infallible;

    async fn fetch<'a>(
        &self,
        _key: &str,
        _buf: &'a mut [u8],
    ) -> Result<Option<&'a [u8]>, Self::Error> {
        Ok(None) // Always "not found" - triggers first-boot behavior
    }

    async fn store(&self, _key: &str, _value: &[u8]) -> Result<(), Self::Error> {
        Ok(()) // No-op
    }

    async fn remove(&self, _key: &str) -> Result<(), Self::Error> {
        Ok(()) // No-op
    }

    async fn remove_if<F>(&self, _prefix: &str, _predicate: F) -> Result<usize, Self::Error>
    where
        F: FnMut(&str) -> bool,
    {
        Ok(0) // No keys to remove
    }
}
