//! Error types for multi-shadow manager operations.

use std::fmt;

/// Errors that can occur during multi-shadow manager operations.
#[derive(Debug)]
pub enum MultiShadowError {
    /// MQTT communication error.
    Mqtt(String),

    /// JSON serialization/deserialization error.
    Serialization(serde_json::Error),

    /// File I/O error during persistence operations.
    Io(std::io::Error),

    /// Shadow operation was rejected by AWS IoT.
    ShadowRejected {
        /// Error code from AWS IoT.
        code: u16,
        /// Error message from AWS IoT.
        message: String,
    },

    /// Operation timed out waiting for response.
    Timeout,

    /// Invalid shadow document format.
    InvalidDocument(String),

    /// Shadow not found.
    ShadowNotFound,

    /// Shadow discovery error (AWS IoT data plane).
    DiscoveryError(Box<dyn std::error::Error + Send + Sync>),

    /// Invalid topic format.
    InvalidTopic(String),

    /// Storage error from state store.
    StorageError(String),

    /// Shadow ID not managed by this manager.
    ShadowNotManaged(String),
}

impl fmt::Display for MultiShadowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MultiShadowError::Mqtt(err) => write!(f, "MQTT error: {}", err),
            MultiShadowError::Serialization(err) => write!(f, "Serialization error: {}", err),
            MultiShadowError::Io(err) => write!(f, "File I/O error: {}", err),
            MultiShadowError::ShadowRejected { code, message } => {
                write!(f, "Shadow operation rejected ({}): {}", code, message)
            }
            MultiShadowError::Timeout => write!(f, "Operation timeout"),
            MultiShadowError::InvalidDocument(msg) => write!(f, "Invalid shadow document: {}", msg),
            MultiShadowError::DiscoveryError(err) => write!(f, "Shadow discovery error: {}", err),
            MultiShadowError::ShadowNotFound => write!(f, "Shadow not found"),
            MultiShadowError::InvalidTopic(topic) => write!(f, "Invalid topic format: {}", topic),
            MultiShadowError::StorageError(msg) => write!(f, "Storage error: {}", msg),
            MultiShadowError::ShadowNotManaged(id) => write!(f, "Shadow ID '{}' not managed", id),
        }
    }
}

impl std::error::Error for MultiShadowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MultiShadowError::Serialization(err) => Some(err),
            MultiShadowError::Io(err) => Some(err),
            MultiShadowError::DiscoveryError(err) => Some(&**err),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for MultiShadowError {
    fn from(err: serde_json::Error) -> Self {
        MultiShadowError::Serialization(err)
    }
}

impl From<std::io::Error> for MultiShadowError {
    fn from(err: std::io::Error) -> Self {
        MultiShadowError::Io(err)
    }
}

/// Result type for multi-shadow manager operations.
pub type MultiShadowResult<T> = Result<T, MultiShadowError>;
