//! File-based KVStore implementation for std environments
//!
//! Uses the filesystem for storage. Each key is stored as a separate file
//! with base64-encoded filename to handle path separators in keys.

use std::path::PathBuf;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use super::{KVStore, StateStore};
use crate::shadows::commit::CommitStats;
use crate::shadows::error::KvError;
use crate::shadows::migration::LoadResult;
use crate::shadows::KVPersist;

/// Suffix for schema hash keys.
const SCHEMA_HASH_SUFFIX: &str = "/__schema_hash__";

/// Error type for FileKVStore operations
#[derive(Debug)]
pub enum FileKVStoreError {
    /// I/O error
    Io(std::io::Error),
    /// Key encoding error
    KeyEncoding,
}

impl From<std::io::Error> for FileKVStoreError {
    fn from(e: std::io::Error) -> Self {
        FileKVStoreError::Io(e)
    }
}

impl std::fmt::Display for FileKVStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileKVStoreError::Io(e) => write!(f, "I/O error: {}", e),
            FileKVStoreError::KeyEncoding => write!(f, "Key encoding error"),
        }
    }
}

impl std::error::Error for FileKVStoreError {}

/// File-based KV store for testing and desktop environments.
///
/// Each key is stored as a separate file with base64url-encoded filename.
/// Uses `tokio::sync::Mutex` for interior mutability.
///
/// # Key Encoding
///
/// Keys like `"device/config/timeout"` are base64url-encoded to produce
/// valid filenames: `"ZGV2aWNlL2NvbmZpZy90aW1lb3V0"`.
///
/// # Example
///
/// ```ignore
/// let kv = FileKVStore::new("/tmp/shadow_test");
/// let mut shadow = Shadow::<DeviceShadow, _>::new(&kv);
/// ```
pub struct FileKVStore {
    /// Base directory for storing key files
    base_path: PathBuf,
    /// Mutex for interior mutability (allows &self methods)
    _mutex: Mutex<()>,
}

impl FileKVStore {
    /// Create a new FileKVStore at the given directory.
    ///
    /// Creates the directory if it doesn't exist.
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        let base_path = base_path.into();
        Self {
            base_path,
            _mutex: Mutex::new(()),
        }
    }

    /// Create a new FileKVStore in a unique temporary directory.
    ///
    /// Useful for testing. The directory is created immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if the temporary directory cannot be created.
    pub fn temp() -> Result<Self, FileKVStoreError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let tmp_dir = std::env::temp_dir().join(format!(
            "kv_shadow_test_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            id
        ));

        std::fs::create_dir_all(&tmp_dir)?;

        Ok(Self {
            base_path: tmp_dir,
            _mutex: Mutex::new(()),
        })
    }

    /// Get the base path of the store.
    pub fn base_path(&self) -> &std::path::Path {
        &self.base_path
    }

    /// Initialize the store, creating the directory if needed.
    pub async fn init(&self) -> Result<(), FileKVStoreError> {
        fs::create_dir_all(&self.base_path).await?;
        Ok(())
    }

    /// Encode a key to a filesystem-safe filename.
    fn encode_key(key: &str) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        URL_SAFE_NO_PAD.encode(key.as_bytes())
    }

    /// Decode a filename back to a key.
    fn decode_key(filename: &str) -> Option<String> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        URL_SAFE_NO_PAD
            .decode(filename)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }

    /// Get the file path for a key.
    fn key_path(&self, key: &str) -> PathBuf {
        self.base_path.join(Self::encode_key(key))
    }
}

// =============================================================================
// KVStore Implementation (byte-level operations)
// =============================================================================

impl KVStore for FileKVStore {
    type Error = FileKVStoreError;

