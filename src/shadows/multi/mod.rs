//! Multi-shadow manager for runtime-named AWS IoT Device Shadows.
//!
//! This module provides [`MultiShadowManager`] for managing multiple named shadows
//! with pattern-based filtering and efficient wildcard MQTT subscriptions.
//!
//! # Key Features
//!
//! - **Runtime shadow names**: Unlike [`Shadow`](super::Shadow) which uses compile-time
//!   names, `MultiShadowManager` supports runtime-named shadows (e.g., "flow-pump-01",
//!   "flow-valve-02")
//!
//! - **Efficient subscriptions**: Single wildcard subscription for all shadows matching
//!   a pattern instead of one subscription per shadow
//!
//! - **Generic MQTT**: Works with any [`MqttClient`](crate::mqtt::MqttClient) implementation
//!   (Greengrass IPC, rumqttc, etc.)
//!
//! # Example
//!
//! ```rust,ignore
//! use rustot::shadows::multi::{MultiShadowManager, MultiShadowRoot};
//! use rustot::shadows::ShadowNode;
//! use rustot::shadows::rustot_derive::shadow_node;
//! use rustot::shadows::store::FileKVStore;
//! use serde::{Serialize, Deserialize};
//!
//! #[shadow_node]
//! #[derive(Debug, Clone, Default, Serialize, Deserialize)]
//! pub struct FlowState {
//!     pub flow_rate: f64,
//!     pub temperature: f64,
//!     pub active: bool,
//! }
//!
//! impl MultiShadowRoot for FlowState {
//!     const PATTERN: &'static str = "flow-";
//! }
//!
//! async fn run(mqtt: impl rustot::mqtt::MqttClient) {
//!     let store = FileKVStore::new("/data/shadows".into());
//!     let manager = MultiShadowManager::<FlowState, _, _>::new(
//!         mqtt,
//!         "my-device".into(),
//!         store,
//!     );
//!
//!     // Discover existing shadows matching the pattern
//!     manager.initialize().await.unwrap();
//!
//!     // Wait for delta from ANY managed shadow
//!     loop {
//!         let (shadow_id, state, delta) = manager.wait_delta().await.unwrap();
//!         println!("Shadow {} updated", shadow_id);
//!     }
//! }
//! ```

mod error;
mod manager;

pub use error::{MultiShadowError, MultiShadowResult};
pub use manager::MultiShadowManager;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::ShadowNode;

/// Trait for shadow types used with [`MultiShadowManager`] (runtime naming).
///
/// This is similar to [`ShadowRoot`](super::ShadowRoot) but uses `PATTERN` instead
/// of `NAME` to support runtime-named shadows like "flow-pump-01", "flow-valve-02".
///
/// # Implementation
///
/// Types implementing `MultiShadowRoot` must also implement `ShadowNode`
/// (typically via `#[shadow_node]` derive) and `KVPersist` for storage.
///
/// ```rust,ignore
/// use rustot::shadows::multi::MultiShadowRoot;
/// use rustot::shadows::{ShadowNode, KVPersist};
/// use rustot::shadows::rustot_derive::shadow_node;
/// use serde::{Serialize, Deserialize};
///
/// #[shadow_node]
/// #[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// pub struct FlowState {
///     pub flow_rate: f64,
///     pub temperature: f64,
///     pub active: bool,
/// }
///
/// impl MultiShadowRoot for FlowState {
///     const PATTERN: &'static str = "flow-";
/// }
/// ```
///
/// # Topic Format
///
/// Full shadow names are constructed as `PATTERN + id`:
/// - Pattern: "flow-"
/// - ID: "pump-01"
/// - Full name: "flow-pump-01"
/// - Delta topic: `$aws/things/{thing}/shadow/name/flow-pump-01/update/delta`
/// - Wildcard: `$aws/things/{thing}/shadow/name/+/update/delta`
pub trait MultiShadowRoot: ShadowNode + Serialize + DeserializeOwned + Clone + Send + Sync {
    /// Shadow pattern prefix (e.g., "flow-" matches "flow-pump-01", "flow-valve-02").
    ///
    /// The full shadow name is constructed as `PATTERN + id`.
    const PATTERN: &'static str;

    /// AWS IoT topic prefix (default: "$aws").
    const PREFIX: &'static str = "$aws";

    /// Maximum payload size for shadow updates (default: 512 bytes).
    const MAX_PAYLOAD_SIZE: usize = 512;
}
