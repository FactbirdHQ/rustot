//! Mock MQTT client for testing.
//!
//! This module provides a mock implementation of the MQTT traits
//! for testing shadows without requiring a real MQTT connection.

use core::convert::Infallible;
use core::future::Future;

use super::{MqttClient, MqttMessage, MqttSubscription, PublishOptions, QoS, ToPayload};

/// Mock MQTT client for testing.
///
/// Provides a simple MQTT client implementation that doesn't
/// actually connect to any broker. Useful for testing shadow
/// storage operations without cloud connectivity.
pub struct MockMqttClient {
    client_id: String,
}

impl MockMqttClient {
    /// Create a new mock MQTT client with the given client ID.
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
        }
    }
}

impl MqttClient for MockMqttClient {
    type Subscription<'m, const N: usize>
        = MockSubscription<N>
    where
        Self: 'm;
    type Error = Infallible;

    fn client_id(&self) -> &str {
        &self.client_id
    }

    fn wait_connected(&self) -> impl Future<Output = ()> {
        async {}
    }

    fn publish_with_options<P: ToPayload>(
        &self,
        _topic: &str,
        _payload: P,
        _options: PublishOptions,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        async { Ok(()) }
    }

    fn subscribe<const N: usize>(
        &self,
        _topics: &[(&str, QoS); N],
    ) -> impl Future<Output = Result<Self::Subscription<'_, N>, Self::Error>> {
        async { Ok(MockSubscription::new()) }
    }
}

/// Mock subscription that never yields messages.
///
/// This is sufficient for testing storage operations which
/// don't require actual MQTT message flow.
pub struct MockSubscription<const N: usize> {
    _marker: core::marker::PhantomData<[(); N]>,
}

impl<const N: usize> MockSubscription<N> {
    /// Create a new mock subscription.
    pub fn new() -> Self {
        Self {
            _marker: core::marker::PhantomData,
        }
    }
}

impl<const N: usize> Default for MockSubscription<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> MqttSubscription for MockSubscription<N> {
    type Message<'m>
        = MockMessage
    where
        Self: 'm;
    type Error = Infallible;

    fn next_message(&mut self) -> impl Future<Output = Option<Self::Message<'_>>> {
        // Never yields messages - tests that need messages should
        // use a more sophisticated mock or integration tests
        async { core::future::pending().await }
    }

    fn unsubscribe(self) -> impl Future<Output = Result<(), Self::Error>> {
        async { Ok(()) }
    }
}

/// Mock MQTT message.
pub struct MockMessage {
    topic: String,
    payload: Vec<u8>,
    qos: QoS,
}

impl MockMessage {
    /// Create a new mock message.
    #[allow(dead_code)]
    pub fn new(topic: impl Into<String>, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            topic: topic.into(),
            payload: payload.into(),
            qos: QoS::AtMostOnce,
        }
    }
}

impl MqttMessage for MockMessage {
    fn topic_name(&self) -> &str {
        &self.topic
    }

    fn payload(&self) -> &[u8] {
        &self.payload
    }

    fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.payload
    }

    fn qos(&self) -> QoS {
        self.qos
    }

    fn dup(&self) -> bool {
        false
    }

    fn retain(&self) -> bool {
        false
    }
}