    async fn fetch<'a>(
        &self,
        key: &str,
        buf: &'a mut [u8],
    ) -> Result<Option<&'a [u8]>, Self::Error> {
        let path = self.key_path(key);

        match fs::File::open(&path).await {
            Ok(mut file) => {
                let len = file.read(buf).await?;
                Ok(Some(&buf[..len]))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn fetch_to_vec(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        let path = self.key_path(key);

        match fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn store(&self, key: &str, value: &[u8]) -> Result<(), Self::Error> {
        // Ensure directory exists
        fs::create_dir_all(&self.base_path).await?;

        let path = self.key_path(key);
        let mut file = fs::File::create(&path).await?;
        file.write_all(value).await?;
        file.sync_all().await?;
        Ok(())
    }

    async fn remove(&self, key: &str) -> Result<(), Self::Error> {
        let path = self.key_path(key);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn remove_if<F>(&self, prefix: &str, mut predicate: F) -> Result<usize, Self::Error>
    where
        F: FnMut(&str) -> bool,
    {
        let mut removed = 0;

        // Collect matching keys first
        let mut keys_to_remove = Vec::new();

        let mut read_dir = match fs::read_dir(&self.base_path).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };

        while let Some(entry) = read_dir.next_entry().await? {
            if let Some(filename) = entry.file_name().to_str() {
                if let Some(key) = Self::decode_key(filename) {
                    if key.starts_with(prefix) && predicate(&key) {
                        keys_to_remove.push(key);
                    }
                }
            }
        }

        // Remove collected keys
        for key in keys_to_remove {
            let path = self.key_path(&key);
            match fs::remove_file(&path).await {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }

        Ok(removed)
    }
}

// =============================================================================
// StateStore Implementation (state-level operations)
// =============================================================================

impl<St: KVPersist> StateStore<St> for FileKVStore {
    type Error = FileKVStoreError;

    async fn get_state(&self, prefix: &str) -> Result<St, Self::Error> {
        let mut state = St::default();
        let _ = state
            .load_from_kv::<Self, 128>(prefix, self)
            .await
            .map_err(|e| match e {
                KvError::Kv(kv_err) => kv_err,
                _ => FileKVStoreError::KeyEncoding,
            })?;
        Ok(state)
    }

    async fn set_state(&self, prefix: &str, state: &St) -> Result<(), Self::Error> {
        state
            .persist_to_kv::<Self, 128>(prefix, self)
            .await
            .map_err(|e| match e {
                KvError::Kv(kv_err) => kv_err,
                _ => FileKVStoreError::KeyEncoding,
            })
    }

    async fn apply_delta(&self, prefix: &str, delta: &St::Delta) -> Result<St, Self::Error> {
        St::persist_delta::<Self, 128>(delta, self, prefix)
            .await
            .map_err(|e| match e {
                KvError::Kv(kv_err) => kv_err,
                _ => FileKVStoreError::KeyEncoding,
            })?;
        self.get_state(prefix).await
    }

    async fn load(
        &self,
        prefix: &str,
        hash: u64,
    ) -> Result<LoadResult<St>, KvError<Self::Error>> {
        // Build hash key: prefix + SCHEMA_HASH_SUFFIX
        let hash_key = format!("{}{}", prefix, SCHEMA_HASH_SUFFIX);

        let mut hash_buf = [0u8; 8];
        match self.fetch(&hash_key, &mut hash_buf).await.map_err(KvError::Kv)? {
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
                        .load_from_kv::<Self, 128>(prefix, self)
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
                        .load_from_kv_with_migration::<Self, 128>(prefix, self)
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

    async fn commit(
        &self,
        prefix: &str,
        hash: u64,
    ) -> Result<CommitStats, KvError<Self::Error>> {
        // Build set of valid keys for O(1) lookup during GC
        let mut valid: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Collect all valid keys using per-field codegen
        St::collect_valid_keys::<128>(prefix, &mut |key| {
            // Strip prefix to get relative key for comparison
            let rel_key = key.strip_prefix(prefix).unwrap_or(key);
            valid.insert(rel_key.to_string());
        });

        // Remove orphaned keys using KVStore::remove_if
        let orphans_removed = self
            .remove_if(prefix, |key| {
                let rel_key = key.strip_prefix(prefix).unwrap_or(key);
                !valid.contains(rel_key) && !rel_key.starts_with("/__")
            })
            .await
            .map_err(KvError::Kv)?;

        // Update schema hash to current
        let hash_key = format!("{}{}", prefix, SCHEMA_HASH_SUFFIX);

        self.store(&hash_key, &hash.to_le_bytes())
            .await
            .map_err(KvError::Kv)?;

        Ok(CommitStats {
            orphans_removed,
            schema_hash_updated: true,
        })
    }
}
