# AWS IoT Device Shadows

Synchronise a device's `reported` and `desired` state with the cloud's
authoritative shadow document. This module wraps the AWS IoT Device Shadow
service and adds optional KV-backed persistence so a device can resume from
its last known state across reboots.

See the [AWS IoT Device Shadow documentation][aws-docs] for the service
specification (named shadows, classic vs named, deltas).

[aws-docs]: https://docs.aws.amazon.com/iot/latest/developerguide/iot-device-shadows.html

## Workflow

1. **Define your shadow type** with the `#[shadow_root]` and `#[shadow_node]`
   attribute macros from `rustot_derive`. The macros generate a `Reported`,
   `Desired`, and `Delta` representation, plus FNV1A hashes for
   change-detection.
2. **Pick a [`StateStore`]** — [`InMemory`] for ephemeral, [`FileKVStore`]
   (std-only), [`SequentialKVStore`] (flash via `sequential-storage`), or
   your own.
3. **Construct a [`Shadow`]** with `Shadow::new(&store, &mqtt)`.
4. **Drive the lifecycle**:
   - [`load`] — repopulate from the KV store on boot.
   - [`create_shadow`] — initialize the cloud shadow on first run.
   - [`sync_shadow`] — pull the cloud's current state.
   - [`wait_delta`] — block until the cloud sends a delta; apply it.
   - [`update_reported`] — push device-side changes to the cloud.
   - [`commit`] — flush dirty fields back to the KV store.

## Topic table

| Direction | Purpose | Topic |
|---|---|---|
| Pub | Get shadow                | `$aws/things/{Thing}/shadow/get` |
| Sub | Get reply                 | `$aws/things/{Thing}/shadow/get/accepted` / `.../rejected` |
| Pub | Update shadow             | `$aws/things/{Thing}/shadow/update` |
| Sub | Update reply              | `$aws/things/{Thing}/shadow/update/accepted` / `.../rejected` |
| Sub | Delta from cloud          | `$aws/things/{Thing}/shadow/update/delta` |
| Sub | Documents (full)          | `$aws/things/{Thing}/shadow/update/documents` |
| Pub | Delete shadow             | `$aws/things/{Thing}/shadow/delete` |
| Sub | Delete reply              | `$aws/things/{Thing}/shadow/delete/accepted` / `.../rejected` |

Named shadows replace `/shadow/` with `/shadow/name/{shadowName}/`.

## Example

```rust
use rustot::shadows::{InMemory, Shadow};
use rustot_derive::{shadow_node, shadow_root};

#[shadow_root]
#[derive(Default)]
struct DeviceState {
    network: Network,
    sensors: Sensors,
}

#[shadow_node]
#[derive(Default)]
struct Network {
    ssid: heapless::String<32>,
    rssi_dbm: i16,
}

#[shadow_node]
#[derive(Default)]
struct Sensors {
    temperature_c: f32,
    occupied: bool,
}

async fn run<C: rustot::mqtt::MqttClient>(mqtt: &C) {
    let store = InMemory::<DeviceState>::default();
    let shadow = Shadow::new(&store, mqtt);

    // First boot: initialize cloud-side shadow with the local default.
    shadow.create_shadow().await.ok();

    // Pull cloud state so we converge before publishing anything.
    let _ = shadow.sync_shadow().await;

    loop {
        // Block until the cloud requests a change.
        let (state, delta) = shadow.wait_delta().await.unwrap();
        if let Some(delta) = delta {
            // Apply the delta to local hardware here.
            log::info!("delta requested: {:?}", delta);
            shadow.update_reported(state).await.ok();
        }
    }
}
```

For periodic publishing of sensor changes alongside delta handling, drive
`wait_delta` and `update_reported` from independent tasks against the same
`Shadow`.

## KV persistence

With the `shadows_kv_persist` feature, fields can be persisted individually
(field-level dirty tracking) to any [`KVStore`]. Backing implementations:

| Store | Feature | Notes |
|---|---|---|
| [`InMemory`] | always available | RAM only; no persistence. |
| [`FileKVStore`] | `std` + `shadows_kv_persist` | One file per field; useful in tests / std targets. |
| [`SequentialKVStore`] | `sequential_storage` | Flash-backed via `sequential-storage`. |
| Bring your own | `shadows_kv_persist` | Implement [`KVStore`] for whatever you have. |

