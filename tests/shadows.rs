//!
//! ## Integration test of `AWS IoT Shadows`
//!
//!
//! This test simulates updates of the shadow state from both device side &
//! cloud side. Cloud side updates are done by publishing directly to the shadow
//! topics, and ignoring the resulting update accepted response. Device side
//! updates are done through the shadow API provided by this crate.
//!
//! The test runs through the following update sequence:
//! 1. Setup clean starting point (`desired = null, reported = null`)
//! 2. Do a `GetShadow` request to sync empty state
//! 3. Update to initial shadow state from the device
//! 4. Assert on the initial state
//! 5. Update state from device
//! 6. Assert on shadow state
//! 7. Update state from cloud
//! 8. Assert on shadow state
//! 9. Update state from device
//! 10. Assert on shadow state
//! 11. Update state from cloud
//! 12. Assert on shadow state
//!
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

mod common;

use common::credentials;
use common::network::TlsNetwork;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_mqtt::{
    self, transport::embedded_nal::NalTransport, Config, DomainBroker, Publish, QoS, State,
    Subscribe, SubscribeTopic,
};
use futures::StreamExt;
use rustot::shadows::{derive::ShadowState, Shadow};
use serde::{Deserialize, Serialize};
use static_cell::StaticCell;

#[derive(Debug, Default, Serialize, Deserialize, ShadowState, PartialEq)]
#[shadow("state")]
pub struct TestShadow {
    foo: u32,
    // #[serde(skip_serializing_if = "Option::is_none")]
    // bar: Option<bool>,
}

#[tokio::test(flavor = "current_thread")]
async fn test_shadow_update_from_device() {
    env_logger::init();

    const DESIRED_1: &str = r#"{
    "state": {
        "desired": {
            "foo": 42
        }
    },
    "metadata": {
        "foo": {
            "timestamp": 1672047508
        }
    },
    "version": 2,
    "timestamp": 1672047508
}"#;

    let (thing_name, identity) = credentials::identity();
    let hostname = credentials::HOSTNAME.unwrap();

    static NETWORK: StaticCell<TlsNetwork> = StaticCell::new();
    let network = NETWORK.init(TlsNetwork::new(hostname.to_owned(), identity));

    // Create the MQTT stack
    let broker =
        DomainBroker::<_, 128>::new(format!("{}:8883", hostname).as_str(), network).unwrap();

    let config = Config::builder()
        .client_id(thing_name.try_into().unwrap())
        .keepalive_interval(embassy_time::Duration::from_secs(50))
        .build();
    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 10 }, 4>> = StaticCell::new();
    let state = STATE.init(State::<NoopRawMutex, 4096, { 4096 * 10 }, 4>::new());
    let (mut stack, client) = embedded_mqtt::new(state, config);

    // Create the shadow
    let mut shadow = Shadow::<TestShadow, _, 4>::new(TestShadow::default(), &client);

    let mqtt_fut = async {
        let mut update_subscription = client
            .subscribe::<2>(
                Subscribe::builder()
                    .topics(&[SubscribeTopic::builder()
                        .topic_path(
                            rustot::shadows::topics::Topic::Update
                                .format::<128>(client.client_id(), Some("state"))
                                .unwrap()
                                .as_str(),
                        )
                        .build()])
                    .build(),
            )
            .await
            .unwrap();

        let mut get_subscription = client
            .subscribe::<2>(
                Subscribe::builder()
                    .topics(&[SubscribeTopic::builder()
                        .topic_path(
                            rustot::shadows::topics::Topic::Get
                                .format::<128>(client.client_id(), Some("state"))
                                .unwrap()
                                .as_str(),
                        )
                        .build()])
                    .build(),
            )
            .await
            .unwrap();

        // Force a shadow get first, to sync up state
        client
            .publish(
                Publish::builder()
                    .topic_name(
                        rustot::shadows::topics::Topic::Get
                            .format::<128>(client.client_id(), Some("state"))
                            .unwrap()
                            .as_str(),
                    )
                    .qos(QoS::AtLeastOnce)
                    .payload(&[])
                    .build(),
            )
            .await
            .unwrap();

        // Wait for the device to try to fetch the shadow first.
        let _ = get_subscription.next().await;

        // Initial shadow state update
        log::info!("Doing initial shadow update");
        client
            .publish(
                Publish::builder()
                    .topic_name(
                        rustot::shadows::topics::Topic::Update
                            .format::<128>(client.client_id(), Some("state"))
                            .unwrap()
                            .as_str(),
                    )
                    .payload(DESIRED_1.as_bytes())
                    .qos(QoS::AtLeastOnce)
                    .build(),
            )
            .await
            .unwrap();

        loop {
            select::select(update_subscription.next(), async {
                // Device-side update 1
                shadow
                    .update(|_, desired| {
                        desired.foo = Some(1337);
                    })
                    .await
                    .unwrap();

                let current = shadow.get();
                assert_eq!(current.foo, 1337);
                let payload = serde_json_core::to_string::<_, 512>(current).unwrap();
                log::info!("ASSERT-DEVICE: {:?}", payload);

                // Cloud-side update 1
                client
                    .publish(
                        Publish::builder()
                            .topic_name(
                                rustot::shadows::topics::Topic::Update
                                    .format::<128>(client.client_id(), Some("state"))
                                    .unwrap()
                                    .as_str(),
                            )
                            .payload(r#"{"state": {"desired": {"bar": true}}}"#.as_bytes())
                            .qos(QoS::AtLeastOnce)
                            .build(),
                    )
                    .await
                    .unwrap();

                // Device-side update 2
                // shadow
                //     .update(|_state, desired| {
                //         // desired.bar = Some(false);
                //     })
                //     .await
                //     .unwrap();

                // let current = shadow.get();
                // let payload = serde_json_core::to_string::<_, 512>(current).unwrap();
                // log::info!("ASSERT-DEVICE: {}", payload);
                // assert_eq!(current.bar, Some(false));

                // Cloud-side update 2
                client
                    .publish(
                        Publish::builder()
                            .topic_name(
                                rustot::shadows::topics::Topic::Update
                                    .format::<128>(client.client_id(), Some("state"))
                                    .unwrap()
                                    .as_str(),
                            )
                            .payload(r#"{"state": {"desired": {"foo": 100}}}"#.as_bytes())
                            .qos(QoS::AtLeastOnce)
                            .build(),
                    )
                    .await
                    .unwrap();

                let (s, _) = shadow.wait_delta().await.unwrap();
                let payload = serde_json_core::to_string::<_, 512>(s).unwrap();
                log::info!("ASSERT-DEVICE: {:?}", payload);
                assert_eq!(s.foo, 100);
            })
            .await;
        }
    };

    let mut transport = NalTransport::new(network, broker);
    let _ = embassy_time::with_timeout(
        embassy_time::Duration::from_secs(60),
        select::select(stack.run(&mut transport), mqtt_fut),
    )
    .await;
}
