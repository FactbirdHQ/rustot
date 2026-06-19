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
//! We pool IPC subscription streams per topic inside [`GreengrassClient`]. A
//! single stream per topic is kept open and a lightweight channel is swapped
//! on each logical subscribe / unsubscribe from rustot's core, so back-to-back
//! subscribe cycles on the same topic never churn the cloud subscription.
//!
//! ## Lifecycle: linger eviction + settle guard
//!
//! Pooled streams are not kept forever — every cloud-side MQTT subscription
//! counts toward the broker's per-connection subscription limit (AWS IoT opens
//! a second client connection past 50, which a thing-policy-variable IoT policy
//! then rejects). So when a topic's last logical subscriber unsubscribes or is
//! dropped, the slot goes idle and arms a `linger` timer instead of being torn
//! down immediately. If it is reused before the timer fires, the live stream is
//! reused and nothing is terminated — eliminating the resubscribe race for the
//! common request/response pattern. Only a slot that stays idle for the full
//! `linger` window is evicted: it is removed from the pool and its forwarder is
//! aborted, dropping the [`StreamOperation`] and sending `TERMINATE_STREAM`.
//!
//! Because terminate is un-acked, a fresh subscribe to a *just-terminated*
//! topic could still overlap the in-flight teardown. A `settle` guard closes
//! that residual window: the next subscribe to a topic terminated less than
//! `settle` ago waits out the remainder before re-establishing, so the stale
//! `TERMINATE_STREAM` is processed by the core first.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::task::Poll;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::mpsc;

use crate::mqtt::{MqttMessage, MqttSubscription, PublishOptions, QoS, ToPayload};

type TopicChannel = mpsc::UnboundedSender<greengrass_ipc_rust::MqttMessage>;
type PoolMap = Arc<RwLock<HashMap<String, Arc<PooledSlot>>>>;
type TerminatedMap = Arc<Mutex<HashMap<String, Instant>>>;

/// Default time an idle pooled subscription is kept alive before its IPC
/// stream is terminated (releasing the cloud-side MQTT subscription).
const DEFAULT_LINGER: Duration = Duration::from_secs(5);

/// Default time to wait before re-subscribing to a topic whose stream was just
/// terminated, so the un-acked `TERMINATE_STREAM` is processed by the core
/// before the new subscription is established.
const DEFAULT_SETTLE: Duration = Duration::from_millis(500);

/// Per-topic pooled state. Shared between the forwarding task and every
/// logical subscriber.
struct PooledSlot {
    /// Topic this slot subscribes to; also its key in the pool map.
    topic: String,
    /// Sender for the currently active logical subscriber. `None` when the
    /// topic was previously subscribed and has since been unsubscribed.
    /// Accessed synchronously (short critical sections only).
    sender: Mutex<Option<TopicChannel>>,
    /// Bumped on every claim (subscribe). A pending eviction timer captures
    /// the generation when armed and no-ops if it has since changed — i.e.
    /// the slot was reused before the linger elapsed.
    generation: AtomicU64,
    /// Abort handle for the forwarder task. Aborting drops the IPC
    /// `StreamOperation`, whose `Drop` sends `TERMINATE_STREAM`.
    forwarder: Mutex<Option<tokio::task::AbortHandle>>,
    /// Weak handle to the shared pool so the slot can evict itself when its
    /// linger expires. Weak to avoid a pool → slot → pool reference cycle.
    pool: Weak<RwLock<HashMap<String, Arc<PooledSlot>>>>,
    /// Shared map of recently-terminated topics, written on eviction and read
    /// by the settle guard on the next subscribe.
    terminated_at: TerminatedMap,
    /// Linger duration captured from the client at construction.
    linger: Duration,
}

