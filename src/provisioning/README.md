# AWS IoT Fleet Provisioning

Securely exchange a bootstrap "claim" certificate (shared across a fleet) for
a unique per-device certificate when the device first connects. The
per-device certificate is then used for all subsequent AWS IoT operations.

This module implements **provisioning by claim**. **Provisioning by trusted
user** is out of scope.

See the [AWS IoT Fleet Provisioning documentation][aws-docs] for service
architecture, IAM/policy setup, and pre-provisioning hooks.

[aws-docs]: https://docs.aws.amazon.com/iot/latest/developerguide/provision-wo-cert.html

## Cloud-side prerequisites

Done once, before devices are deployed:

1. `CreateProvisioningTemplate` — creates a fleet-provisioning template (also
   doable in the AWS IoT console under *Onboard → Fleet provisioning
   templates*).
2. Generate a claim certificate + private key.
3. Register the claim certificate with AWS IoT and attach an IoT policy that
   restricts the certificate to provisioning operations only.
4. Attach the `AWSIoTThingsRegistration` managed policy to the provisioning
   IAM role.
5. Manufacture devices with the claim certificate + private key embedded.

`tests/provisioning_infrastructure/` contains the basic AWS infrastructure
files (template, policy, pre-provisioning Lambda) used by the integration
test. **Note**: the example pre-provisioning hook blindly accepts any
request — replace it with your own validation logic in production.

## Workflow (device side)

1. Connect to AWS IoT MQTT using the claim certificate.
2. Call [`FleetProvisioner::provision`] (or [`provision_csr`] /
   [`provision_cbor`]). The function:
   - Subscribes to the create-cert + register-thing reply topics.
   - Publishes `CreateKeysAndCertificate` (or `CreateCertificateFromCsr`).
   - Receives the new certificate; calls your [`CredentialHandler`] to
     persist it.
   - Publishes `RegisterThing` with the template name, ownership token, and
     any user-supplied parameters.
   - Returns the provisioned device configuration (deserialized into your
     own `DeserializeOwned` type) on success.
3. Reconnect with the new per-device certificate.

## Topic table

| Direction | Purpose | Topic |
|---|---|---|
| Pub | Create keys & certificate         | `$aws/certificates/create/{format}` |
| Sub | CreateKeys reply                  | `$aws/certificates/create/{format}/accepted` / `.../rejected` |
| Pub | Create certificate from CSR       | `$aws/certificates/create-from-csr/{format}` |
| Sub | CreateFromCsr reply               | `$aws/certificates/create-from-csr/{format}/accepted` / `.../rejected` |
| Pub | Register thing                    | `$aws/provisioning-templates/{template}/provision/{format}` |
| Sub | RegisterThing reply               | `$aws/provisioning-templates/{template}/provision/{format}/accepted` / `.../rejected` |

`{format}` is `cbor` when the `provision_cbor` feature is enabled, `json`
otherwise. The CBOR variant uses smaller wire payloads at the cost of pulling
in `minicbor`.

## Example

```rust
use rustot::provisioning::{Credentials, CredentialHandler, Error, FleetProvisioner};

#[derive(Default)]
struct Storage { /* ... */ }
impl CredentialHandler for Storage {
    async fn store_credentials(&mut self, c: Credentials<'_>) -> Result<(), Error> {
        // Persist `c.certificate_pem`, `c.private_key`, `c.certificate_id`
        // somewhere durable (flash, secure element, etc.).
        Ok(())
    }
}

#[derive(serde::Deserialize)]
struct DeviceConfig {
    thing_name: heapless::String<128>,
    // ... whatever your provisioning template emits ...
}

async fn provision<M: rustot::mqtt::MqttClient>(mqtt: &M) {
    let mut storage = Storage::default();

    let parameters = serde_json::json!({
        "SerialNumber": "1234567890",
        "DeviceLocation": "factory-7",
    });

    let cfg: Option<DeviceConfig> = FleetProvisioner::provision(
        mqtt,
        "MyProvisioningTemplate",
        Some(&parameters),
        &mut storage,
    )
    .await
    .expect("provisioning");

    if let Some(cfg) = cfg {
        log::info!("Provisioned as {}", cfg.thing_name);
    }
}
```

For higher security, generate the private key on-device and submit a CSR via
[`provision_csr`] — the private key never leaves the device.

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `provision_cbor` | yes | Encode payloads as CBOR via `minicbor-serde`; topic suffix becomes `cbor`. Disable for JSON. |

When enabled, [`FleetProvisioner::provision_cbor`] is also available
explicitly; otherwise only the JSON entry points compile.

## Limitations

- **By-claim only.** Provisioning by trusted user is not implemented.
- **Single attempt.** No automatic retry / backoff; wrap calls if you need
  it.
- **No certificate rotation.** Once provisioned, the device uses the issued
  certificate; rotation is a separate AWS feature.

## Testing

Unit tests cover topic format/parse and request/response serialization:

```bash
cargo test --lib provisioning::
```

The integration test (`tests/provisioning.rs`) runs against a real AWS IoT
endpoint:

```bash
RUST_LOG=trace \
  TEMPLATE_NAME=provision_template_name \
  THING_NAME=MyTestThing \
  AWS_HOSTNAME=xxxxxxxx-ats.iot.eu-west-1.amazonaws.com \
  cargo test --test provisioning --features "provision_cbor,log"
```

`tests/secrets/claim_identity.pfx` must hold the claim certificate. If
password-protected, supply `IDENTITY_PASSWORD`.

`pfx` files can be built from a certificate + private key with:

```bash
openssl pkcs12 -export \
  -out claim_identity.pfx \
  -inkey private.pem.key -in certificate.pem.crt -certfile root-ca.pem
```

[`FleetProvisioner::provision`]: https://docs.rs/rustot/latest/rustot/provisioning/struct.FleetProvisioner.html#method.provision
[`provision_csr`]: https://docs.rs/rustot/latest/rustot/provisioning/struct.FleetProvisioner.html#method.provision_csr
[`provision_cbor`]: https://docs.rs/rustot/latest/rustot/provisioning/struct.FleetProvisioner.html#method.provision_cbor
[`FleetProvisioner::provision_cbor`]: https://docs.rs/rustot/latest/rustot/provisioning/struct.FleetProvisioner.html#method.provision_cbor
[`CredentialHandler`]: https://docs.rs/rustot/latest/rustot/provisioning/trait.CredentialHandler.html
