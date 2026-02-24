//! Integration with `mqttrust`.
//!
//! This module provides trait implementations for using `mqttrust` with
//! our MQTT abstraction traits.

use embassy_sync::blocking_mutex::raw::RawMutex;
use mqttrust::{BufferProvider, SliceBufferProvider};

use crate::mqtt::{MqttMessage, MqttSubscription, PublishOptions, QoS, ToPayload};

/// Wrapper to bridge our [`ToPayload`] to mqttrust's `ToPayload`.
///
/// This is a thin adapter that converts between the two trait interfaces
/// with minimal overhead.
///
/// # Example
///
/// ```ignore
/// use rustot::mqtt::{ToPayload, DeferredPayload};
/// use rustot::mqtt::mqttrust::PayloadBridge;
///
/// let payload = DeferredPayload::new(|buf| {
///     // serialize into buf
///     Ok(10)
/// }, 128);
///
/// let publish = mqttrust::Publish::builder()
///     .topic_name("my/topic")
///     .payload(PayloadBridge(payload))
///     .build();
/// ```
pub struct PayloadBridge<P>(pub P);

impl<P: ToPayload> mqttrust::ToPayload for PayloadBridge<P> {
    fn serialize(&self, buffer: &mut [u8]) -> Result<usize, mqttrust::EncodingError> {
        self.0
            .encode(buffer)
            .map_err(|_| mqttrust::EncodingError::BufferSize)
    }

    fn max_size(&self) -> usize {
        self.0.max_size()
    }
}

/// Convert our [`QoS`] to mqttrust's `QoS`.
pub fn to_embedded_qos(qos: QoS) -> mqttrust::QoS {
    match qos {
        QoS::AtMostOnce => mqttrust::QoS::AtMostOnce,
        QoS::AtLeastOnce => mqttrust::QoS::AtLeastOnce,
        _ => mqttrust::QoS::AtLeastOnce, // Fallback
    }
}

/// Convert mqttrust's `QoS` to our [`QoS`].
pub fn from_embedded_qos(qos: mqttrust::QoS) -> QoS {
    match qos {
        mqttrust::QoS::AtMostOnce => QoS::AtMostOnce,
        _ => QoS::AtLeastOnce,
    }
}

// --- Trait Implementations ---

impl<'a, M: RawMutex, B: BufferProvider> MqttMessage for mqttrust::Message<'a, M, B> {
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
    for mqttrust::Subscription<'a, 'b, M, N>
{
    type Message<'m>
        = mqttrust::Message<'a, M, SliceBufferProvider<'a>>
    where
        Self: 'm;
    type Error = mqttrust::Error;

    fn next_message(&mut self) -> impl core::future::Future<Output = Option<Self::Message<'_>>> {
        mqttrust::Subscription::next_message(self)
    }

    fn unsubscribe(self) -> impl core::future::Future<Output = Result<(), Self::Error>> {
        mqttrust::Subscription::unsubscribe(self)
    }
}

impl<'a, M: RawMutex> crate::mqtt::MqttClient for mqttrust::MqttClient<'a, M> {
    type Subscription<'m, const N: usize>
        = mqttrust::Subscription<'a, 'm, M, N>
    where
        Self: 'm;
    type Error = mqttrust::Error;

    fn client_id(&self) -> &str {
        self.client_id()
    }

    fn wait_connected(&self) -> impl core::future::Future<Output = ()> {
        mqttrust::MqttClient::wait_connected(self)
    }

    fn publish_with_options<P: ToPayload>(
        &self,
        topic: &str,
        payload: P,
        options: PublishOptions,
    ) -> impl core::future::Future<Output = Result<(), Self::Error>> {
        let publish = mqttrust::Publish::builder()
            .topic_name(topic)
            .payload(PayloadBridge(payload))
            .qos(to_embedded_qos(options.qos))
            .retain(options.retain)
            .dup(options.dup)
            .build();
        mqttrust::MqttClient::publish(self, publish)
    }

    fn subscribe<const N: usize>(
        &self,
        topics: &[(&str, QoS); N],
    ) -> impl core::future::Future<Output = Result<Self::Subscription<'_, N>, Self::Error>> {
        let subscribe_topics: [mqttrust::SubscribeTopic<'_>; N] = core::array::from_fn(|i| {
            mqttrust::SubscribeTopic::builder()
                .topic_path(topics[i].0)
                .maximum_qos(to_embedded_qos(topics[i].1))
                .build()
        });

        async move {
            let subscribe = mqttrust::Subscribe::builder()
                .topics(&subscribe_topics)
                .build();
            mqttrust::MqttClient::subscribe(self, subscribe).await
        }
    }
}

/// Re-export mqttrust types for convenience.
pub use mqttrust::{
    Broker, Config, DomainBroker, Error as EmbeddedMqttError, IpBroker, Message,
    MqttClient as EmbeddedMqttClient, MqttStack, Publish, QoS as EmbeddedQoS, State, Subscribe,
    Subscription, ToPayload as EmbeddedToPayload,
};
