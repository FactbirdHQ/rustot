# Rust of Things (rustot)

> A `no_std`, `no_alloc` crate for talking to AWS IoT services from embedded devices.

`rustot` is a pure-Rust, async-first implementation of the device-side AWS IoT
service protocols. It is designed for resource-constrained MCUs (think
embassy + ESP32 / nRF / STM32) but also runs unchanged on `std` targets via
`tokio` for testing.

The crate aims for parity with the equivalent surface area of the [Amazon
FreeRTOS AWS IoT Device SDK], spelt in idiomatic Rust.

[Amazon FreeRTOS AWS IoT Device SDK]: https://github.com/aws/aws-iot-device-sdk-embedded-C

## Supported services

| Module | AWS service | Description |
|---|---|---|
| [`jobs`](src/jobs) | [Jobs] | Receive remote operations, report progress, drive a job state machine. |
| [`commands`](src/commands) | [Commands] | One-shot, cloud-to-device instructions with optional progress reporting and ack handshake. |
| [`shadows`](src/shadows) | [Device Shadow] | Synchronise reported / desired state with the cloud. Supports classic and named shadows, plus a multi-shadow manager. |
| [`provisioning`](src/provisioning) | [Fleet Provisioning] | Provisioning by claim — exchange a bootstrap certificate for a per-device certificate. |
| [`defender_metrics`](src/defender_metrics) | [Device Defender] | Publish device-side security metrics. |
| [`transfer`](src/transfer) | OTA on top of Jobs | Generic file transfer (`Transfer::perform`) and OTA firmware updates (`Transfer::perform_ota`) with self-test / image-state hooks, over MQTT or HTTP. The OTA path is wire-compatible with the Amazon FreeRTOS [`afr_ota`][afr] job-document format produced by the [`CreateOTAUpdate`][create-ota] control-plane API. |

[Jobs]: https://docs.aws.amazon.com/iot/latest/developerguide/iot-jobs.html
[Commands]: https://docs.aws.amazon.com/iot/latest/developerguide/iot-remote-command.html
[Device Shadow]: https://docs.aws.amazon.com/iot/latest/developerguide/iot-device-shadows.html
[Fleet Provisioning]: https://docs.aws.amazon.com/iot/latest/developerguide/provision-wo-cert.html
[Device Defender]: https://docs.aws.amazon.com/iot/latest/developerguide/device-defender.html
[afr]: https://docs.aws.amazon.com/freertos/latest/userguide/ota-mqtt-protocol.html
[create-ota]: https://docs.aws.amazon.com/iot/latest/apireference/API_CreateOTAUpdate.html

## Design

- **`no_std`, `no_alloc`** — every API uses `heapless`, borrowed `&str`, and
  caller-provided buffers. No dynamic allocation in the hot path.
- **Backend-agnostic MQTT** — service modules are written against the
  `crate::mqtt::MqttClient` trait. Pick one backend per build:
  - `mqtt_mqttrust`   — bare-metal / embassy
  - `mqtt_rumqttc`    — std + tokio (testing, gateways)
  - `mqtt_greengrass` — AWS Greengrass IPC
- **Async, with no executor lock-in** — futures + `embassy-time` for
  timeouts. Works with `embassy-executor`, `tokio`, or any other.
- **Generic over user types** — services that carry a payload (Jobs job
  documents, Commands request payloads, Shadow state) are parameterised by a
  user `Serialize` / `Deserialize` type. Bring your own enum.

## Toolchain

Requires nightly Rust. The crate uses `generic_const_exprs` for compile-time
topic-length sizing.

```bash
rustup toolchain install nightly
rustup override set nightly  # in the project directory
```

## Cargo features

### Service-level

| Feature | Default | Effect |
|---|---|---|
| `transfer_mqtt`    | yes | Enable MQTT-based file transfer / OTA. |
| `transfer_http`    | no  | Enable HTTP-based file transfer (no client). |
| `transfer_http_reqwest` | no | HTTP transfer via `reqwest` (`std`-only). |
| `metric_cbor`      | yes | Defender Metrics: CBOR payloads (else JSON). |
| `provision_cbor`   | yes | Fleet Provisioning: CBOR payloads (else JSON). |
| `commands_cbor`    | yes | Commands: CBOR payloads (else JSON). |
| `shadows_kv_persist`  | yes | Field-level shadow persistence to a KV store. |
| `sequential_storage`  | no  | Flash-backed KV store via `sequential-storage`. |
| `shadows_builders`    | no  | Generate `bon` builder methods on shadow structs. |
| `shadows_multi`       | no  | Runtime-named shadows with wildcard subscriptions (requires `std`). |

### MQTT backend (pick exactly one)

| Feature | When to use |
|---|---|
| `mqtt_mqttrust`   | `no_std` embedded device with a `mqttrust` client. **Default.** |
| `mqtt_rumqttc`    | `std` host (e.g. integration tests, gateways) using `rumqttc`. |
| `mqtt_greengrass` | Running inside an AWS Greengrass nucleus via IPC. |

### Build / runtime

| Feature | Effect |
|---|---|
| `std`     | Enable `std`-only paths (file-backed KV, tokio-based test infra). |
| `defmt`   | Derive `defmt::Format` on public types and route logs through `defmt`. |
| `log`     | Route logs through the `log` crate. |

## Per-service usage

Each service module ships its own README with workflow, topic table, and a
worked example:

- [`commands`](src/commands/README.md)
- [`jobs`](src/jobs/README.md)
- [`shadows`](src/shadows/README.md)
- [`provisioning`](src/provisioning/README.md)
- [`defender_metrics`](src/defender_metrics/README.md)
- [`transfer`](src/transfer/README.md)

Integration tests under `tests/` (`ota_mqtt.rs`, `provisioning.rs`,
`shadows.rs`, …) double as end-to-end usage examples against real AWS IoT.

## Testing

Unit tests run in-tree:

```bash
cargo test --lib
```

Integration tests under `tests/` exercise real AWS IoT endpoints. They expect
a `tests/secrets/identity.pfx` (or `claim_identity.pfx` for provisioning) and
environment variables (`THING_NAME`, `AWS_HOSTNAME`, …); see each test's
header for specifics.

## Contributing

Contributions, suggestions, bug reports, and reviews are highly appreciated.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
