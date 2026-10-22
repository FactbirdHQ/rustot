//! Implementation of MQTT traits for AWS Greengrass IPC.
//!
//! This module provides an std/tokio-based MQTT client using `greengrass-ipc-rust`.
//!
//! ## Subscriptions and unsubscribe
//!
//! Subscriptions go through `greengrass-ipc-rust`'s IoT Core pool: one IPC
//! stream per topic filter per IPC client, shared by every subscriber and kept
//! open after the last one leaves. That is what makes request/response cycles
//! (subscribe, publish, await the reply, unsubscribe) safe to repeat: the
//! Greengrass core has no unsubscribe acknowledgement, and a resubscribe that
//! races its cloud-side unsubscribe is left without a cloud subscription.
//!
//! [`MqttSubscription::unsubscribe`] (or dropping the subscription) therefore
//! means: no further message reaches this subscription, and a later subscribe
//! to the same topic receives everything published after it returns. It does
//! not release a cloud subscription the way an UNSUBACK would; the pool closes
//! idle streams only when new topics take it past its limit
//! ([`GreengrassCoreIPCClient::set_iot_core_stream_limit`]).
//!
//! Every [`GreengrassClient`] built from the same IPC client shares that pool.

use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use bytes::Bytes;

use crate::mqtt::{MqttMessage, MqttSubscription, PublishOptions, QoS, ToPayload};

/// Wrapper around greengrass-ipc-rust client.
///
/// Provides our [`MqttClient`](crate::mqtt::MqttClient) trait implementation
/// for Greengrass components communicating with AWS IoT Core.
pub struct GreengrassClient {
    client: Arc<greengrass_ipc_rust::GreengrassCoreIPCClient>,
    /// Greengrass doesn't expose client_id directly - use thing name from env.
    client_id: String,
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

    const fn new(
        client: Arc<greengrass_ipc_rust::GreengrassCoreIPCClient>,
        client_id: String,
    ) -> Self {
        Self { client, client_id }
    }

    /// Number of IoT Core streams the shared IPC client holds open, one per
    /// cloud-side subscription.
    pub fn pooled_subscription_count(&self) -> usize {
        self.client.iot_core_stream_count()
    }

    /// Topic filters of the IoT Core streams the shared IPC client holds open.
    pub fn pooled_topics(&self) -> Vec<String> {
        self.client.iot_core_topics()
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
            // A drop part-way through releases the subscriptions made so far.
            let mut subscriptions = Vec::with_capacity(N);
            for (topic, qos) in topics_owned {
                subscriptions.push(
                    self.client
                        .subscribe_to_iot_core(greengrass_ipc_rust::SubscribeToIoTCoreRequest {
                            topic_name: topic,
                            qos: to_gg_qos(qos),
                        })
                        .await?,
                );
            }
            Ok(GreengrassSubscription { subscriptions })
        }
    }
}

/// Subscription wrapper for Greengrass.
///
/// Holds one pooled IoT Core subscription per topic. Dropping it,
/// [`close()`](Self::close) or [`unsubscribe()`](MqttSubscription::unsubscribe)
/// unsubscribes; see the module docs for what that guarantees.
pub struct GreengrassSubscription {
    subscriptions: Vec<greengrass_ipc_rust::IoTCoreSubscription>,
}

impl GreengrassSubscription {
    /// Release this subscription.
    pub async fn close(self) -> Result<(), greengrass_ipc_rust::Error> {
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
        use futures::Stream;
        futures::future::poll_fn(|cx| {
            let mut all_done = true;
            for sub in &mut self.subscriptions {
                match Pin::new(&mut *sub).poll_next(cx) {
                    Poll::Ready(Some(msg)) => {
                        return Poll::Ready(Some(GreengrassMessage { inner: msg.message }));
                    }
                    Poll::Ready(None) => {}
                    Poll::Pending => all_done = false,
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
    Error as GreengrassError, GreengrassCoreIPCClient, IoTCoreMessage, IoTCoreSubscription,
    PublishToIoTCoreRequest, StreamOperation, SubscribeToIoTCoreRequest,
};
