# AWS IoT Jobs

Receive remote operations from the cloud, drive a job-execution state machine,
and report progress and final status. Unlike Commands, jobs are queued, can be
described and re-described, and survive disconnects via the cloud-side queue.

See the [AWS IoT Jobs documentation][aws-docs] for the full service
specification.

[aws-docs]: https://docs.aws.amazon.com/iot/latest/developerguide/iot-jobs.html

## Workflow

1. **Subscribe** to `$aws/things/{ThingName}/jobs/notify-next` (and the
   `$next/get/accepted` reply topic).
2. **Describe** the next pending job by publishing on
   `$aws/things/{ThingName}/jobs/$next/get` — the response carries the
   `jobDocument`. [`JobAgent::subscribe`] does both in one call.
3. **Deserialize** the job document into your own `Deserialize` type
   (typically a tagged enum of supported job kinds) using
   [`parse_job_message`].
4. **Update** status via `$aws/things/{ThingName}/jobs/{jobId}/update`:
   `IN_PROGRESS` (with optional `statusDetails` for progress), then a terminal
   `SUCCEEDED` / `FAILED` / `REJECTED`. [`JobAgent::report_progress`],
   [`succeed_job`], [`fail_job`], and [`reject_job`] handle this.
5. The cloud publishes a fresh `notify-next` whenever the queue head changes,
   so the loop re-runs with the next pending job automatically.

## Topic table

| Direction | Purpose | Topic |
|---|---|---|
| Sub  | Next-job notifications        | `$aws/things/{Thing}/jobs/notify-next` |
| Pub  | Describe job by id            | `$aws/things/{Thing}/jobs/{jobId}/get` (or `$next/get`) |
| Sub  | Describe response             | `$aws/things/{Thing}/jobs/{jobId}/get/accepted` / `.../rejected` |
| Pub  | Get pending list              | `$aws/things/{Thing}/jobs/get` |
| Pub  | Start next pending            | `$aws/things/{Thing}/jobs/start-next` |
| Pub  | Update execution              | `$aws/things/{Thing}/jobs/{jobId}/update` |
| Sub  | Update response               | `$aws/things/{Thing}/jobs/{jobId}/update/accepted` / `.../rejected` |

The full [`JobTopic`] enum covers every variant.

## Status values

| Value | Cloud-set | Device-set | Terminal |
|---|---|---|---|
| `QUEUED`      | yes | — | no |
| `IN_PROGRESS` | —   | yes | no |
| `SUCCEEDED`   | —   | yes | yes |
| `FAILED`      | —   | yes | yes |
| `REJECTED`    | —   | yes | yes |
| `CANCELED`    | yes | — | yes |
| `REMOVED`     | yes | — | yes |

`statusDetails` is a free-form `LinearMap<&str, &str, 8>` ([`StatusDetails`]);
both the device and the cloud may carry arbitrary key/value pairs across
updates.

## Example

```rust
use rustot::jobs::stream::{JobAgent, parse_job_message};
use rustot::mqtt::{Mqtt, MqttClient, MqttSubscription};

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum MyJob<'a> {
    Reboot { delay_secs: u32 },
    Reset,
    Custom { kind: &'a str },
}

async fn run<C: MqttClient>(mqtt: Mqtt<&C>) {
    let agent = JobAgent::new(&mqtt);

    loop {
        let mut sub = agent.subscribe().await.unwrap();

        while let Some(mut msg) = sub.next_message().await {
            let Some(execution) = parse_job_message::<MyJob>(&mut msg) else { continue };
            let job_id = execution.job_id;

            match execution.job_document {
                Some(MyJob::Reboot { delay_secs }) => {
                    agent.report_progress(job_id, &("scheduled", delay_secs)).await.ok();
                    // ... schedule reboot ...
                    agent.succeed_job::<()>(job_id, None).await.ok();
                }
                Some(MyJob::Reset) => {
                    agent.succeed_job::<()>(job_id, None).await.ok();
                }
                Some(MyJob::Custom { kind }) => {
                    agent.reject_job(job_id, kind).await.ok();
                }
                None => {}
            }
        }

        // Subscription closed (clean session / disconnect): re-subscribe.
    }
}
```

### What the helpers publish

- `report_progress(job_id, &details)` — QoS 0, fire-and-forget. Sets
  `IN_PROGRESS` and serializes `details` into `statusDetails`.
- `succeed_job(job_id, details)` / `fail_job(...)` / `reject_job(...)` —
  QoS 1, publish then await `update/accepted` (or `update/rejected`) from
  the cloud with a 5-second timeout.

## Cargo features

The Jobs module has no dedicated feature flags — it is compiled in
unconditionally as part of the default build, since [`transfer`] and
[`commands`] both build on top of it.

[`transfer`]: ../transfer
[`commands`]: ../commands

## Limitations

- **JSON only.** Jobs payloads are JSON; CBOR is not part of the AWS Jobs
  topic schema.
- **One pending / running job tracked at a time** in
  [`MAX_PENDING_JOBS`] / [`MAX_RUNNING_JOBS`]. Bump the consts if your
  application needs to track more.

## Testing

Unit tests cover topic format/parse, request-payload serialization, and
response deserialization:

```bash
cargo test --lib jobs::
```

The Jobs module is exercised end-to-end by the OTA integration test
(`tests/ota_mqtt.rs`), which uses [`JobAgent`] + [`parse_job_message`] to
drive a real `CreateOTAUpdate`-issued job through to completion.

[`JobAgent`]: https://docs.rs/rustot/latest/rustot/jobs/stream/struct.JobAgent.html
[`JobAgent::subscribe`]: https://docs.rs/rustot/latest/rustot/jobs/stream/struct.JobAgent.html#method.subscribe
[`JobAgent::report_progress`]: https://docs.rs/rustot/latest/rustot/jobs/stream/struct.JobAgent.html#method.report_progress
[`succeed_job`]: https://docs.rs/rustot/latest/rustot/jobs/stream/struct.JobAgent.html#method.succeed_job
[`fail_job`]: https://docs.rs/rustot/latest/rustot/jobs/stream/struct.JobAgent.html#method.fail_job
[`reject_job`]: https://docs.rs/rustot/latest/rustot/jobs/stream/struct.JobAgent.html#method.reject_job
[`parse_job_message`]: https://docs.rs/rustot/latest/rustot/jobs/stream/fn.parse_job_message.html
[`JobTopic`]: https://docs.rs/rustot/latest/rustot/jobs/enum.JobTopic.html
[`StatusDetails`]: https://docs.rs/rustot/latest/rustot/jobs/type.StatusDetails.html
[`MAX_PENDING_JOBS`]: https://docs.rs/rustot/latest/rustot/jobs/constant.MAX_PENDING_JOBS.html
[`MAX_RUNNING_JOBS`]: https://docs.rs/rustot/latest/rustot/jobs/constant.MAX_RUNNING_JOBS.html
