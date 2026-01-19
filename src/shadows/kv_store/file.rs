//! File-based KVStore implementation for std environments
//!
//! Uses the filesystem for storage. Each key is stored as a separate file
//! with base64-encoded filename to handle path separators in keys.

use std::path::PathBuf;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use super::KVStore;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    async fn setup_test_kv() -> FileKVStore {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let tmp_dir = env::temp_dir().join(format!(
            "kv_test_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            id
        ));
        let kv = FileKVStore::new(&tmp_dir);
        kv.init().await.unwrap();
        kv
    }

    async fn cleanup_test_kv(kv: &FileKVStore) {
        let _ = fs::remove_dir_all(&kv.base_path).await;
    }

    #[tokio::test]
    async fn test_file_kv_basic_operations() {
        let kv = setup_test_kv().await;

        // Store
        kv.store("device/timeout", &[0x88, 0x13, 0x00, 0x00])
            .await
            .unwrap();

        // Fetch
        let mut buf = [0u8; 16];
        let value = kv.fetch("device/timeout", &mut buf).await.unwrap();
        assert_eq!(value, Some([0x88, 0x13, 0x00, 0x00].as_slice()));

        // Fetch non-existent
        let value = kv.fetch("device/nonexistent", &mut buf).await.unwrap();
        assert!(value.is_none());

        // Remove
        kv.remove("device/timeout").await.unwrap();
        let value = kv.fetch("device/timeout", &mut buf).await.unwrap();
        assert!(value.is_none());

        cleanup_test_kv(&kv).await;
    }

    #[tokio::test]
    async fn test_file_kv_remove_if() {
        let kv = setup_test_kv().await;

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

        // Verify
        let mut buf = [0u8; 4];
        assert!(kv.fetch("device/a", &mut buf).await.unwrap().is_none());
        assert!(kv.fetch("device/b", &mut buf).await.unwrap().is_none());
        assert!(kv.fetch("device/c", &mut buf).await.unwrap().is_some());
        assert!(kv.fetch("network/x", &mut buf).await.unwrap().is_some());

        cleanup_test_kv(&kv).await;
    }

    #[tokio::test]
    async fn test_file_kv_key_encoding() {
        let kv = setup_test_kv().await;

        // Keys with special characters
        let key = "device/config/nested/deep/field";
        kv.store(key, &[42]).await.unwrap();

        let mut buf = [0u8; 4];
        let value = kv.fetch(key, &mut buf).await.unwrap();
        assert_eq!(value, Some([42].as_slice()));

        cleanup_test_kv(&kv).await;
    }
}
