//! Implementation of MQTT traits for AWS Greengrass IPC.
//!
//! This module provides an std/tokio-based MQTT client using `greengrass-ipc-rust`.
//! Greengrass IPC natively supports single-topic subscriptions; multi-topic
//! subscriptions are implemented by polling individual streams.

use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use bytes::Bytes;
use futures::Stream;

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
        // Thing name from Greengrass environment variable
        let client_id =
            std::env::var("AWS_IOT_THING_NAME").unwrap_or_else(|_| "unknown".to_string());
        Ok(Self {
            client: Arc::new(client),
            client_id,
        })
    }

    /// Create a client from an existing IPC client.
    ///
    /// The client ID is retrieved from the `AWS_IOT_THING_NAME` environment variable.
    pub fn from_ipc(client: Arc<greengrass_ipc_rust::GreengrassCoreIPCClient>) -> Self {
        let client_id =
            std::env::var("AWS_IOT_THING_NAME").unwrap_or_else(|_| "unknown".to_string());
        Self { client, client_id }
    }

    /// Create a new client with a custom client ID.
    pub async fn connect_with_client_id(
        client_id: impl Into<String>,
    ) -> Result<Self, greengrass_ipc_rust::Error> {
        let client = greengrass_ipc_rust::GreengrassCoreIPCClient::connect().await?;
        Ok(Self {
            client: Arc::new(client),
            client_id: client_id.into(),
        })
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
                // MQTTv5 properties (could be extended)
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
        let client = self.client.clone();
        let topics_owned: [(&str, QoS); N] = *topics;
        async move {
            let mut streams = Vec::with_capacity(N);
            for (topic, qos) in &topics_owned {
                let stream = client
                    .subscribe_to_iot_core(greengrass_ipc_rust::SubscribeToIoTCoreRequest {
                        topic_name: topic.to_string(),
                        qos: to_gg_qos(*qos),
                    })
                    .await?;
                streams.push(stream);
            }

            Ok(GreengrassSubscription { streams })
        }
    }
}

/// Subscription wrapper for Greengrass.
///
/// Holds the underlying IPC stream operations. Call [`close()`](Self::close)
/// to deterministically terminate all streams before dropping. If `close()` is
/// not called, `Drop` on each [`StreamOperation`] performs best-effort cleanup
/// via fire-and-forget async tasks.
pub struct GreengrassSubscription {
    streams: Vec<greengrass_ipc_rust::StreamOperation<greengrass_ipc_rust::IoTCoreMessage>>,
}

impl GreengrassSubscription {
    /// Explicitly close all underlying IPC streams, awaiting each termination.
    ///
    /// Mirrors [`StreamOperation::close()`] — each stream sends
    /// `TERMINATE_STREAM` and unregisters its handler synchronously (awaited).
    /// Because the handler is removed before `Drop` runs, the `Drop` impl on
    /// each `StreamOperation` becomes a no-op (it cannot find the stream in
    /// the map).
    pub async fn close(self) -> Result<(), greengrass_ipc_rust::Error> {
        for stream in self.streams {
            // Errors are non-fatal: the stream may already be closed server-side.
            let _ = stream.close().await;
        }
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
            for stream in self.streams.iter_mut() {
                match Pin::new(stream).poll_next(cx) {
                    Poll::Ready(Some(msg)) => {
                        return Poll::Ready(Some(GreengrassMessage { inner: msg.message }));
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
        // Bytes requires conversion to owned Vec for mutation
        // For now, we use make_mut which clones if shared
        self.inner.payload.to_vec().leak()
    }

    fn qos(&self) -> QoS {
        // Greengrass doesn't expose QoS on received messages
        QoS::AtLeastOnce // Default assumption
    }

    fn dup(&self) -> bool {
        false // Not exposed by Greengrass
    }

    fn retain(&self) -> bool {
        false // Not exposed by Greengrass
    }
}

/// Convert our QoS to Greengrass QoS.
fn to_gg_qos(qos: QoS) -> greengrass_ipc_rust::QoS {
    match qos {
        QoS::AtMostOnce => greengrass_ipc_rust::QoS::AtMostOnce,
        // Greengrass only supports QoS 0 and 1
        QoS::AtLeastOnce | QoS::ExactlyOnce => greengrass_ipc_rust::QoS::AtLeastOnce,
    }
}

/// Re-export greengrass-ipc-rust types for convenience.
pub use greengrass_ipc_rust::{
    Error as GreengrassError, GreengrassCoreIPCClient, IoTCoreMessage, PublishToIoTCoreRequest,
    StreamOperation, SubscribeToIoTCoreRequest,
};
