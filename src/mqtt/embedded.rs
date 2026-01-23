//! Integration with `embedded-mqtt`.
//!
//! This module provides trait implementations for using `embedded-mqtt` with
//! our MQTT abstraction traits.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_mqtt::{BufferProvider, SliceBufferProvider};

use crate::mqtt::{MqttMessage, MqttSubscription, PublishOptions, QoS, ToPayload};

/// Wrapper to bridge our [`ToPayload`] to embedded-mqtt's `ToPayload`.
///
/// This is a thin adapter that converts between the two trait interfaces
/// with minimal overhead.
///
/// # Example
///
/// ```ignore
/// use rustot::mqtt::{ToPayload, DeferredPayload};
/// use rustot::mqtt::embedded::PayloadBridge;
///
/// let payload = DeferredPayload::new(|buf| {
///     // serialize into buf
///     Ok(10)
/// }, 128);
///
/// let publish = embedded_mqtt::Publish::builder()
///     .topic_name("my/topic")
///     .payload(PayloadBridge(payload))
///     .build();
/// ```
pub struct PayloadBridge<P>(pub P);

impl<P: ToPayload> embedded_mqtt::ToPayload for PayloadBridge<P> {
    fn serialize(&self, buffer: &mut [u8]) -> Result<usize, embedded_mqtt::EncodingError> {
        self.0
            .encode(buffer)
            .map_err(|_| embedded_mqtt::EncodingError::BufferSize)
    }

    fn max_size(&self) -> usize {
        self.0.max_size()
    }
}

/// Convert our [`QoS`] to embedded-mqtt's `QoS`.
pub fn to_embedded_qos(qos: QoS) -> embedded_mqtt::QoS {
    match qos {
        QoS::AtMostOnce => embedded_mqtt::QoS::AtMostOnce,
        QoS::AtLeastOnce => embedded_mqtt::QoS::AtLeastOnce,
        #[cfg(feature = "qos2")]
        QoS::ExactlyOnce => embedded_mqtt::QoS::ExactlyOnce,
        #[cfg(not(feature = "qos2"))]
        QoS::ExactlyOnce => embedded_mqtt::QoS::AtLeastOnce, // Fallback
    }
}

/// Convert embedded-mqtt's `QoS` to our [`QoS`].
pub fn from_embedded_qos(qos: embedded_mqtt::QoS) -> QoS {
    match qos {
        embedded_mqtt::QoS::AtMostOnce => QoS::AtMostOnce,
        embedded_mqtt::QoS::AtLeastOnce => QoS::AtLeastOnce,
        #[cfg(feature = "qos2")]
        embedded_mqtt::QoS::ExactlyOnce => QoS::ExactlyOnce,
    }
}

// --- Trait Implementations ---

impl<'a, M: RawMutex, B: BufferProvider> MqttMessage for embedded_mqtt::Message<'a, M, B> {
    fn topic_name(&self) -> &str {
        self.topic_name()
    }

    fn payload(&self) -> &[u8] {
        self.payload()
    }

    fn payload_mut(&mut self) -> &mut [u8] {
        self.payload_mut()
    }

    fn qos(&self) -> QoS {
        from_embedded_qos(self.qos_pid().qos())
    }

    fn dup(&self) -> bool {
        self.dup()
    }

    fn retain(&self) -> bool {
        self.retain()
    }
}

impl<'a, 'b, M: RawMutex, const N: usize> MqttSubscription
    for embedded_mqtt::Subscription<'a, 'b, M, N>
{
    type Message<'m> = embedded_mqtt::Message<'a, M, SliceBufferProvider<'a>> where Self: 'm;
    type Error = embedded_mqtt::Error;

    fn next_message(&mut self) -> impl core::future::Future<Output = Option<Self::Message<'_>>> {
        embedded_mqtt::Subscription::next_message(self)
    }

    fn unsubscribe(self) -> impl core::future::Future<Output = Result<(), Self::Error>> {
        embedded_mqtt::Subscription::unsubscribe(self)
    }
}

impl<'a, M: RawMutex> crate::mqtt::MqttClient for embedded_mqtt::MqttClient<'a, M> {
    type Subscription<'m, const N: usize> = embedded_mqtt::Subscription<'a, 'm, M, N> where Self: 'm;
    type Error = embedded_mqtt::Error;

    fn client_id(&self) -> &str {
        self.client_id()
    }

    fn wait_connected(&self) -> impl core::future::Future<Output = ()> {
        embedded_mqtt::MqttClient::wait_connected(self)
    }

    fn publish_with_options<P: ToPayload>(
        &self,
        topic: &str,
        payload: P,
        options: PublishOptions,
    ) -> impl core::future::Future<Output = Result<(), Self::Error>> {
        let publish = embedded_mqtt::Publish::builder()
            .topic_name(topic)
            .payload(PayloadBridge(payload))
            .qos(to_embedded_qos(options.qos))
            .retain(options.retain)
            .dup(options.dup)
            .build();
        embedded_mqtt::MqttClient::publish(self, publish)
    }

    fn subscribe<const N: usize>(
        &self,
        topics: &[(&str, QoS); N],
    ) -> impl core::future::Future<Output = Result<Self::Subscription<'_, N>, Self::Error>> {
        let subscribe_topics: [embedded_mqtt::SubscribeTopic<'_>; N] =
            core::array::from_fn(|i| {
                embedded_mqtt::SubscribeTopic::builder()
                    .topic_path(topics[i].0)
                    .maximum_qos(to_embedded_qos(topics[i].1))
                    .build()
            });

        async move {
            let subscribe = embedded_mqtt::Subscribe::builder()
                .topics(&subscribe_topics)
                .build();
            embedded_mqtt::MqttClient::subscribe(self, subscribe)
                .await
        }
    }
}

/// Re-export embedded-mqtt types for convenience.
pub use embedded_mqtt::{
    Broker, Config, DomainBroker, Error as EmbeddedMqttError, IpBroker,
    Message, MqttClient as EmbeddedMqttClient, MqttStack, Publish, QoS as EmbeddedQoS,
    State, Subscribe, Subscription, ToPayload as EmbeddedToPayload,
};
