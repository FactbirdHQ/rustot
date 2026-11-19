//! `wait_delta` against a fake shadow service: copies of a delta, the GET
//! drain racing a live delta, a recreated shadow, and a drop between taking a
//! delta and acking it.

use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

use postcard::experimental::max_size::MaxSize;
use rustot_derive::shadow_root;
use serde::{Deserialize, Serialize};

use super::Shadow;
use crate::mqtt::fake_cloud::FakeCloud;
use crate::shadows::error::Error;
use crate::shadows::store::InMemory;

#[shadow_root(name = "network")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct Routed {
    value: u32,
}

const DELTA: &str = "$aws/things/thing/shadow/name/network/update/delta";
const IN_SYNC: &str = r#"{"desired":{"value":0},"reported":{"value":0}}"#;

fn delta(version: i64, value: u32) -> String {
    format!(
        r#"{{"version":{version},"timestamp":1,"state":{{"value":{value}}},"metadata":{{"value":{{"timestamp":1}}}}}}"#
    )
}

fn pending(value: u32) -> String {
    format!(
        r#"{{"desired":{{"value":{value}}},"reported":{{"value":0}},"delta":{{"value":{value}}}}}"#
    )
}

type TestShadow<'a> = Shadow<'a, 'a, Routed, FakeCloud, InMemory<Routed>>;
type WaitDelta<'s> =
    Pin<Box<dyn Future<Output = Result<(Routed, Option<DeltaRouted>), Error>> + 's>>;

/// `wait_delta` with its future type erased: holding the generic-const future
/// inside `join!`/`timeout` ICEs rustc (generic_const_exprs).
fn wait_delta<'s>(shadow: &'s TestShadow<'_>) -> WaitDelta<'s> {
    Box::pin(shadow.wait_delta())
}

async fn idle(shadow: &TestShadow<'_>) -> bool {
    tokio::time::timeout(Duration::from_millis(50), wait_delta(shadow))
        .await
        .is_err()
}

/// Overlapping subscriptions on one connection deliver one copy per
/// subscription; only the first is applied and acked.
#[tokio::test]
async fn copy_of_an_applied_delta_is_ignored() {
    let kv = InMemory::<Routed>::new();
    let cloud = FakeCloud::new("thing", 9, IN_SYNC);
    let shadow = Shadow::<Routed, _, _>::new(&kv, &cloud);
    shadow.load().await.unwrap();

    let (first, ()) = tokio::join!(wait_delta(&shadow), async {
        cloud.wait_subscribed(DELTA).await;
        cloud.deliver(DELTA, &delta(10, 5));
        cloud.deliver(DELTA, &delta(10, 5));
    });
    let (state, applied) = first.unwrap();
    assert_eq!(applied.unwrap().value, Some(5));
    assert_eq!(state.value, 5);
    assert_eq!(cloud.updates().len(), 1, "one ack for version 10");

    assert!(
        idle(&shadow).await,
        "the copy must not come back as a delta"
    );
    assert_eq!(cloud.updates().len(), 1, "the copy is not acked again");

    let (next, ()) = tokio::join!(wait_delta(&shadow), async {
        cloud.deliver(DELTA, &delta(11, 7));
    });
    assert_eq!(next.unwrap().1.unwrap().value, Some(7));
}

/// The live delta that lands while the post-subscribe GET is in flight is the
/// same change the GET already drained.
#[tokio::test]
async fn live_copy_of_the_drained_delta_is_ignored() {
    let kv = InMemory::<Routed>::new();
    let cloud = FakeCloud::new("thing", 10, &pending(5));
    let shadow = Shadow::<Routed, _, _>::new(&kv, &cloud);
    shadow.load().await.unwrap();

    let (first, ()) = tokio::join!(wait_delta(&shadow), async {
        cloud.wait_subscribed(DELTA).await;
        cloud.deliver(DELTA, &delta(10, 5));
    });
    assert_eq!(first.unwrap().1.unwrap().value, Some(5));
    assert_eq!(cloud.updates().len(), 1);

    assert!(
        idle(&shadow).await,
        "the buffered live copy must be skipped"
    );
    assert_eq!(cloud.updates().len(), 1);
}

/// A version below the applied one is either a late copy or a recreated
/// shadow (versions restart at 1); a GET decides, and its delta is applied.
#[tokio::test]
async fn older_version_resyncs_from_the_cloud() {
    let kv = InMemory::<Routed>::new();
    let cloud = FakeCloud::new("thing", 9, IN_SYNC);
    let shadow = Shadow::<Routed, _, _>::new(&kv, &cloud);
    shadow.load().await.unwrap();

    let (first, ()) = tokio::join!(wait_delta(&shadow), async {
        cloud.wait_subscribed(DELTA).await;
        cloud.deliver(DELTA, &delta(10, 5));
    });
    first.unwrap();
    let gets = cloud.gets();

    // Shadow deleted and recreated in the cloud with a new desired value.
    cloud.set_document(2, &pending(9));
    let (next, ()) = tokio::join!(wait_delta(&shadow), async {
        cloud.deliver(DELTA, &delta(2, 9));
    });
    let (state, applied) = next.unwrap();
    assert_eq!(applied.unwrap().value, Some(9));
    assert_eq!(state.value, 9);
    assert_eq!(cloud.gets(), gets + 1, "resolved by a GET");

    // Versions continue from the recreated document.
    let (after, ()) = tokio::join!(wait_delta(&shadow), async {
        cloud.deliver(DELTA, &delta(4, 3));
    });
    assert_eq!(after.unwrap().1.unwrap().value, Some(3));
}

/// A `wait_delta` dropped after it took a delta off the subscription but
/// before the ack completed: the cloud never publishes that delta again, so
/// the next call must drain it via subscribe + GET.
#[tokio::test]
async fn delta_dropped_before_its_ack_is_drained_again() {
    let kv = InMemory::<Routed>::new();
    let cloud = FakeCloud::new("thing", 9, IN_SYNC);
    let shadow = Shadow::<Routed, _, _>::new(&kv, &cloud);
    shadow.load().await.unwrap();

    cloud.hold_updates(true);
    let (dropped, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(100), wait_delta(&shadow)),
        async {
            cloud.wait_subscribed(DELTA).await;
            cloud.deliver(DELTA, &delta(10, 5));
        }
    );
    assert!(
        dropped.is_err(),
        "the ack never returns, the caller gives up"
    );
    assert!(
        shadow.subscription.lock().await.is_none(),
        "the next call must resubscribe and GET"
    );

    // Still pending in the cloud: the ack was never accepted.
    cloud.hold_updates(false);
    cloud.set_document(10, &pending(5));
    let (state, applied) = wait_delta(&shadow).await.unwrap();
    assert_eq!(applied.unwrap().value, Some(5));
    assert_eq!(state.value, 5);
}

/// After `delete_shadow` the next document starts again at version 1.
#[tokio::test]
async fn delete_forgets_the_applied_version() {
    let kv = InMemory::<Routed>::new();
    let cloud = FakeCloud::new("thing", 9, IN_SYNC);
    let shadow = Shadow::<Routed, _, _>::new(&kv, &cloud);
    shadow.load().await.unwrap();

    let (first, ()) = tokio::join!(wait_delta(&shadow), async {
        cloud.wait_subscribed(DELTA).await;
        cloud.deliver(DELTA, &delta(10, 5));
    });
    first.unwrap();
    assert_eq!(shadow.delta_version.get(), Some(10));

    shadow.delete_shadow().await.unwrap();
    assert_eq!(shadow.delta_version.get(), None);
}
