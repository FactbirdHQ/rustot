# AWS IoT Device Defender (Metrics)

Publish device-side security and operational metrics to AWS IoT Device
Defender. The cloud aggregates them, applies behavior profiles, and raises
alarms on deviation.

See the [AWS IoT Device Defender documentation][aws-docs] for the full
service specification (reserved metrics, behavior profiles, audit findings).
This module covers the device-side **publish-metrics** flow only.

[aws-docs]: https://docs.aws.amazon.com/iot/latest/developerguide/device-defender.html

## Workflow

1. **Build** a [`Metric`] with a [`Header`] (`report_id` + `version`),
   optional standard [`Metrics`] (TCP/UDP listeners, network stats, TCP
   connections), and optional `custom_metrics` of your own type.
2. **Publish** via [`MetricHandler::publish_metric`]. The handler subscribes
   to the `accepted` / `rejected` reply topics first, then publishes, then
   awaits the cloud's reply.
3. The handler returns `Ok(())` on `accepted`, or [`MetricError`] on
   rejection or transport failure.

## Topic table

| Direction | Purpose | Topic |
|---|---|---|
| Pub | Publish report | `$aws/things/{Thing}/defender/metrics/{format}` |
| Sub | Cloud accepted | `$aws/things/{Thing}/defender/metrics/{format}/accepted` |
| Sub | Cloud rejected | `$aws/things/{Thing}/defender/metrics/{format}/rejected` |

`{format}` is `cbor` when the `metric_cbor` feature is enabled, `json`
otherwise.

## Standard vs custom metrics

[`Metrics`] carries the AWS-defined standard metrics (`listening_tcp_ports`,
`listening_udp_ports`, `network_stats`, `tcp_connections`). Use the helpers
in [`crate::defender_metrics::aws_types`] to build them.

`custom_metrics` is generic over any `Serialize` type. The
[`CustomMetric`] enum covers the four AWS-supported value kinds:

| Variant | JSON shape | Purpose |
|---|---|---|
| `CustomMetric::Number(i64)` | `{"number": 1}` | Single numeric value |
| `CustomMetric::NumberList(&[u64])` | `{"number_list": [1,2,3]}` | List of numbers |
| `CustomMetric::StringList(&[&str])` | `{"string_list": ["a","b"]}` | List of strings |
| `CustomMetric::IpList(&[&str])` | `{"ip_list": ["172.0.0.1"]}` | List of IPs |

Each custom metric must be wrapped in a single-element array
(`[CustomMetric; 1]`) to match the AWS schema; the [`LinearMap`] keys are the
metric names you registered via `CreateCustomMetric`.

## Example

```rust
use core::str::FromStr;
use heapless::{LinearMap, String};
use rustot::defender_metrics::{
    MetricHandler,
    data_types::{CustomMetric, Header, Metric},
};

async fn report<C: rustot::mqtt::MqttClient>(mqtt: &C) {
    // Custom metrics keyed by the names registered with CreateCustomMetric.
    let mut custom: LinearMap<String<32>, [CustomMetric; 1], 4> = LinearMap::new();
    custom.insert(
        String::from_str("Battery_Level").unwrap(),
        [CustomMetric::Number(85)],
    ).unwrap();
    custom.insert(
        String::from_str("Wifi_Signal_dBm").unwrap(),
        [CustomMetric::Number(-62)],
    ).unwrap();

    let metric = Metric::builder()
        .header(Header::default())          // report_id from monotonic clock, v=1.0
        .custom_metrics(custom)
        .build();

    let handler = MetricHandler::new(mqtt);
    handler.publish_metric(metric, 2048).await.expect("defender publish");
}
```

The `2048` is the maximum payload size the engine is allowed to allocate on
the stack for the encoded report — size it for the largest report you'll
publish.

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `metric_cbor` | yes | Encode reports as CBOR via `minicbor-serde`; topic suffix becomes `cbor`. Disable for JSON. |

Only one format is active per build, mirroring `provision_cbor` /
`commands_cbor`.

## Limitations

- **No on-device audit support.** Device Defender audits run server-side; this
  module only handles the metrics-publish topic.
- **`Header.report_id` defaults to the monotonic `embassy_time::Instant`**
  rather than wall-clock epoch milliseconds. Override
  `Header { report_id, version }` if you have an RTC and want true epoch ids.

## Testing

Unit tests cover JSON / CBOR serialization for each `CustomMetric` variant
and the `Version` text format:

```bash
cargo test --lib defender_metrics::
```

The integration test (`tests/metric.rs`) publishes a representative custom
report against a real AWS IoT endpoint and asserts an `accepted` reply:

```bash
RUST_LOG=trace \
  THING_NAME=MyTestThing \
  AWS_HOSTNAME=xxxxxxxx-ats.iot.eu-west-1.amazonaws.com \
  cargo test --test metric --features "metric_cbor,log"
```

`tests/secrets/identity.pfx` must hold a valid AWS IoT identity for the
thing; `IDENTITY_PASSWORD` may be set if the file is password-protected.

[`MetricHandler::publish_metric`]: https://docs.rs/rustot/latest/rustot/defender_metrics/struct.MetricHandler.html#method.publish_metric
[`Metric`]: https://docs.rs/rustot/latest/rustot/defender_metrics/data_types/struct.Metric.html
[`Metrics`]: https://docs.rs/rustot/latest/rustot/defender_metrics/data_types/struct.Metrics.html
[`Header`]: https://docs.rs/rustot/latest/rustot/defender_metrics/data_types/struct.Header.html
[`CustomMetric`]: https://docs.rs/rustot/latest/rustot/defender_metrics/data_types/enum.CustomMetric.html
[`MetricError`]: https://docs.rs/rustot/latest/rustot/defender_metrics/errors/enum.MetricError.html
[`LinearMap`]: https://docs.rs/heapless/latest/heapless/struct.LinearMap.html
[`crate::defender_metrics::aws_types`]: https://docs.rs/rustot/latest/rustot/defender_metrics/aws_types/index.html
