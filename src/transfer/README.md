# File Transfer & OTA

A file-transfer engine that downloads files described by AWS IoT Job
documents, with bitmap-tracked block retries and progress reporting. Built on
top of [`crate::jobs`](../jobs).

Two entry points:

- [`Transfer::perform`] — generic file transfer; needs only a
  [`TransferPal`].
- [`Transfer::perform_ota`] — OTA firmware update; needs an [`OtaPal`]
  (which extends [`TransferPal`]) and adds self-test, image-state, and
  reset/activation hooks.

The OTA path is wire-compatible with the Amazon FreeRTOS [`afr_ota`][afr]
job-document format produced by AWS's [`CreateOTAUpdate`][create-ota]
control-plane API.

[afr]: https://docs.aws.amazon.com/freertos/latest/userguide/ota-mqtt-protocol.html
[create-ota]: https://docs.aws.amazon.com/iot/latest/apireference/API_CreateOTAUpdate.html

## Workflow

1. Receive a job document via [`crate::jobs`] and deserialize it (an
   [`OtaJob`] for the FreeRTOS format, or your own [`JobDocument`]
   implementation).
2. Build a [`JobContext`] from the parsed execution.
3. Pick a data interface (MQTT streams or HTTP pre-signed URL) — the engine
   negotiates against the `protocols` field in the job document.
4. Call [`Transfer::perform`] (generic) or [`Transfer::perform_ota`]
   (firmware). The engine:
   - drives block requests over the chosen data interface,
   - tracks completed blocks via a [`Bitmap`] for resumption / retry,
   - reports progress (`IN_PROGRESS` with `statusDetails`) back to Jobs,
   - calls into your `Pal` to write blocks, finalize, abort, or activate,
   - publishes a terminal Jobs status on completion or failure.
5. (OTA only) After reset, the device re-enters self-test; your `OtaPal`
   accepts or rejects the new image and reports the outcome.

## Data interfaces

| Interface | Cargo feature | Notes |
|---|---|---|
| MQTT (IoT Streams) | `transfer_mqtt` | Default. Requests blocks over `$aws/things/{thing}/streams/{stream}/get/cbor` and receives them on `.../data/cbor`. CBOR-only. |
| HTTP (S3 presigned URL)  | `transfer_http`         | No HTTP client included. Plug your own. |
| HTTP via `reqwest` | `transfer_http_reqwest` | `std`-only convenience implementation built on `reqwest`. |

Both interfaces can coexist in one build — the engine picks whichever the job
document advertises.

## Job document formats

| Format | Type | Notes |
|---|---|---|
| Amazon FreeRTOS `afr_ota` | [`OtaJob`] | Default for OTA. Compatible with `CreateOTAUpdate`. |
| Custom | impl [`JobDocument`] | For non-OTA file transfers or custom OTA shapes. |

The wire-format fields (`fileid`, `filesize`, `streamname`, `update_data_url`,
`sig-sha256-ecdsa`, …) follow the `afr_ota` schema 1:1.

## PAL responsibilities

The platform abstraction layer (your code) handles flash / filesystem writes
and image lifecycle. Two traits, one extending the other:

- [`TransferPal`] — `create_file`, `write_block`, `close_file`, `abort`,
  `status_details`. All that's needed for generic file transfer.
- [`OtaPal`] — adds `set_platform_image_state`, `get_platform_image_state`,
  `activate_new_image`, `reset_device`, plus signature verification hooks. The
  engine drives the [`ImageState`] state machine
  (`Unknown` → `Testing` → `Accepted` / `Rejected` / `Aborted`).

OTA reasons surface through [`ImageStateReason`] and [`PalError`], with
numeric error codes:

| Range | Meaning |
|---|---|
| `1xxx` | PAL-level errors (signature mismatch, write failure, …) |
| `2xxx` | Image-state reasons (newer job, version check, user abort, …) |

## Example (OTA, MQTT)

