//! Implementation of MQTT traits for `rumqttc`.
//!
//! This module provides an std/tokio-based MQTT client using `rumqttc`.
//! Unlike mqttrust, rumqttc separates sending (AsyncClient) from
//! receiving (EventLoop), requiring a wrapper that manages message routing.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc, watch};

use crate::mqtt::{MqttMessage, MqttSubscription, PublishOptions, QoS, ToPayload};

/// Wrapper around rumqttc providing our [`MqttClient`](crate::mqtt::MqttClient) trait.
///
/// Unlike mqttrust, rumqttc separates sending (AsyncClient) from
/// receiving (EventLoop). This wrapper:
/// 1. Spawns a task to poll the EventLoop
/// 2. Routes incoming messages to subscriptions via channels
pub struct RumqttcClient {
    client: rumqttc::AsyncClient,
    client_id: String,
    /// Routes incoming publishes to subscriptions by subscription ID
    router: Arc<Mutex<MessageRouter>>,
    /// `true` when CONNACK received, `false` after disconnect or error.
    /// Used by `wait_connected` only.
    connection_state: watch::Sender<bool>,
    /// Bumped on each CONNACK with `session_present=false` (broker dropped
    /// our subs). Subscriptions snapshot this at creation; `next_message`
    /// races against `changed()` so a parked waiter wakes with `None` when
    /// the broker has actually dropped our routing. Transient disconnects
    /// and session-resume reconnects do not bump this counter — broker
    /// state is intact in those cases, so the subscription stays valid.
    clean_session_count: watch::Sender<u8>,
}

struct MessageRouter {
    next_sub_id: u64,
    /// Map of subscription_id -> (topic_filters, sender)
    subscriptions: HashMap<u64, (Vec<String>, mpsc::Sender<RumqttcMessage>)>,
}

impl RumqttcClient {
    /// Create a new client and spawn the eventloop task.
    ///
    /// Returns the client and a join handle for the eventloop task.
    pub fn new(options: rumqttc::MqttOptions, cap: usize) -> (Self, tokio::task::JoinHandle<()>) {
        let client_id = options.client_id().to_string();
        let (client, eventloop) = rumqttc::AsyncClient::new(options, cap);

        let router = Arc::new(Mutex::new(MessageRouter {
            next_sub_id: 0,
            subscriptions: HashMap::new(),
        }));
        let (connection_state, _) = watch::channel(false);
        let (clean_session_count, _) = watch::channel(0u8);

        let router_clone = router.clone();
        let connection_state_clone = connection_state.clone();
        let clean_session_count_clone = clean_session_count.clone();
        let handle = tokio::spawn(async move {
            Self::run_eventloop(
                eventloop,
                router_clone,
                connection_state_clone,
                clean_session_count_clone,
            )
            .await;
        });

        (
            Self {
                client,
                client_id,
                router,
                connection_state,
                clean_session_count,
            },
            handle,
        )
    }

    async fn run_eventloop(
        mut eventloop: rumqttc::EventLoop,
        router: Arc<Mutex<MessageRouter>>,
        connection_state: watch::Sender<bool>,
        clean_session_count: watch::Sender<u8>,
    ) {
        loop {
            match eventloop.poll().await {
                Ok(rumqttc::Event::Incoming(rumqttc::Packet::ConnAck(connack))) => {
                    let _ = connection_state.send(true);
                    if !connack.session_present {
                        let next = clean_session_count.borrow().wrapping_add(1);
                        let _ = clean_session_count.send(next);
                    }
                }
                Ok(rumqttc::Event::Incoming(rumqttc::Packet::Publish(publish))) => {
                    // Route to matching subscriptions
                    let router = router.lock().await;
                    for (_, (filters, sender)) in router.subscriptions.iter() {
                        if filters.iter().any(|f| topic_matches(f, &publish.topic)) {
                            let msg = RumqttcMessage {
                                topic: publish.topic.clone(),
                                payload: publish.payload.to_vec(),
                                qos: from_rumqttc_qos(publish.qos),
                                dup: publish.dup,
                                retain: publish.retain,
                            };
                            let _ = sender.try_send(msg);
                        }
                    }
                }
                Ok(rumqttc::Event::Incoming(rumqttc::Packet::Disconnect)) => {
                    let _ = connection_state.send(false);
                }
                Ok(_) => {} // Other events (SubAck, PubAck, etc.)
                Err(_e) => {
                    // Connection error - rumqttc will auto-reconnect on next poll
                    let _ = connection_state.send(false);
                    #[cfg(feature = "log")]
                    log::warn!("EventLoop error: {:?}", _e);
                }
            }
        }
    }
}

