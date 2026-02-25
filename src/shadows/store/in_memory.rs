//! In-memory StateStore implementation for non-persisted shadows.
//!
//! Provides an in-memory implementation that holds the shadow state directly.
//! State is returned by cloning on access, enabling the stateless Shadow design.

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;

use super::{ApplyJsonError, StateStore};
use crate::shadows::commit::CommitStats;
use crate::shadows::error::KvError;
use crate::shadows::migration::LoadResult;
use crate::shadows::{ShadowNode, VariantResolver};

/// An in-memory store that holds the shadow state directly.
///
/// This implementation stores the shadow state in a `Mutex` and returns clones
/// on access via `get_state()`. No serialization occurs - state mutations use
/// `ShadowNode::apply_delta()` directly.
///
/// ## Memory Model
///
/// Unlike persistent stores where state lives in flash/file and Shadow holds
/// no state, `InMemory<S>` holds the authoritative state copy. This is the
/// RAM-efficient design for non-persisted shadows.
///
/// ## Usage
///
/// ```ignore
/// let store = InMemory::<DeviceShadow>::new();
/// let shadow = Shadow::new(&store, &mqtt);
/// shadow.load().await?;
///
/// // State access is async (clones from InMemory's Mutex)
/// let state = shadow.state().await?;
/// ```
pub struct InMemory<S> {
    state: Mutex<NoopRawMutex, Option<S>>,
}

impl<S: Default> InMemory<S> {
    /// Create a new InMemory store with no initial state.
    ///
    /// State is initialized to `None` and will be set to `S::default()`
    /// when `load()` is called.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(None),
        }
    }

    /// Create a new InMemory store with the given initial state.
    pub fn with_state(state: S) -> Self {
        Self {
            state: Mutex::new(Some(state)),
        }
    }
}

impl<S: Default> Default for InMemory<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: ShadowNode> StateStore<S> for InMemory<S> {
    type Error = core::convert::Infallible;

    async fn get_state(&self, _prefix: &str) -> Result<S, Self::Error> {
        let guard = self.state.lock().await;
        Ok(guard.clone().unwrap_or_default())
    }

    async fn set_state(&self, _prefix: &str, state: &S) -> Result<(), Self::Error> {
        *self.state.lock().await = Some(state.clone());
        Ok(())
    }

    async fn apply_delta(&self, _prefix: &str, delta: &S::Delta) -> Result<S, Self::Error> {
        let mut guard = self.state.lock().await;
        let state = guard.get_or_insert_with(S::default);
        state.apply_delta(delta); // Pure mutation, no serialization!
        Ok(state.clone())
    }

    async fn load(&self, _prefix: &str, _hash: u64) -> Result<LoadResult<S>, KvError<Self::Error>> {
        // No migration for in-memory - just return the current state or default
        let mut guard = self.state.lock().await;
        match guard.as_ref() {
            Some(s) => Ok(LoadResult {
                first_boot: false,
                schema_changed: false,
                fields_loaded: 0,
                fields_migrated: 0,
                fields_defaulted: 0,
                state: s.clone(),
            }),
            None => {
                let state = S::default();
                *guard = Some(state.clone());
                Ok(LoadResult {
                    first_boot: true,
                    schema_changed: false,
                    fields_loaded: 0,
                    fields_migrated: 0,
                    fields_defaulted: 0,
                    state,
                })
            }
        }
    }

    async fn commit(&self, _prefix: &str, _hash: u64) -> Result<CommitStats, KvError<Self::Error>> {
        // No-op for in-memory - nothing to commit
        Ok(CommitStats::default())
    }

    fn resolver<'a>(&'a self, _prefix: &'a str) -> impl VariantResolver + 'a {
        InMemoryResolver { store: self }
    }

    async fn apply_json_delta(
        &self,
        prefix: &str,
        json: &[u8],
    ) -> Result<S::Delta, ApplyJsonError<Self::Error>> {
        let resolver = self.resolver(prefix);
        let delta = S::parse_delta(json, "", &resolver)
            .await
            .map_err(ApplyJsonError::Parse)?;
        self.apply_delta(prefix, &delta)
            .await
            .map_err(ApplyJsonError::Store)?;
        Ok(delta)
    }
}

/// Resolver for InMemory that uses the current in-memory state.
struct InMemoryResolver<'a, S> {
    store: &'a InMemory<S>,
}

impl<S: ShadowNode> VariantResolver for InMemoryResolver<'_, S> {
    async fn resolve(&self, path: &str) -> Option<heapless::String<32>> {
        let guard = self.store.state.lock().await;
        guard.as_ref().and_then(|s| s.variant_at_path(path))
    }
}
