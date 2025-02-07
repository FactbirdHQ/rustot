//! ## Integration test of `AWS IoT Device defender metrics`
//!
//!
//! This test simulates publishing of metrics and expects a accepted response from aws
//!
//! The test runs through the following update sequence:
//! 1. Setup metric state
//! 2. Assert json format
//! 2. Publish metric
//! 3. Assert result from AWS

mod common;

use std::str::FromStr;

use common::credentials;
use common::network::TlsNetwork;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_mqtt::{
    self, transport::embedded_nal::NalTransport, Config, DomainBroker, Publish, State, Subscribe,
};
use futures::StreamExt;
use heapless::LinearMap;
use rustot::{
    defender_metrics::{
        data_types::{CustomMetric, Metric},
        MetricHandler,
    },
    shadows::{derive::ShadowState, Shadow, ShadowState},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use static_cell::StaticCell;

fn assert_json_format<'a>(json: &'a str) {
    log::debug!("{json}");
    let format = "{\"hed\":{\"rid\":0,\"v\":\"1.0\"},\"met\":null,\"cmet\":{\"MyMetricOfType_Number\":[{\"number\":1}],\"MyMetricOfType_NumberList\":[{\"number_list\":[1,2,3]}],\"MyMetricOfType_StringList\":[{\"string_list\":[\"value_1\",\"value_2\"]}],\"MyMetricOfType_IpList\":[{\"ip_list\":[\"172.0.0.0\",\"172.0.0.10\"]}]}}";

    assert_eq!(json, format);
}

#[tokio::test(flavor = "current_thread")]
async fn test_publish_metric() {
    env_logger::init();

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

    // Define metrics
    let mut custom_metrics: LinearMap<String, [CustomMetric; 1], 4> = LinearMap::new();

    custom_metrics
        .insert(
            String::from_str("MyMetricOfType_Number").unwrap(),
            [CustomMetric::Number(1)],
        )
        .unwrap();

    custom_metrics
        .insert(
            String::from_str("MyMetricOfType_NumberList").unwrap(),
            [CustomMetric::NumberList(&[1, 2, 3])],
        )
        .unwrap();

    custom_metrics
        .insert(
            String::from_str("MyMetricOfType_StringList").unwrap(),
            [CustomMetric::StringList(&["value_1", "value_2"])],
        )
        .unwrap();

    custom_metrics
        .insert(
            String::from_str("MyMetricOfType_IpList").unwrap(),
            [CustomMetric::IpList(&["172.0.0.0", "172.0.0.10"])],
        )
        .unwrap();

    // Build metric
    let mut metric = Metric::builder()
        .custom_metrics(custom_metrics)
        .header(Default::default())
        .build();

    // Test the json format
    let json = serde_json::to_string(&metric).unwrap();

    assert_json_format(&json);

    let mut metric_handler = MetricHandler::new(&client);

    // Publish metric with mqtt
    let mqtt_fut = async { assert!(metric_handler.publish_metric(metric, 2000).await.is_ok()) };

    let mut transport = NalTransport::new(network, broker);
    let _ = embassy_time::with_timeout(
        embassy_time::Duration::from_secs(60),
        select::select(stack.run(&mut transport), mqtt_fut),
    )
    .await;
}
