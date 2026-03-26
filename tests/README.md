# Integration Tests

These integration tests run against real AWS IoT infrastructure. They require AWS credentials, a provisioned IoT thing, and device certificates.

## Prerequisites

All tests require:

* A PKCS #12 (.pfx) identity file in `tests/secrets/identity.pfx`
* A root CA certificate in `tests/secrets/root-ca.pem`
* Environment variables:
  * `IDENTITY_PASSWORD` — password for the .pfx file
  * `AWS_HOSTNAME` — your AWS IoT endpoint (e.g. `xxxxx-ats.iot.eu-west-1.amazonaws.com`)

To create the .pfx file from PEM certificates:

```bash
openssl pkcs12 -export -out identity.pfx -inkey private.pem.key -in certificate.pem.crt -certfile root-ca.pem
```

Fleet provisioning additionally requires `tests/secrets/claim_identity.pfx` with claim credentials.

## Tests

### OTA via MQTT (`ota_mqtt.rs`)

OTA firmware update using MQTT for both control and data planes. Uses `mqttrust` (no_std/embassy MQTT client). Creates an OTA job with MQTT protocol, downloads the file as CBOR-encoded blocks over MQTT streams, verifies file integrity and signature, then runs the self-test commit flow.

```bash
cargo test --test ota_mqtt --features "ota_mqtt_data,log,std"
```

### OTA via HTTP (`ota_http.rs`)

OTA firmware update using MQTT for the control plane and HTTP for the data plane. Uses `rumqttc` (std/tokio MQTT client) for job management and `reqwest` for downloading the file via HTTP Range requests against a pre-signed S3 URL. Same verification and self-test flow as the MQTT variant.

```bash
cargo test --test ota_http --features "ota_http_reqwest,mqtt_rumqttc,log,std"
```

### Device Defender Metrics (`metric.rs`)

Publishes device defender metrics and verifies an accepted response from AWS IoT.

```bash
cargo test --test metric --features "metric_cbor,log,std"
```

### Fleet Provisioning (`provisioning.rs`)

Provisions a device using AWS IoT Fleet Provisioning with claim-based workflow and certificate signing.

```bash
cargo test --test provisioning --features "provision_cbor,log,std"
```

### Device Shadows (`shadows.rs`)

Tests device shadow operations — create, update, get, and delete — including named shadows and KV-persist integration.

```bash
cargo test --test shadows --features "std,log,shadows_kv_persist,shadows_builders"
```

## CI

All integration tests run in CI after build, unit tests, fmt, and clippy pass. See `.github/workflows/ci.yml` for the full feature set used.
