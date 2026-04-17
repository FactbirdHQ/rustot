//! Implementation of MQTT traits for AWS Greengrass IPC.
//!
//! This module provides an std/tokio-based MQTT client using `greengrass-ipc-rust`.
//!
//! ## Subscription pooling
//!
//! The Greengrass IPC protocol has a quirk: when the client terminates a
//! subscription stream (`TERMINATE_STREAM`), the Greengrass core unsubscribes
//! from the underlying MQTT topic at the cloud broker. If the client then
//! immediately resubscribes to the same topic, there is a window where the
//! topic is unsubscribed cloud-side, and any response published during that
//! window is lost. Unlike normal MQTT, `TERMINATE_STREAM` has no protocol
//! acknowledgment, so the client cannot wait for the unsubscribe to complete.
//!
//! To avoid this race, we pool IPC subscription streams per topic inside
//! [`GreengrassClient`]. A single long-lived stream per topic is kept open,
//! and a lightweight channel is swapped on each logical subscribe / unsubscribe
//! from rustot's core. The IPC stream is never terminated during normal
//! operation, so the MQTT subscription at the Greengrass core stays alive and
//! no messages are lost between rustot subscribe cycles.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::Poll;

use bytes::Bytes;
use tokio::sync::mpsc;

use crate::mqtt::{MqttMessage, MqttSubscription, PublishOptions, QoS, ToPayload};

type TopicChannel = mpsc::UnboundedSender<greengrass_ipc_rust::MqttMessage>;

/// Per-topic pooled state. Shared between the forwarding task and every
/// logical subscriber.
struct PooledSlot {
    /// Sender for the currently active logical subscriber. `None` when the
    /// topic was previously subscribed and has since been unsubscribed.
    /// Accessed synchronously (short critical sections only).
    sender: Mutex<Option<TopicChannel>>,
}

/// Wrapper around greengrass-ipc-rust client.
///
/// Provides our [`MqttClient`](crate::mqtt::MqttClient) trait implementation
/// for Greengrass components communicating with AWS IoT Core.
pub struct GreengrassClient {
    client: Arc<greengrass_ipc_rust::GreengrassCoreIPCClient>,
    /// Greengrass doesn't expose client_id directly - use thing name from env.
    client_id: String,
    /// Pool of active IPC subscription streams keyed by topic.
    /// Uses std RwLock — no lock is held across await points.
    pool: Arc<RwLock<HashMap<String, Arc<PooledSlot>>>>,
}

impl GreengrassClient {
    /// Connect to the Greengrass Core IPC service.
    ///
    /// The client ID is retrieved from the `AWS_IOT_THING_NAME` environment variable.
    pub async fn connect() -> Result<Self, greengrass_ipc_rust::Error> {
        let client = greengrass_ipc_rust::GreengrassCoreIPCClient::connect().await?;
        let client_id =
            std::env::var("AWS_IOT_THING_NAME").unwrap_or_else(|_| "unknown".to_string());
        Ok(Self::new(Arc::new(client), client_id))
    }

    /// Create a client from an existing IPC client.
    ///
    /// The client ID is retrieved from the `AWS_IOT_THING_NAME` environment variable.
    pub fn from_ipc(client: Arc<greengrass_ipc_rust::GreengrassCoreIPCClient>) -> Self {
        let client_id =
            std::env::var("AWS_IOT_THING_NAME").unwrap_or_else(|_| "unknown".to_string());
        Self::new(client, client_id)
    }

    /// Create a new client with a custom client ID.
    pub async fn connect_with_client_id(
        client_id: impl Into<String>,
    ) -> Result<Self, greengrass_ipc_rust::Error> {
        let client = greengrass_ipc_rust::GreengrassCoreIPCClient::connect().await?;
        Ok(Self::new(Arc::new(client), client_id.into()))
    }

