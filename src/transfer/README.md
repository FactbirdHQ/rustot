# File Transfer & OTA

A file-transfer engine that downloads files described by AWS IoT Job
documents, with bitmap-tracked block retries and progress reporting. Built on
top of [`crate::jobs`](../jobs).

Two entry points:

- [`Transfer::perform`] — generic file transfer; needs only a
  [`TransferPal`].
- [`Transfer::perform_ota`] — OTA update; needs an [`OtaPal`]
  (which extends [`TransferPal`]) and adds self-test, image-state, and
  reset/activation hooks.

A job may contain several files. They are downloaded in document order and
the job is all-or-nothing: activation happens once after every file has been
downloaded and closed, and any failure aborts the whole job.

The OTA path is wire-compatible with the Amazon FreeRTOS [`afr_ota`][afr]
job-document format produced by AWS's [`CreateOTAUpdate`][create-ota]
control-plane API.

[afr]: https://docs.aws.amazon.com/freertos/latest/userguide/ota-mqtt-protocol.html
[create-ota]: https://docs.aws.amazon.com/iot/latest/apireference/API_CreateOTAUpdate.html

## Workflow

1. Receive a job document via [`crate::jobs`] and deserialize it (an
   [`OtaJob`] for the FreeRTOS format, or your own [`JobDocument`]
   implementation).
2. Build a [`JobContext`] from the parsed execution. Its `N` parameter
   (default `1`) caps the number of files per job.
3. Pick a data interface (MQTT streams or HTTP pre-signed URL) — the engine
   negotiates against the `protocols` field in the job document.
4. Call [`Transfer::perform`] (generic) or [`Transfer::perform_ota`]
   (with activation). For each file in the job, the engine:
   - drives block requests over the chosen data interface,
   - tracks completed blocks via a [`Bitmap`] for retry within a session,
   - reports progress (`IN_PROGRESS` with `statusDetails`, summed over all
     files) back to Jobs,
   - calls into your `Pal` to create, write and close the file.

   Once every file is closed it publishes a single terminal status. If no
   file requires activation the job is `SUCCEEDED`; otherwise it stays
   `IN_PROGRESS` (self-test `ready`). On any failure it calls `abort` once
   and marks the job `FAILED`.
5. (OTA only) `perform_ota` fires `OtaEvent::Activate`. If the callback
   returns an error, the engine rejects the update, marks the job `FAILED`
   and fires `OtaEvent::Fail`. After activation (typically a reset), the
   device re-enters self-test; your `OtaPal` accepts or rejects the update
   and reports the outcome. If the PAL can't enter the `Testing` state, or
   the `StartTest` callback returns an error or exceeds
   `Config::self_test_timeout`, the engine rejects the update, marks the job
   `FAILED`, fires `OtaEvent::SelfTestFailed` and calls `reset_device` to
   roll back. With plain `perform`, finishing an activated job is up to the
   caller (e.g. `JobAgent::succeed_job`).

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
| Amazon FreeRTOS `afr_ota` | [`OtaJob`] | Default for OTA. Compatible with `CreateOTAUpdate`. `OtaJob<'a, N>` accepts up to `N` files (default `1`). |
| Custom | impl [`JobDocument`] | For non-OTA file transfers or custom OTA shapes. |

The wire-format fields (`fileid`, `filesize`, `streamname`, `update_data_url`,
`sig-sha256-ecdsa`, …) follow the `afr_ota` schema 1:1.

## PAL responsibilities

The platform abstraction layer (your code) handles flash / filesystem writes
and image lifecycle. Two traits, one extending the other:

- [`TransferPal`] — `create_file_for_rx` / `close_file` (per file,
  signature verification belongs in `close_file`), `abort` (per job: discard
  everything staged for it), `requires_activation` (per file, defaults to
  `fileType == 0`) and `status_details`. All that's needed for generic file
  transfer.
- [`OtaPal`] — adds `set_platform_image_state`, `get_platform_image_state`,
  `activate_new_image`, `reset_device` and `complete_callback`. The engine
  drives the [`ImageState`] state machine
  (`Unknown` → `Testing` → `Accepted` / `Rejected` / `Aborted`) once per job.

Override `requires_activation` to put non-firmware files (e.g. ML models
deployed with a custom `fileType`) through activation, self-test and
rollback.

## Status details

Every job update carries a flat `statusDetails` map (AWS IoT Jobs only
accepts string values):

| Key | Content |
|---|---|
| `self_test` | `receiving`, `ready`, `active`, `accepted`, `rejected`, `aborted` |
| `progress` | `"received/total"` blocks, summed over all files in the job |
| `reason`, `error_code` | Failure reason and code, on `rejected` / `aborted` |
| `files` | Multi-file jobs only (never present for single-file jobs): a JSON array, encoded as a string, with one entry per file |

A `files` entry looks like
`{"id":1,"status":"failed","progress":"3/3","reason":"sig_check_failed","error_code":1001}`,
where `id` is the job document's `fileid` and `status` is `pending`,
`in_progress`, `succeeded` or `failed`. `reason` and `error_code` only
appear on the failed file. AWS limits each value to 1024 characters, which
caps `N` at 10 files on 64-bit targets and 14 on 32-bit targets; a larger
`N` fails to compile.

Use [`JobAgent::parse_or_reject`] to receive jobs: a document that doesn't
deserialize is rejected instead of staying queued and blocking every later
job. The same applies to errors from [`JobContext::new`] (e.g. more files
than `N`) — reject the job with `JobAgent::reject_job`.

OTA reasons surface through [`ImageStateReason`] and [`PalError`], with
numeric error codes:

| Range | Meaning |
|---|---|
| `1xxx` | PAL-level errors (signature mismatch, write failure, …) |
| `2xxx` | Image-state reasons (newer job, version check, user abort, …) |

## Example (OTA, MQTT)

```rust
use rustot::jobs::stream::JobAgent;
use rustot::mqtt::{Mqtt, OwnedMessage};
use rustot::transfer::{Transfer, config::Config, encoding::afr_ota::OtaJob, error::TransferError};

#[derive(serde::Deserialize)]
enum Jobs<'a> {
    #[serde(rename = "afr_ota", borrow)]
    Ota(OtaJob<'a>),
}

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

    // Rejects the job in the cloud if its document doesn't parse into `Jobs`,
    // so it can't stay queued and block later jobs.
    let execution = agent
        .parse_or_reject::<Jobs>(&owned)
        .await
        .map_err(|_| TransferError::Mqtt)?
        .ok_or(TransferError::NoActiveJob)?;

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
- **No resume across reboots.** Download progress is not restored from the
  job's `statusDetails`; a job interrupted by a reboot restarts from the
  first block of the first file.
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
[`JobAgent::parse_or_reject`]: https://docs.rs/rustot/latest/rustot/jobs/stream/struct.JobAgent.html#method.parse_or_reject
[`JobContext::new`]: https://docs.rs/rustot/latest/rustot/transfer/encoding/struct.JobContext.html#method.new
