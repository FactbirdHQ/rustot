//! Commit types for OTA-safe schema transitions
//!
//! The `commit()` operation is called after a successful boot to finalize
//! schema changes and clean up orphaned keys from previous firmware versions.

/// Result of a Shadow::commit() operation.
///
/// Provides statistics about the commit process, useful for logging.
#[derive(Debug, Default, Clone)]
pub struct CommitStats {
    /// Number of orphaned keys removed (keys not in current schema)
    pub orphans_removed: usize,

    /// Whether the schema hash was updated in KV storage
    pub schema_hash_updated: bool,
}

impl CommitStats {
    /// True if any cleanup work was done
    pub fn had_cleanup(&self) -> bool {
        self.orphans_removed > 0
    }
}