    fn new(client: Arc<greengrass_ipc_rust::GreengrassCoreIPCClient>, client_id: String) -> Self {
        Self {
            client,
            client_id,
            pool: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Ensure a long-lived IPC subscription exists for `topic` and return its
    /// pooled slot. Spawns a forwarding task on first subscribe.
    ///
    /// The pool lock is never held across an await point.
    async fn ensure_slot(
        &self,
        topic: &str,
        qos: QoS,
    ) -> Result<Arc<PooledSlot>, greengrass_ipc_rust::Error> {
        // Fast path: already pooled.
        if let Some(slot) = self.pool.read().unwrap().get(topic).cloned() {
            return Ok(slot);
        }

        // Slow path: subscribe without holding the pool lock.
        let stream = self
            .client
            .subscribe_to_iot_core(greengrass_ipc_rust::SubscribeToIoTCoreRequest {
                topic_name: topic.to_string(),
                qos: to_gg_qos(qos),
            })
            .await?;

        let slot = Arc::new(PooledSlot {
            sender: Mutex::new(None),
        });

        // Insert into pool. Recheck in case of race — another task might
        // have subscribed to the same topic while we were awaiting.
        {
            let mut pool = self.pool.write().unwrap();
            if let Some(existing) = pool.get(topic).cloned() {
                drop(stream);
                return Ok(existing);
            }
            pool.insert(topic.to_string(), slot.clone());
        }

        // Spawn the forwarder. It reads from the IPC stream forever and
        // forwards each message to whichever subscriber's sender is
        // currently installed in the slot. When the stream closes (e.g.
        // IPC connection lost) the task removes the entry so a subsequent
        // subscribe reopens the stream.
        let slot_for_task = slot.clone();
        let pool_for_task = self.pool.clone();
        let topic_for_task = topic.to_string();
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = stream;
            while let Some(msg) = stream.next().await {
                let sender = { slot_for_task.sender.lock().unwrap().clone() };
                if let Some(tx) = sender {
                    let _ = tx.send(msg.message);
                }
            }
            pool_for_task.write().unwrap().remove(&topic_for_task);
        });

        Ok(slot)
    }
}

impl crate::mqtt::MqttClient for GreengrassClient {
    type Subscription<'m, const N: usize>
        = GreengrassSubscription
    where
        Self: 'm;
    type Error = greengrass_ipc_rust::Error;

    fn client_id(&self) -> &str {
        &self.client_id
    }

    async fn wait_connected(&self) {
        // Greengrass IPC is already connected when client is created
    }

    fn publish_with_options<P: ToPayload>(
        &self,
        topic: &str,
        payload: P,
        options: PublishOptions,
    ) -> impl core::future::Future<Output = Result<(), Self::Error>> {
        let client = self.client.clone();
        let topic = topic.to_string();
        async move {
            // Encode payload to Bytes
            let mut buf = vec![0u8; payload.max_size()];
            let len = payload.encode(&mut buf).map_err(|_| {
                greengrass_ipc_rust::Error::SerializationError(
                    "Payload encoding failed".to_string(),
                )
            })?;
            buf.truncate(len);

            let request = greengrass_ipc_rust::PublishToIoTCoreRequest {
                topic_name: topic,
                qos: to_gg_qos(options.qos),
                payload: Bytes::from(buf),
                user_properties: None,
                message_expiry_interval_seconds: None,
                correlation_data: None,
                response_topic: None,
                content_type: None,
            };

            client.publish_to_iot_core(request).await?;
            Ok(())
        }
    }

    fn subscribe<const N: usize>(
        &self,
        topics: &[(&str, QoS); N],
    ) -> impl core::future::Future<Output = Result<Self::Subscription<'_, N>, Self::Error>> {
        let topics_owned: [(String, QoS); N] = topics.map(|(t, q)| (t.to_string(), q));
        async move {
            let mut receivers = Vec::with_capacity(N);
            for (topic, qos) in &topics_owned {
                let slot = self.ensure_slot(topic, *qos).await?;
                let (tx, rx) = mpsc::unbounded_channel();
                // Swap in the new sender. Any previous sender is dropped —
                // its receiver (held by the previous subscriber) sees the
                // channel close but keeps whatever is still buffered.
                *slot.sender.lock().unwrap() = Some(tx.clone());
                receivers.push(SubscriptionReceiver { slot, tx, rx });
            }
            Ok(GreengrassSubscription { receivers })
        }
    }
}

/// One logical-subscriber handle: a reference to the pooled slot plus the
/// receiver half of the channel currently installed in that slot. Holds a
/// clone of its own sender for identity checks on teardown.
struct SubscriptionReceiver {
    slot: Arc<PooledSlot>,
    tx: TopicChannel,
    rx: mpsc::UnboundedReceiver<greengrass_ipc_rust::MqttMessage>,
}

