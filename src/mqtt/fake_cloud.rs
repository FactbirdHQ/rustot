//! A fake AWS IoT shadow service for tests that need messages to flow.
//!
//! Answers `get`, `update` and `delete` like the Device Shadow service (accepted
//! response with the document version, the request's `clientToken` echoed)
//! and lets a test deliver arbitrary messages, e.g. the same delta twice.

use core::convert::Infallible;
use core::future::Future;
use std::sync::Mutex;

use tokio::sync::mpsc;

use super::mock::MockMessage;
use super::{MqttClient, MqttSubscription, PublishOptions, QoS, ToPayload};

struct Sub {
    filters: Vec<String>,
    tx: mpsc::UnboundedSender<MockMessage>,
}

struct Inner {
    subs: Vec<Sub>,
    /// `state` object returned by GET.
    get_state: String,
    /// Current document version.
    version: i64,
    gets: usize,
    updates: Vec<String>,
    /// Swallow `update` requests without answering (lost response).
    hold_updates: bool,
}

pub struct FakeCloud {
    client_id: String,
    inner: Mutex<Inner>,
}

/// MQTT topic filter match with `+` and `#`.
fn matches(filter: &str, topic: &str) -> bool {
    let mut f = filter.split('/');
    let mut t = topic.split('/');
    loop {
        match (f.next(), t.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(_)) => {}
            (Some(a), Some(b)) if a == b => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

impl FakeCloud {
    pub fn new(client_id: &str, version: i64, get_state: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            inner: Mutex::new(Inner {
                subs: Vec::new(),
                get_state: get_state.to_string(),
                version,
                gets: 0,
                updates: Vec::new(),
                hold_updates: false,
            }),
        }
    }

    /// What the next GET returns.
    pub fn set_document(&self, version: i64, get_state: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.version = version;
        inner.get_state = get_state.to_string();
    }

    pub fn hold_updates(&self, hold: bool) {
        self.inner.lock().unwrap().hold_updates = hold;
    }

    pub fn gets(&self) -> usize {
        self.inner.lock().unwrap().gets
    }

    pub fn updates(&self) -> Vec<String> {
        self.inner.lock().unwrap().updates.clone()
    }

    /// Deliver to every live subscription matching `topic`, once per matching
    /// subscription, as the broker does.
    pub fn deliver(&self, topic: &str, payload: &str) {
        let inner = self.inner.lock().unwrap();
        for sub in &inner.subs {
            if sub.filters.iter().any(|f| matches(f, topic)) {
                let _ = sub.tx.send(MockMessage::new(topic, payload.as_bytes()));
            }
        }
    }

    /// Wait until a live subscription covers `topic`.
    pub async fn wait_subscribed(&self, topic: &str) {
        loop {
            {
                let inner = self.inner.lock().unwrap();
                if inner
                    .subs
                    .iter()
                    .any(|s| !s.tx.is_closed() && s.filters.iter().any(|f| matches(f, topic)))
                {
                    return;
                }
            }
            tokio::time::sleep(core::time::Duration::from_millis(1)).await;
        }
    }

    fn handle_publish(&self, topic: &str, payload: &[u8]) {
        let reply = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(base) = topic.strip_suffix("/get") {
                inner.gets += 1;
                Some((
                    format!("{base}/get/accepted"),
                    format!(
                        r#"{{"state":{},"version":{},"timestamp":1}}"#,
                        inner.get_state, inner.version
                    ),
                ))
            } else if let Some(base) = topic.strip_suffix("/update") {
                let body = String::from_utf8_lossy(payload).to_string();
                inner.updates.push(body.clone());
                if inner.hold_updates {
                    None
                } else {
                    inner.version += 1;
                    let token = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("clientToken").cloned())
                        .unwrap_or(serde_json::Value::Null);
                    Some((
                        format!("{base}/update/accepted"),
                        format!(
                            r#"{{"state":{{}},"clientToken":{},"version":{},"timestamp":1}}"#,
                            token, inner.version
                        ),
                    ))
                }
            } else if let Some(base) = topic.strip_suffix("/delete") {
                let deleted = inner.version;
                // A recreated document starts again at version 1.
                inner.version = 0;
                Some((
                    format!("{base}/delete/accepted"),
                    format!(r#"{{"version":{deleted},"timestamp":1}}"#),
                ))
            } else {
                None
            }
        };
        if let Some((topic, payload)) = reply {
            self.deliver(&topic, &payload);
        }
    }

    fn subscribe_filters(&self, filters: Vec<String>) -> FakeSubscription {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut inner = self.inner.lock().unwrap();
        inner.subs.retain(|s| !s.tx.is_closed());
        inner.subs.push(Sub { filters, tx });
        FakeSubscription { rx }
    }
}

pub struct FakeSubscription {
    rx: mpsc::UnboundedReceiver<MockMessage>,
}

impl MqttSubscription for FakeSubscription {
    type Message<'m>
        = MockMessage
    where
        Self: 'm;
    type Error = Infallible;

    fn next_message(&mut self) -> impl Future<Output = Option<Self::Message<'_>>> {
        self.rx.recv()
    }

    fn unsubscribe(self) -> impl Future<Output = Result<(), Self::Error>> {
        async { Ok(()) }
    }
}

fn encode<P: ToPayload>(payload: &P) -> Vec<u8> {
    let mut buf = vec![0u8; payload.max_size()];
    let len = payload.encode(&mut buf).expect("payload encodes");
    buf.truncate(len);
    buf
}

impl MqttClient for FakeCloud {
    type Subscription<'m, const N: usize>
        = FakeSubscription
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
        topic: &str,
        payload: P,
        _options: PublishOptions,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        self.handle_publish(topic, &encode(&payload));
        async { Ok(()) }
    }

    fn subscribe<const N: usize>(
        &self,
        topics: &[(&str, QoS); N],
    ) -> impl Future<Output = Result<Self::Subscription<'_, N>, Self::Error>> {
        let sub = self.subscribe_filters(topics.iter().map(|(t, _)| t.to_string()).collect());
        async { Ok(sub) }
    }
}

/// The multi-shadow manager owns its client; share one fake between it and
/// the test.
impl MqttClient for &FakeCloud {
    type Subscription<'m, const N: usize>
        = FakeSubscription
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
        topic: &str,
        payload: P,
        _options: PublishOptions,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        self.handle_publish(topic, &encode(&payload));
        async { Ok(()) }
    }

    fn subscribe<const N: usize>(
        &self,
        topics: &[(&str, QoS); N],
    ) -> impl Future<Output = Result<Self::Subscription<'_, N>, Self::Error>> {
        let sub = self.subscribe_filters(topics.iter().map(|(t, _)| t.to_string()).collect());
        async { Ok(sub) }
    }
}

#[test]
fn topic_filters_match_like_mqtt() {
    assert!(matches("a/+/c", "a/b/c"));
    assert!(matches("a/#", "a/b/c"));
    assert!(!matches("a/+/c", "a/b/d"));
    assert!(!matches("a/b", "a/b/c"));
}