[`load`] returns a [`LoadResult`] indicating how many fields were restored,
defaulted, or migrated. [`commit`] returns [`CommitStats`] for telemetry.

## Multi-shadow manager

With the `shadows_multi` feature (std-only), [`crate::shadows::multi`]
provides a manager for runtime-named shadows with wildcard subscriptions —
useful when the device tracks an unbounded set of shadow instances (e.g. one
per attached peripheral).

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `shadows_kv_persist`  | yes | Field-level KV persistence traits and `FileKVStore` (std). |
| `sequential_storage`  | no  | Flash-backed `SequentialKVStore`. Implies `shadows_kv_persist`. |
| `shadows_builders`    | no  | Generate `bon` builder methods on shadow structs (downstream must add `bon`). |
| `shadows_multi`       | no  | Runtime-named shadows with wildcard subscriptions (`std`-only; AWS SDK). |

## Limitations

- **Persistent sessions recommended.** Without them, deltas published while
  the device is offline are lost.
- **One unnamed (classic) shadow per `Shadow` instance.** Use named shadows
  or the `shadows_multi` manager for multiple.
- **Field migrations are explicit.** Renaming a field requires a migration
  hook ([`MigrationSource`]); the macros do not silently rename.

## Testing

Unit tests cover topic format/parse, codegen output for the derive macros,
and KV round-trips:

```bash
cargo test --lib shadows::
```

The integration test (`tests/shadows.rs`) runs through delete / update / get
sequences against a real AWS IoT endpoint:

```bash
RUST_LOG=trace \
  THING_NAME=MyTestThing \
  AWS_HOSTNAME=xxxxxxxx-ats.iot.eu-west-1.amazonaws.com \
  cargo test --test shadows --features "log"
```

`tests/secrets/identity.pfx` must hold a valid AWS IoT identity for the
thing; `IDENTITY_PASSWORD` may be set if the file is password-protected.

`pfx` files can be built from a certificate + private key with:

```bash
openssl pkcs12 -export \
  -out identity.pfx \
  -inkey private.pem.key -in certificate.pem.crt -certfile root-ca.pem
```

This test runs as a CI integration test in Factbird's AWS account on every
PR.

[`Shadow`]: https://docs.rs/rustot/latest/rustot/shadows/struct.Shadow.html
[`StateStore`]: https://docs.rs/rustot/latest/rustot/shadows/trait.StateStore.html
[`InMemory`]: https://docs.rs/rustot/latest/rustot/shadows/struct.InMemory.html
[`FileKVStore`]: https://docs.rs/rustot/latest/rustot/shadows/struct.FileKVStore.html
[`SequentialKVStore`]: https://docs.rs/rustot/latest/rustot/shadows/struct.SequentialKVStore.html
[`KVStore`]: https://docs.rs/rustot/latest/rustot/shadows/trait.KVStore.html
[`LoadResult`]: https://docs.rs/rustot/latest/rustot/shadows/enum.LoadResult.html
[`CommitStats`]: https://docs.rs/rustot/latest/rustot/shadows/struct.CommitStats.html
[`MigrationSource`]: https://docs.rs/rustot/latest/rustot/shadows/trait.MigrationSource.html
[`load`]: https://docs.rs/rustot/latest/rustot/shadows/struct.Shadow.html#method.load
[`commit`]: https://docs.rs/rustot/latest/rustot/shadows/struct.Shadow.html#method.commit
[`create_shadow`]: https://docs.rs/rustot/latest/rustot/shadows/struct.Shadow.html#method.create_shadow
[`sync_shadow`]: https://docs.rs/rustot/latest/rustot/shadows/struct.Shadow.html#method.sync_shadow
[`wait_delta`]: https://docs.rs/rustot/latest/rustot/shadows/struct.Shadow.html#method.wait_delta
[`update_reported`]: https://docs.rs/rustot/latest/rustot/shadows/struct.Shadow.html#method.update_reported
[`crate::shadows::multi`]: https://docs.rs/rustot/latest/rustot/shadows/multi/index.html
