//! MQTT abstraction traits for AWS IoT services.
//!
//! This module provides a unified MQTT interface compatible with:
//! - `embedded-mqtt` (no_std/embassy) - zero-copy, no allocation
//! - `rumqttc` (std/tokio) - standard library with tokio
//! - `greengrass-ipc-rust` (std/tokio) - AWS Greengrass IPC
//!
//! # Design Constraints
//! - **no_std**: No standard library dependency in core traits
//! - **no_alloc**: No heap allocation required
//! - **Interior mutability**: `&self` methods for shared access across tasks
//! - **Const generics**: Fixed-size topic subscriptions
//! - **Associated types**: No trait objects or boxing

use core::fmt::Debug;
use core::future::Future;

mod embedded;

#[cfg(feature = "rumqttc")]
mod rumqttc;

#[cfg(feature = "greengrass")]
mod greengrass;

pub use embedded::*;

#[cfg(feature = "rumqttc")]
pub use self::rumqttc::*;

#[cfg(feature = "greengrass")]
pub use self::greengrass::*;

/// Newtype wrapper for implementing service-specific traits
/// (OTA, Shadows, etc.) atop any MqttClient implementation.
pub struct Mqtt<C>(pub C);

/// MQTT Quality of Service levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QoS {
    /// At most once delivery (fire and forget).
    #[default]
    AtMostOnce,
    /// At least once delivery (acknowledged delivery).
    AtLeastOnce,
    /// Exactly once delivery (assured delivery).
    ExactlyOnce,
}

/// Options for publishing an MQTT message.
#[derive(Debug, Clone, Default)]
pub struct PublishOptions {
    /// Quality of Service level.
    pub qos: QoS,
    /// Whether the message should be retained by the broker.
    pub retain: bool,
    /// Duplicate delivery flag.
    pub dup: bool,
}

impl PublishOptions {
    /// Create new publish options with defaults (QoS 0, no retain, no dup).
    pub const fn new() -> Self {
        Self {
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
        }
    }

    /// Set the QoS level.
    pub const fn qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    /// Set the retain flag.
    pub const fn retain(mut self, retain: bool) -> Self {
        self.retain = retain;
        self
    }

    /// Set the duplicate flag.
    pub const fn dup(mut self, dup: bool) -> Self {
        self.dup = dup;
        self
    }
}

/// A received MQTT message.
pub trait MqttMessage {
    /// Get the topic name of the message.
    fn topic_name(&self) -> &str;

    /// Get the message payload as bytes.
    fn payload(&self) -> &[u8];

    /// Get mutable access to the message payload.
    fn payload_mut(&mut self) -> &mut [u8];

    /// Get the QoS level of the message.
    fn qos(&self) -> QoS;

    /// Check if this is a duplicate message.
    fn dup(&self) -> bool;

    /// Check if this message was retained.
    fn retain(&self) -> bool;
}

/// An active MQTT subscription that yields messages.
pub trait MqttSubscription {
    /// The message type returned by this subscription.
    type Message<'m>: MqttMessage
    where
        Self: 'm;

    /// Error type for subscription operations.
    type Error: Debug;

    /// Wait for and return the next message.
    ///
    /// Returns `None` if the subscription has been closed.
    fn next_message(&mut self) -> impl Future<Output = Option<Self::Message<'_>>>;

    /// Unsubscribe and close this subscription.
    fn unsubscribe(self) -> impl Future<Output = Result<(), Self::Error>>;
}

/// Error when encoding a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadError {
    /// Buffer too small for the payload.
    BufferSize,
    /// Encoding/serialization failed.
    EncodingFailed,
}

/// Payload encoding trait supporting deferred serialization.
///
/// The `encode` method receives the transmit buffer directly, allowing
/// lazy serialization (like [`DeferredPayload`]) to write directly into it
/// without intermediate copies.
pub trait ToPayload {
    /// Maximum size this payload could serialize to.
    fn max_size(&self) -> usize;

    /// Serialize into the provided buffer.
    ///
    /// Returns the number of bytes written on success.
    fn encode(&self, buf: &mut [u8]) -> Result<usize, PayloadError>;
}

impl ToPayload for &[u8] {
    fn max_size(&self) -> usize {
        self.len()
    }

    fn encode(&self, buf: &mut [u8]) -> Result<usize, PayloadError> {
        if buf.len() < self.len() {
            return Err(PayloadError::BufferSize);
        }
        buf[..self.len()].copy_from_slice(self);
        Ok(self.len())
    }
}

impl<const N: usize> ToPayload for &[u8; N] {
    fn max_size(&self) -> usize {
        N
    }

    fn encode(&self, buf: &mut [u8]) -> Result<usize, PayloadError> {
        if buf.len() < N {
            return Err(PayloadError::BufferSize);
        }
        buf[..N].copy_from_slice(*self);
        Ok(N)
    }
}

/// Deferred payload that serializes lazily into the transmit buffer.
///
/// This is the key optimization: no intermediate buffer needed. The closure
/// is called with the actual transmit buffer, allowing direct serialization.
///
/// # Example
///
/// ```ignore
/// let payload = DeferredPayload::new(
///     |buf| serde_json_core::to_slice(&data, buf)
///         .map_err(|_| PayloadError::EncodingFailed),
///     512, // max size
/// );
/// mqtt.publish("topic", payload).await;
/// ```
pub struct DeferredPayload<F> {
    func: F,
    max_len: usize,
}

impl<F> DeferredPayload<F> {
    /// Create a new deferred payload.
    ///
    /// # Arguments
    /// * `func` - Closure that serializes into the provided buffer
    /// * `max_len` - Maximum size the serialized payload could be
    pub const fn new(func: F, max_len: usize) -> Self {
        Self { func, max_len }
    }
}

impl<F: Fn(&mut [u8]) -> Result<usize, PayloadError>> ToPayload for DeferredPayload<F> {
    fn max_size(&self) -> usize {
        self.max_len
    }

    fn encode(&self, buf: &mut [u8]) -> Result<usize, PayloadError> {
        if buf.len() < self.max_len {
            return Err(PayloadError::BufferSize);
        }
        (self.func)(buf)
    }
}

/// The main MQTT client trait.
///
/// Provides publish/subscribe operations with interior mutability (`&self`)
/// for sharing across async tasks.
pub trait MqttClient {
    /// Subscription type returned by [`MqttClient::subscribe`].
    type Subscription<'m, const N: usize>: MqttSubscription
    where
        Self: 'm;

    /// Error type for client operations.
    type Error: Debug;

    /// Get the client ID.
    fn client_id(&self) -> &str;

    /// Wait until the client is connected to the broker.
    fn wait_connected(&self) -> impl Future<Output = ()>;

    /// Publish a message with default options (QoS 0, no retain).
    fn publish<P: ToPayload>(
        &self,
        topic: &str,
        payload: P,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        self.publish_with_options(topic, payload, PublishOptions::new())
    }

    /// Publish a message with custom options.
    fn publish_with_options<P: ToPayload>(
        &self,
        topic: &str,
        payload: P,
        options: PublishOptions,
    ) -> impl Future<Output = Result<(), Self::Error>>;

    /// Subscribe to multiple topics.
    ///
    /// Returns a subscription that yields incoming messages matching
    /// any of the subscribed topics.
    fn subscribe<const N: usize>(
        &self,
        topics: &[(&str, QoS); N],
    ) -> impl Future<Output = Result<Self::Subscription<'_, N>, Self::Error>>;
}
