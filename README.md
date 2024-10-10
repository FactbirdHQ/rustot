# Rust of Things (rustot)

**Work in progress**

> A `no_std`, `no_alloc` crate for interacting with AWS IoT services on embedded devices.

This crate aims to provide a pure-Rust implementation of essential AWS IoT features for embedded systems, inspired by the Amazon FreeRTOS AWS IoT Device SDK.

## Features

* **OTA Updates:** ([`ota`] module)
    * Download and apply firmware updates securely over MQTT or HTTP.
    * Supports both CBOR and raw binary firmware formats.
* **Device Shadow:** ([`shadow`] module)
    * Synchronize device state with the cloud using AWS IoT Device Shadow service.
    * Get, update, and delete device shadows.
* **Jobs:** ([`jobs`] module)
    * Receive and execute jobs remotely on your devices.
    * Track job status and report progress to AWS IoT.
* **Device Defender:** ([`defender`] module)
    * Implement security best practices and detect anomalies on your devices.
* **Fleet Provisioning:** ([`provisioning`] module)
    * Securely provision and connect devices to AWS IoT at scale.
* **Lightweight and `no_std`:**  Designed specifically for resource-constrained embedded devices.


## Contributing

Contributions, suggestions, bug reports, and reviews are highly appreciated!
Please refer to [CONTRIBUTING.md](CONTRIBUTING.md) for more information on how
to contribute.

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
