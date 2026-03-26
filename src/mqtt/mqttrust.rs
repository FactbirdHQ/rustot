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
pub fn to_mqttrust_qos(qos: QoS) -> mqttrust::QoS {
    match qos {
        QoS::AtMostOnce => mqttrust::QoS::AtMostOnce,
        QoS::AtLeastOnce => mqttrust::QoS::AtLeastOnce,
        _ => mqttrust::QoS::AtLeastOnce, // Fallback
    }
}

/// Convert mqttrust's `QoS` to our [`QoS`].
pub fn from_mqttrust_qos(qos: mqttrust::QoS) -> QoS {
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
        from_mqttrust_qos(self.qos_pid().qos())
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
            .qos(to_mqttrust_qos(options.qos))
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
                .maximum_qos(to_mqttrust_qos(topics[i].1))
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

/// Owned copy of an MQTT message, backed by fixed-size `heapless` buffers.
///
/// Use this to copy a message out of the mqttrust ring buffer so the
/// grant is released immediately. Holding a `mqttrust::Message` pins the
/// pubsub ring buffer and prevents incoming messages from being written —
/// this type avoids that.
///
/// # Type Parameters
///
/// * `TOPIC` — max topic length in bytes (default 256)
/// * `PAYLOAD` — max payload length in bytes (default 2048)
///
/// # Example
///
/// ```ignore
/// let message = subscription.next_message().await.unwrap();
/// let owned = OwnedMessage::from_ref(&message);
/// drop(message); // release the ring buffer grant
///
/// // owned implements MqttMessage — use it anywhere a message is expected
/// let execution = parse_job_message::<MyJobs>(&mut owned);
/// ```
pub struct OwnedMessage<const TOPIC: usize = 256, const PAYLOAD: usize = 2048> {
    topic: heapless::String<TOPIC>,
    payload: heapless::Vec<u8, PAYLOAD>,
    qos: QoS,
    dup: bool,
    retain: bool,
}

impl<const TOPIC: usize, const PAYLOAD: usize> OwnedMessage<TOPIC, PAYLOAD> {
    /// Copy a mqttrust message into owned buffers, releasing the ring buffer grant.
    ///
    /// Returns `None` if the topic or payload exceeds the buffer capacity.
    pub fn from_ref<M: RawMutex, B: BufferProvider>(
        msg: &mqttrust::Message<'_, M, B>,
    ) -> Option<Self> {
        Some(Self {
            topic: heapless::String::try_from(msg.topic_name()).ok()?,
            payload: heapless::Vec::from_slice(msg.payload()).ok()?,
            qos: from_mqttrust_qos(msg.qos_pid().qos()),
            dup: msg.dup(),
            retain: msg.retain(),
        })
    }
}

impl<const TOPIC: usize, const PAYLOAD: usize> MqttMessage for OwnedMessage<TOPIC, PAYLOAD> {
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
        self.dup
    }

    fn retain(&self) -> bool {
        self.retain
    }
}

/// Re-export mqttrust types for convenience.
pub use mqttrust::{
    Broker, Config, DomainBroker, Error as EmbeddedMqttError, IpBroker, Message,
    MqttClient as EmbeddedMqttClient, MqttStack, Publish, QoS as EmbeddedQoS, State, Subscribe,
    Subscription, ToPayload as EmbeddedToPayload,
};
