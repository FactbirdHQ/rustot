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
    self, transport::embedded_nal::NalTransport, Config, DomainBroker, MqttClient, Publish, QoS,
    State, Subscribe, SubscribeTopic,
};
use futures::StreamExt;
use rustot::shadows::{derive::ShadowState, Shadow, ShadowState};
use serde::{Deserialize, Serialize};
use serde_json::json;
use static_cell::StaticCell;

#[derive(Debug, Default, Serialize, Deserialize, ShadowState, PartialEq)]
#[shadow("state")]
pub struct TestShadow {
    foo: u32,
    // #[serde(skip_serializing_if = "Option::is_none")]
    // bar: Option<bool>,
}

/// Helper function to mimic cloud side updates using MQTT client directly
async fn cloud_update(client: &MqttClient<'static, NoopRawMutex>, payload: &[u8]) {
    client
        .publish(
            Publish::builder()
                .topic_name(
                    rustot::shadows::topics::Topic::Update
                        .format::<128>(client.client_id(), TestShadow::NAME)
                        .unwrap()
                        .as_str(),
                )
                .payload(payload)
                .qos(QoS::AtLeastOnce)
                .build(),
        )
        .await
        .unwrap();
}

/// Helper function to assert on the current shadow state
async fn assert_shadow(client: &MqttClient<'static, NoopRawMutex>, expected: serde_json::Value) {
    let mut get_shadow_sub = client
        .subscribe::<1>(
            Subscribe::builder()
                .topics(&[SubscribeTopic::builder()
                    .topic_path(
                        rustot::shadows::topics::Topic::GetAccepted
                            .format::<128>(client.client_id(), TestShadow::NAME)
                            .unwrap()
                            .as_str(),
                    )
                    .build()])
                .build(),
        )
        .await
        .unwrap();

    client
        .publish(
            Publish::builder()
                .topic_name(
                    rustot::shadows::topics::Topic::Get
                        .format::<128>(client.client_id(), TestShadow::NAME)
                        .unwrap()
                        .as_str(),
                )
                .payload(b"")
                .build(),
        )
        .await
        .unwrap();

    let current_shadow = get_shadow_sub.next().await.unwrap();

    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(current_shadow.payload())
            .unwrap()
            .get("state")
            .unwrap(),
        &expected,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_shadow_update_from_device() {
    env_logger::init();

    const DESIRED_1: &str = r#"{
        "state": {
            "desired": {
                "foo": 42
            }
        }
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

    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 10 }>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = embedded_mqtt::new(state, config);

    // Create the shadow
    let mut shadow = Shadow::<TestShadow, _>::new(TestShadow::default(), &client);

    // let delta_fut = async {
    //     loop {
    //         let delta = shadow.wait_delta().await.unwrap();
    //     }
    // };

    let mqtt_fut = async {
        // 1. Setup clean starting point (`desired = null, reported = null`)
        cloud_update(
            &client,
            r#"{"state": {"desired": null, "reported": null} }"#.as_bytes(),
        )
        .await;

        // 2. Do a `GetShadow` request to sync empty state
        let _ = shadow.get_shadow().await.unwrap();

        // 3. Update to initial shadow state from the device
        let _ = shadow.report().await.unwrap();

        // 4. Assert on the initial state
        assert_shadow(
            &client,
            json!({
                "reported": {
                    "foo": 0
                }
            }),
        )
        .await;

        // 5. Update state from device
        // 6. Assert on shadow state
        // 7. Update state from cloud
        cloud_update(&client, DESIRED_1.as_bytes()).await;

        // 8. Assert on shadow state
        // 9. Update state from device

        // 10. Assert on shadow state
        assert_shadow(
            &client,
            json!({
                "reported": {
                    "foo": 0
                },
                "desired": {
                    "foo": 42
                },
                "delta": {
                    "foo": 42
                }
            }),
        )
        .await;

        // 11. Update desired state from cloud
        cloud_update(
            &client,
            r#"{"state": {"desired": {"bar": true}}}"#.as_bytes(),
        )
        .await;

        // 12. Assert on shadow state
        assert_shadow(
            &client,
            json!({
                "reported": {
                    "foo": 0
                },
                "desired": {
                    "foo": 42,
                    "bar": true
                },
                "delta": {
                    "foo": 42,
                    "bar": true
                }
            }),
        )
        .await;
    };

    let mut transport = NalTransport::new(network, broker);
    let _ = embassy_time::with_timeout(
        embassy_time::Duration::from_secs(60),
        select::select(stack.run(&mut transport), mqtt_fut),
    )
    .await;
}