/// Subscription wrapper for Greengrass.
///
/// Holds one receiver per subscribed topic. On drop or explicit
/// [`close()`](Self::close) / [`unsubscribe()`](MqttSubscription::unsubscribe),
/// the receivers are dropped and the pooled slots' senders are cleared.
/// The underlying IPC streams stay open and reusable for future subscribes.
pub struct GreengrassSubscription {
    receivers: Vec<SubscriptionReceiver>,
}

impl GreengrassSubscription {
    /// Release this subscription. Clears the pooled slots' senders so that
    /// future messages are discarded until someone subscribes again. Does
    /// NOT terminate the underlying IPC streams — they stay in the pool.
    pub async fn close(self) -> Result<(), greengrass_ipc_rust::Error> {
        // Delegated to Drop.
        Ok(())
    }
}

impl MqttSubscription for GreengrassSubscription {
    type Message<'m>
        = GreengrassMessage
    where
        Self: 'm;
    type Error = greengrass_ipc_rust::Error;

    async fn next_message(&mut self) -> Option<Self::Message<'_>> {
        futures::future::poll_fn(|cx| {
            let mut all_done = true;
            for recv in self.receivers.iter_mut() {
                match Pin::new(&mut recv.rx).poll_recv(cx) {
                    Poll::Ready(Some(msg)) => {
                        return Poll::Ready(Some(GreengrassMessage { inner: msg }));
                    }
                    Poll::Ready(None) => continue,
                    Poll::Pending => {
                        all_done = false;
                    }
                }
            }
            if all_done {
                Poll::Ready(None)
            } else {
                Poll::Pending
            }
        })
        .await
    }

    async fn unsubscribe(self) -> Result<(), Self::Error> {
        self.close().await
    }
}

impl Drop for GreengrassSubscription {
    fn drop(&mut self) {
        // Only clear the slot if it still holds OUR sender. If someone
        // else has already resubscribed to the same topic, they installed
        // a new sender and we must not clobber it. `same_channel` tells us
        // whether two senders reference the same underlying channel.
        for receiver in &self.receivers {
            let mut guard = receiver.slot.sender.lock().unwrap();
            if let Some(ref current) = *guard
                && current.same_channel(&receiver.tx)
            {
                *guard = None;
            }
        }
    }
}

/// Message wrapper for Greengrass.
pub struct GreengrassMessage {
    inner: greengrass_ipc_rust::MqttMessage,
}

impl GreengrassMessage {
    /// Get the inner MqttMessage for access to Greengrass-specific fields.
    pub fn inner(&self) -> &greengrass_ipc_rust::MqttMessage {
        &self.inner
    }

    /// Get MQTTv5 user properties, if present.
    pub fn user_properties(&self) -> Option<&[greengrass_ipc_rust::model::UserProperty]> {
        self.inner.user_properties.as_deref()
    }

    /// Get the correlation data, if present.
    pub fn correlation_data(&self) -> Option<&Bytes> {
        self.inner.correlation_data.as_ref()
    }

    /// Get the response topic, if present.
    pub fn response_topic(&self) -> Option<&str> {
        self.inner.response_topic.as_deref()
    }

    /// Get the content type, if present.
    pub fn content_type(&self) -> Option<&str> {
        self.inner.content_type.as_deref()
    }
}

impl MqttMessage for GreengrassMessage {
    fn topic_name(&self) -> &str {
        &self.inner.topic_name
    }

    fn payload(&self) -> &[u8] {
        &self.inner.payload
    }

    fn payload_mut(&mut self) -> &mut [u8] {
        self.inner.payload.to_vec().leak()
    }

    fn qos(&self) -> QoS {
        QoS::AtLeastOnce
    }

    fn dup(&self) -> bool {
        false
    }

    fn retain(&self) -> bool {
        false
    }
}

/// Convert our QoS to Greengrass QoS.
fn to_gg_qos(qos: QoS) -> greengrass_ipc_rust::QoS {
    match qos {
        QoS::AtMostOnce => greengrass_ipc_rust::QoS::AtMostOnce,
        QoS::AtLeastOnce | QoS::ExactlyOnce => greengrass_ipc_rust::QoS::AtLeastOnce,
    }
}

/// Re-export greengrass-ipc-rust types for convenience.
pub use greengrass_ipc_rust::{
    Error as GreengrassError, GreengrassCoreIPCClient, IoTCoreMessage, PublishToIoTCoreRequest,
    StreamOperation, SubscribeToIoTCoreRequest,
};