impl PooledSlot {
    /// Arm a linger timer after the slot goes idle. When it fires, the slot is
    /// evicted only if it has not been reused (generation unchanged) and is
    /// still idle.
    fn arm_eviction(self: &Arc<Self>) {
        let armed_gen = self.generation.load(Ordering::SeqCst);
        let slot = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(slot.linger).await;
            slot.try_evict(armed_gen);
        });
    }

    /// Evict the slot if it was not reused since `armed_gen` and is still idle.
    /// Removal from the pool and the generation check happen under the pool
    /// write lock, which also serializes against [`GreengrassClient::ensure_slot`]
    /// claiming the slot — so a slot is never handed out and evicted at once.
    fn try_evict(self: &Arc<Self>, armed_gen: u64) {
        let Some(pool) = self.pool.upgrade() else {
            return;
        };
        let mut pool = pool.write().unwrap();

        // Reused since the timer was armed, or re-claimed without bumping the
        // generation yet — either way, leave it alone.
        if self.generation.load(Ordering::SeqCst) != armed_gen {
            return;
        }
        if self.sender.lock().unwrap().is_some() {
            return;
        }
        // Only evict if we are still the pooled slot for this topic.
        match pool.get(&self.topic) {
            Some(existing) if Arc::ptr_eq(existing, self) => {}
            _ => return,
        }
        pool.remove(&self.topic);

        // Record the terminate time for the settle guard, then abort the
        // forwarder — dropping the StreamOperation sends TERMINATE_STREAM.
        self.terminated_at
            .lock()
            .unwrap()
            .insert(self.topic.clone(), Instant::now());
        if let Some(handle) = self.forwarder.lock().unwrap().take() {
            handle.abort();
        }
    }
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
    pool: PoolMap,
    /// Topics whose stream was terminated recently, with the terminate time.
    /// Read by the settle guard before re-subscribing; pruned opportunistically.
    terminated_at: TerminatedMap,
    /// How long an idle pooled subscription is kept before its stream is
    /// terminated. See module docs.
    linger: Duration,
    /// Delay applied before re-subscribing to a just-terminated topic.
    settle: Duration,
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
            terminated_at: Arc::new(Mutex::new(HashMap::new())),
            linger: DEFAULT_LINGER,
            settle: DEFAULT_SETTLE,
        }
    }

    /// Override how long an idle pooled subscription is kept before its stream
    /// is terminated (default [`DEFAULT_LINGER`]). A longer linger reduces
    /// churn; a shorter one reclaims subscriptions faster to stay under the
    /// broker's per-connection limit.
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.linger = linger;
        self
    }

    /// Override the settle delay applied before re-subscribing to a topic whose
    /// stream was just terminated (default [`DEFAULT_SETTLE`]).
    pub fn with_settle(mut self, settle: Duration) -> Self {
        self.settle = settle;
        self
    }

    /// Number of IPC subscription streams currently pooled — one per live
    /// cloud-side MQTT subscription. Useful for staying under broker limits.
    pub fn pooled_subscription_count(&self) -> usize {
        self.pool.read().unwrap().len()
    }

    /// Topics currently held in the subscription pool.
    pub fn pooled_topics(&self) -> Vec<String> {
        self.pool.read().unwrap().keys().cloned().collect()
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
        // Fast path: already pooled. Claim it (bump generation) under the write
        // lock so any pending eviction timer for this slot no-ops.
        {
            let pool = self.pool.write().unwrap();
            if let Some(slot) = pool.get(topic) {
                slot.generation.fetch_add(1, Ordering::SeqCst);
                return Ok(slot.clone());
            }
        }

        // Settle guard: if this topic's stream was terminated very recently,
        // wait out the remainder so the stale (un-acked) TERMINATE_STREAM is
        // processed by the core before we establish a new subscription.
        let settle_wait = {
            let mut term = self.terminated_at.lock().unwrap();
            let now = Instant::now();
            term.retain(|_, t| now.duration_since(*t) < self.settle);
            term.get(topic)
                .map(|t| self.settle.saturating_sub(now.duration_since(*t)))
        };
        if let Some(wait) = settle_wait
            && !wait.is_zero()
        {
            tokio::time::sleep(wait).await;
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
            topic: topic.to_string(),
            sender: Mutex::new(None),
            generation: AtomicU64::new(1),
            forwarder: Mutex::new(None),
            pool: Arc::downgrade(&self.pool),
            terminated_at: self.terminated_at.clone(),
            linger: self.linger,
        });

        // Insert into pool. Recheck in case of race — another task might
        // have subscribed to the same topic while we were awaiting.
        {
            let mut pool = self.pool.write().unwrap();
            if let Some(existing) = pool.get(topic) {
                existing.generation.fetch_add(1, Ordering::SeqCst);
                let existing = existing.clone();
                drop(stream);
                return Ok(existing);
            }
            pool.insert(topic.to_string(), slot.clone());
        }

        // Spawn the forwarder. It reads from the IPC stream and forwards each
        // message to whichever subscriber's sender is currently installed in
        // the slot. When the stream closes (e.g. IPC connection lost, or the
        // slot is evicted via `AbortHandle`) the task removes the entry — but
        // only if it is still the pooled slot — so a later subscribe reopens it.
        let slot_for_task = slot.clone();
        let pool_for_task = self.pool.clone();
        let topic_for_task = topic.to_string();
        let handle = tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = stream;
            while let Some(msg) = stream.next().await {
                let sender = { slot_for_task.sender.lock().unwrap().clone() };
                if let Some(tx) = sender {
                    let _ = tx.send(msg.message);
                }
            }
            let mut pool = pool_for_task.write().unwrap();
            if let Some(existing) = pool.get(&topic_for_task)
                && Arc::ptr_eq(existing, &slot_for_task)
            {
                pool.remove(&topic_for_task);
            }
        });
        *slot.forwarder.lock().unwrap() = Some(handle.abort_handle());

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
/// the receivers are dropped and the pooled slots' senders are cleared. Each
/// now-idle slot arms a `linger` timer; if nothing resubscribes within the
/// window the IPC stream is terminated and the cloud subscription released,
/// otherwise the live stream is reused with no cloud churn.
pub struct GreengrassSubscription {
    receivers: Vec<SubscriptionReceiver>,
}