```rust
use rustot::jobs::stream::{JobAgent, parse_job_message};
use rustot::mqtt::{Mqtt, OwnedMessage};
use rustot::transfer::{Transfer, config::Config, encoding::afr_ota::OtaJob, error::TransferError};

async fn run_ota<C: rustot::mqtt::MqttClient, P: rustot::transfer::pal::OtaPal>(
    mqtt: Mqtt<&C>,
    pal: &mut P,
) -> Result<(), TransferError> {
    let agent = JobAgent::new(&mqtt);
    let mut sub = agent.subscribe().await.map_err(|_| TransferError::Mqtt)?;

    let message = sub.next_message().await.ok_or(TransferError::Mqtt)?;
    let mut owned = OwnedMessage::<256, 4096>::from_ref(&message).unwrap();
    drop(message);
    sub.unsubscribe().await.ok();

    let execution = parse_job_message::<OtaJob>(&mut owned)
        .ok_or(TransferError::InvalidJobDocument)?;

    let ctx = build_job_context(execution)?; // user code, see tests/common
    let cfg = Config { block_size: 4096, ..Default::default() };

    Transfer::perform_ota(&mqtt, &mqtt, &ctx, pal, &cfg).await
}
```

A complete working PAL is in
[`tests/common/file_handler.rs`](../../tests/common/file_handler.rs).

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `transfer_mqtt`         | yes | Build the MQTT data interface (uses `minicbor`). |
| `transfer_http`         | no  | Build the HTTP data interface (no client baked in). |
| `transfer_http_reqwest` | no  | `std`-only convenience: HTTP via `reqwest` + `rustls`. |

## Limitations

- **CBOR only on the MQTT data interface.** AWS IoT Streams require CBOR.
- **One file per job.** The `afr_ota` schema allows multiple files but the
  engine tracks one active transfer at a time.
- **Self-test policy is delegated to the PAL.** The engine drives the state
  transitions; your code decides whether the new image passes self-test.

## Testing

Unit tests cover bitmap accounting, block validation, and topic / encoding
round-trips:

```bash
cargo test --lib transfer::
```

End-to-end OTA against real AWS IoT (`tests/ota_mqtt.rs`) requires an
`identity.pfx` under `tests/secrets/` and AWS credentials. The test issues a
real `CreateOTAUpdate`, drives the device through the full job lifecycle,
and asserts cloud-side `SUCCEEDED`:

```bash
RUST_LOG=trace \
  THING_NAME=MyTestThing \
  AWS_HOSTNAME=xxxxxxxx-ats.iot.eu-west-1.amazonaws.com \
  cargo test --test ota_mqtt --features "transfer_mqtt,log"
```

[`Transfer::perform`]: https://docs.rs/rustot/latest/rustot/transfer/struct.Transfer.html#method.perform
[`Transfer::perform_ota`]: https://docs.rs/rustot/latest/rustot/transfer/struct.Transfer.html#method.perform_ota
[`TransferPal`]: https://docs.rs/rustot/latest/rustot/transfer/pal/trait.TransferPal.html
[`OtaPal`]: https://docs.rs/rustot/latest/rustot/transfer/pal/trait.OtaPal.html
[`OtaJob`]: https://docs.rs/rustot/latest/rustot/transfer/encoding/afr_ota/struct.OtaJob.html
[`JobDocument`]: https://docs.rs/rustot/latest/rustot/transfer/encoding/trait.JobDocument.html
[`JobContext`]: https://docs.rs/rustot/latest/rustot/transfer/encoding/struct.JobContext.html
[`Bitmap`]: https://docs.rs/rustot/latest/rustot/transfer/encoding/struct.Bitmap.html
[`ImageState`]: https://docs.rs/rustot/latest/rustot/transfer/pal/enum.ImageState.html
[`ImageStateReason`]: https://docs.rs/rustot/latest/rustot/transfer/pal/enum.ImageStateReason.html
[`PalError`]: https://docs.rs/rustot/latest/rustot/transfer/pal/enum.PalError.html
[`crate::jobs`]: ../jobs