impl crate::mqtt::MqttClient for RumqttcClient {
    type Subscription<'m, const N: usize>
        = RumqttcSubscription<N>
    where
        Self: 'm;
    type Error = rumqttc::ClientError;

    fn client_id(&self) -> &str {
        &self.client_id
    }

    fn wait_connected(&self) -> impl core::future::Future<Output = ()> {
        let mut rx = self.connection_state.subscribe();
        async move {
            loop {
                if *rx.borrow_and_update() {
                    return;
                }
                if rx.changed().await.is_err() {
                    // Sender dropped — eventloop task is gone, will never connect.
                    return;
                }
            }
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
            // Encode payload to Vec<u8>
            let mut buf = vec![0u8; payload.max_size()];
            let len =
                payload.encode(&mut buf).map_err(|_| {
                    rumqttc::ClientError::TryRequest(rumqttc::Request::Publish(
                        rumqttc::Publish::new(&topic, to_rumqttc_qos(options.qos), vec![]),
                    ))
                })?;
            buf.truncate(len);

            client
                .publish(&topic, to_rumqttc_qos(options.qos), options.retain, buf)
                .await
        }
    }

    fn subscribe<const N: usize>(
        &self,
        topics: &[(&str, QoS); N],
    ) -> impl core::future::Future<Output = Result<Self::Subscription<'_, N>, Self::Error>> {
        let client = self.client.clone();
        let router = self.router.clone();
        let mut clean_session_count = self.clean_session_count.subscribe();
        // Mark current value as seen so `changed()` only fires when the
        // counter is bumped (i.e. CONNACK with session_present=false) after
        // this subscribe.
        let _ = clean_session_count.borrow_and_update();
        let topics_owned: [(&str, QoS); N] = *topics;
        async move {
            // Subscribe to all topics
            for (topic, qos) in &topics_owned {
                client.subscribe(*topic, to_rumqttc_qos(*qos)).await?;
            }

            // Register subscription for message routing
            let (tx, rx) = mpsc::channel(16);
            let filters: Vec<String> = topics_owned.iter().map(|(t, _)| t.to_string()).collect();

            let sub_id = {
                let mut router = router.lock().await;
                let id = router.next_sub_id;
                router.next_sub_id += 1;
                router.subscriptions.insert(id, (filters, tx));
                id
            };

            let topic_strings: [String; N] =
                core::array::from_fn(|i| topics_owned[i].0.to_string());

            Ok(RumqttcSubscription {
                receiver: rx,
                client,
                router,
                sub_id,
                topics: topic_strings,
                clean_session_count,
            })
        }
    }
}

/// Subscription for rumqttc.
pub struct RumqttcSubscription<const N: usize> {
    receiver: mpsc::Receiver<RumqttcMessage>,
    client: rumqttc::AsyncClient,
    router: Arc<Mutex<MessageRouter>>,
    sub_id: u64,
    topics: [String; N],
    clean_session_count: watch::Receiver<u8>,
}

impl<const N: usize> MqttSubscription for RumqttcSubscription<N> {
    type Message<'m>
        = RumqttcMessage
    where
        Self: 'm;
    type Error = rumqttc::ClientError;