impl GreengrassSubscription {
    /// Release this subscription. Clears the pooled slots' senders so that
    /// future messages are discarded until someone subscribes again, and arms
    /// each slot's linger timer (see [`GreengrassSubscription`]).
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
            let is_ours = matches!(&*guard, Some(current) if current.same_channel(&receiver.tx));
            if is_ours {
                *guard = None;
                drop(guard);
                // Slot is now idle. Arm the linger timer; if no one
                // resubscribes within `linger`, the stream is terminated.
                receiver.slot.arm_eviction();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A forwarder stand-in: a parked task whose abort handle we can store in
    /// the slot, mirroring the real forwarder that holds the IPC stream.
    fn dummy_forwarder() -> tokio::task::AbortHandle {
        tokio::spawn(futures::future::pending::<()>()).abort_handle()
    }

    fn make_slot(
        topic: &str,
        pool: &PoolMap,
        terminated_at: &TerminatedMap,
        linger: Duration,
    ) -> Arc<PooledSlot> {
        let slot = Arc::new(PooledSlot {
            topic: topic.to_string(),
            sender: Mutex::new(None),
            generation: AtomicU64::new(1),
            forwarder: Mutex::new(Some(dummy_forwarder())),
            pool: Arc::downgrade(pool),
            terminated_at: terminated_at.clone(),
            linger,
        });
        pool.write()
            .unwrap()
            .insert(topic.to_string(), slot.clone());
        slot
    }

    #[tokio::test]
    async fn idle_slot_is_evicted_after_linger() {
        let pool: PoolMap = Arc::new(RwLock::new(HashMap::new()));
        let terminated: TerminatedMap = Arc::new(Mutex::new(HashMap::new()));
        let slot = make_slot("t/a", &pool, &terminated, Duration::from_millis(20));

        slot.arm_eviction();
        assert!(pool.read().unwrap().contains_key("t/a"));

        tokio::time::sleep(Duration::from_millis(60)).await;

        // Evicted: removed from the pool and recorded for the settle guard.
        assert!(!pool.read().unwrap().contains_key("t/a"));
        assert!(terminated.lock().unwrap().contains_key("t/a"));
    }

    #[tokio::test]
    async fn reuse_before_linger_cancels_eviction() {
        let pool: PoolMap = Arc::new(RwLock::new(HashMap::new()));
        let terminated: TerminatedMap = Arc::new(Mutex::new(HashMap::new()));
        let slot = make_slot("t/b", &pool, &terminated, Duration::from_millis(40));

        slot.arm_eviction();
        // Simulate a resubscribe within the linger window: a claim bumps the
        // generation, so the pending timer must no-op.
        slot.generation.fetch_add(1, Ordering::SeqCst);

        tokio::time::sleep(Duration::from_millis(80)).await;

        assert!(pool.read().unwrap().contains_key("t/b"));
        assert!(terminated.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn active_slot_is_not_evicted() {
        let pool: PoolMap = Arc::new(RwLock::new(HashMap::new()));
        let terminated: TerminatedMap = Arc::new(Mutex::new(HashMap::new()));
        let slot = make_slot("t/c", &pool, &terminated, Duration::from_millis(20));

        // A subscriber is active (sender installed) — eviction must not fire.
        let (tx, _rx) = mpsc::unbounded_channel();
        *slot.sender.lock().unwrap() = Some(tx);

        slot.arm_eviction();
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert!(pool.read().unwrap().contains_key("t/c"));
    }

    #[tokio::test]
    async fn evicted_slot_does_not_remove_a_replacement() {
        let pool: PoolMap = Arc::new(RwLock::new(HashMap::new()));
        let terminated: TerminatedMap = Arc::new(Mutex::new(HashMap::new()));
        let old = make_slot("t/d", &pool, &terminated, Duration::from_millis(20));

        // A fresh slot replaces the old one in the pool (same topic).
        let new = make_slot("t/d", &pool, &terminated, Duration::from_millis(20));
        assert!(Arc::ptr_eq(pool.read().unwrap().get("t/d").unwrap(), &new));

        // The old slot's timer firing must not evict the replacement.
        old.try_evict(old.generation.load(Ordering::SeqCst));

        assert!(Arc::ptr_eq(pool.read().unwrap().get("t/d").unwrap(), &new));
    }
}
