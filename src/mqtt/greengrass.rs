//! Implementation of MQTT traits for AWS Greengrass IPC.
//!
//! This module provides an std/tokio-based MQTT client using `greengrass-ipc-rust`.
//! Greengrass IPC has a simpler architecture than direct MQTT - single-topic
//! subscriptions and automatic message routing through the Greengrass nucleus.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;

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
    // Greengrass only supports single-topic subscriptions, so N is ignored
    type Subscription<'m, const N: usize> = GreengrassSubscription where Self: 'm;
    type Error = greengrass_ipc_rust::Error;

    fn client_id(&self) -> &str {
        &self.client_id
    }

    fn wait_connected(&self) -> impl core::future::Future<Output = ()> {
        async {
            // Greengrass IPC is already connected when client is created
        }
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
            // Greengrass only supports single-topic subscriptions
            // For multi-topic, we'd need to create multiple subscriptions
            // For simplicity, use the first topic
            if topics_owned.is_empty() {
                return Err(greengrass_ipc_rust::Error::InvalidInput(
                    "At least one topic required".to_string(),
                ));
            }

            let (topic, qos) = &topics_owned[0];
            let stream = client
                .subscribe_to_iot_core(greengrass_ipc_rust::SubscribeToIoTCoreRequest {
                    topic_name: topic.to_string(),
                    qos: to_gg_qos(*qos),
                })
                .await?;

            Ok(GreengrassSubscription { stream })
        }
    }
}

/// Subscription wrapper for Greengrass.
pub struct GreengrassSubscription {
    stream: greengrass_ipc_rust::StreamOperation<greengrass_ipc_rust::IoTCoreMessage>,
}

impl MqttSubscription for GreengrassSubscription {
    type Message<'m> = GreengrassMessage where Self: 'm;
    type Error = greengrass_ipc_rust::Error;

    fn next_message(&mut self) -> impl core::future::Future<Output = Option<Self::Message<'_>>> {
        async {
            self.stream.next().await.map(|iot_msg| GreengrassMessage {
                inner: iot_msg.message,
            })
        }
    }

    fn unsubscribe(self) -> impl core::future::Future<Output = Result<(), Self::Error>> {
        async move { self.stream.close().await }
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
    Error as GreengrassError, GreengrassCoreIPCClient, IoTCoreMessage,
    PublishToIoTCoreRequest, StreamOperation, SubscribeToIoTCoreRequest,
};