    async fn next_message(&mut self) -> Option<Self::Message<'_>> {
        tokio::select! {
            msg = self.receiver.recv() => msg,
            // Bumped only on CONNACK with session_present=false (broker
            // dropped our subs). Transient disconnects and session-resume
            // reconnects don't trigger this. Caller treats `None` as
            // recoverable: drop the subscription and resubscribe.
            _ = self.clean_session_count.changed() => None,
        }
    }

    async fn unsubscribe(self) -> Result<(), Self::Error> {
        // Remove from router
        {
            let mut router = self.router.lock().await;
            router.subscriptions.remove(&self.sub_id);
        }
        // Unsubscribe from broker
        for topic in &self.topics {
            self.client.unsubscribe(topic).await?;
        }
        Ok(())
    }
}

/// Message wrapper for rumqttc (owns data, unlike mqttrust which borrows).
pub struct RumqttcMessage {
    topic: String,
    payload: Vec<u8>,
    qos: QoS,
    dup: bool,
    retain: bool,
}

impl MqttMessage for RumqttcMessage {
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

/// Convert our QoS to rumqttc's QoS.
fn to_rumqttc_qos(qos: QoS) -> rumqttc::QoS {
    match qos {
        QoS::AtMostOnce => rumqttc::QoS::AtMostOnce,
        QoS::AtLeastOnce => rumqttc::QoS::AtLeastOnce,
        QoS::ExactlyOnce => rumqttc::QoS::ExactlyOnce,
    }
}

/// Convert rumqttc's QoS to our QoS.
fn from_rumqttc_qos(qos: rumqttc::QoS) -> QoS {
    match qos {
        rumqttc::QoS::AtMostOnce => QoS::AtMostOnce,
        rumqttc::QoS::AtLeastOnce => QoS::AtLeastOnce,
        rumqttc::QoS::ExactlyOnce => QoS::ExactlyOnce,
    }
}

/// Simple topic filter matching (supports + and # wildcards).
///
/// MQTT topic matching rules:
/// - `+` matches exactly one topic level
/// - `#` matches any number of topic levels (must be last)
fn topic_matches(filter: &str, topic: &str) -> bool {
    // Exact match
    if filter == topic {
        return true;
    }

    // Multi-level wildcard at end
    if filter == "#" {
        return true;
    }

    let filter_parts: Vec<&str> = filter.split('/').collect();
    let topic_parts: Vec<&str> = topic.split('/').collect();

    let mut fi = 0;
    let mut ti = 0;

    while fi < filter_parts.len() && ti < topic_parts.len() {
        let fp = filter_parts[fi];

        if fp == "#" {
            // # matches everything remaining
            return true;
        } else if fp == "+" {
            // + matches exactly one level
            fi += 1;
            ti += 1;
        } else if fp == topic_parts[ti] {
            // Exact match for this level
            fi += 1;
            ti += 1;
        } else {
            // Mismatch
            return false;
        }
    }

    // Both must be exhausted for a match (unless filter ended with #)
    fi == filter_parts.len() && ti == topic_parts.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_matching() {
        // Exact matches
        assert!(topic_matches("a/b/c", "a/b/c"));
        assert!(!topic_matches("a/b/c", "a/b/d"));

        // Single-level wildcard
        assert!(topic_matches("a/+/c", "a/b/c"));
        assert!(topic_matches("a/+/c", "a/x/c"));
        assert!(!topic_matches("a/+/c", "a/b/d"));
        assert!(!topic_matches("a/+/c", "a/b/c/d"));

        // Multi-level wildcard
        assert!(topic_matches("#", "a/b/c"));
        assert!(topic_matches("a/#", "a/b/c"));
        assert!(topic_matches("a/b/#", "a/b/c"));
        assert!(topic_matches("a/b/#", "a/b/c/d/e"));
        assert!(!topic_matches("a/b/#", "a/x/c"));

        // Combined wildcards
        assert!(topic_matches("a/+/#", "a/b/c/d"));
        assert!(topic_matches("+/+/+", "a/b/c"));
        assert!(!topic_matches("+/+/+", "a/b/c/d"));
    }
}
