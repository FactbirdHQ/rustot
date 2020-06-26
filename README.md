# Rust of things (rustot)


**Work in progress**

> no_std, no_alloc crate for AWS IoT Devices, implementing Jobs, OTA, Device Defender and IoT Shadows

![CI][workflow]
<!-- [![Crates.io Version][crates-io-badge]][crates-io]
[![Crates.io Downloads][crates-io-download-badge]][crates-io-download]
[![chat][chat-badge]][chat] -->

Any contributions will be welcomed! Even if they are just suggestions, bugs or reviews!

This is a port of the Amazon-FreeRTOS AWS IoT Device SDK (https://github.com/nguyenvuhung/amazon-freertos/tree/master/libraries/freertos_plus/aws/ota), written in pure Rust.

It is written to work with [MqttRust](https://github.com/BlackbirdHQ/mqttrust), but should work with any other mqtt client, that implements the [Mqtt trait](https://github.com/BlackbirdHQ/mqttrust/blob/a63084212f24177695e9994971ae94c8c71f4200/src/client.rs#L14) from MqttRust.


## Tests

> The crate is covered by tests. These tests can be run by `cargo test --tests --all-features`, and are run by the CI on every push to master.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
 http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.


<!-- Badges -->
[workflow]: https://github.com/BlackbirdHQ/rustot/workflows/CI/badge.svg
<!-- [crates-io]: https://crates.io/crates/rustot -->
<!-- [crates-io-badge]: https://img.shields.io/crates/v/rustot.svg?maxAge=3600
[crates-io-download]: https://crates.io/crates/rustot
[crates-io-download-badge]: https://img.shields.io/crates/d/rustot.svg?maxAge=3600 -->
