# AWS IoT Commands

Cloud-to-device, one-shot remote instructions. Unlike Jobs, command executions
have no queueing or retry semantics — the device receives a single request and
returns a single terminal response.

See the [AWS IoT Remote Commands documentation][aws-docs] for the full service
specification.

[aws-docs]: https://docs.aws.amazon.com/iot/latest/developerguide/iot-remote-command.html

## Workflow

1. **Subscribe** to the wildcard request topic
   `$aws/commands/things/{ThingName}/executions/+/request/{format}`.
2. On message: **parse** the `executionId` from the topic and deserialize the
   payload into your own `Deserialize` type.
3. (optional) Publish `IN_PROGRESS` updates to
   `.../executions/{executionId}/response/{format}` for long-running work.
4. Publish a **terminal status** (`SUCCEEDED` / `FAILED` / `REJECTED`) on the
   same response topic, optionally with `statusReason` and `result`.
5. (optional) Subscribe to
   `.../response/{accepted,rejected}/{format}` for cloud confirmation.

The cloud-side default execution timeout is 10 seconds, max 12 hours. Devices
may publish a terminal status to override a cloud-issued `TIMED_OUT`.

## Topic table

| Direction | Purpose | Topic |
|---|---|---|
| Sub  | Incoming commands | `$aws/commands/things/{Thing}/executions/+/request/{format}` |
| Pub  | Status / result   | `$aws/commands/things/{Thing}/executions/{id}/response/{format}` |
| Sub  | Cloud accepted    | `$aws/commands/things/{Thing}/executions/{id}/response/accepted/{format}` |
| Sub  | Cloud rejected    | `$aws/commands/things/{Thing}/executions/{id}/response/rejected/{format}` |

`{format}` is `cbor` when the `commands_cbor` feature is enabled, `json`
otherwise. The `executionId` is carried in the topic path; the payload need
not include it.

## Status values

| Value | Terminal? | Notes |
|---|---|---|
| `IN_PROGRESS` | no | Optional progress reports during long-running work. |
| `SUCCEEDED`   | yes | Command finished as requested. |
| `FAILED`      | yes | Command was understood but execution failed. |
| `REJECTED`    | yes | Command is invalid or unsupported on this device. |
| `TIMED_OUT`   | yes (when reported by device) | Device-reported timeout; can override a cloud-issued `TIMED_OUT`. |

`statusReason.reasonCode` must match `[A-Z0-9_-]+` and is at most 64 chars;
`statusReason.reasonDescription` is at most 1024 chars.

## Example

```rust
use rustot::commands::{CommandAgent, CommandResultEntry, ResultMap, parse_command_request};
use rustot::mqtt::{Mqtt, MqttClient, MqttSubscription};

// The on-the-wire payload shape is whatever your command template emits; AWS
// does not impose one. This example uses an internally-tagged enum keyed on
// the field name your template uses (`command` is just illustrative).
#[derive(serde::Deserialize)]
#[serde(tag = "command", content = "parameters")]
enum Command<'a> {
    Reboot,
    SetMode { mode: &'a str },
    GetBatteryLevel,
}

async fn run<C: MqttClient>(mqtt: Mqtt<&C>) {
    let agent = CommandAgent::new(&mqtt);
    let mut sub = agent.subscribe().await.unwrap();

    while let Some(mut msg) = sub.next_message().await {
        let Some(req) = parse_command_request::<Command>(&mut msg) else { continue };
        let id = req.execution_id.as_str();

        match req.payload {
            Command::Reboot => {
                // Long-running: tell the cloud we're working.
                agent.report_in_progress(id, None).await.ok();
                // ... do the reboot prep ...
                agent.succeed::<()>(id, None).await.ok();
            }

            Command::SetMode { mode } if mode == "comfort" || mode == "eco" => {
                agent.succeed::<()>(id, None).await.ok();
            }
            Command::SetMode { mode } => {
                agent.reject(id, "UNSUPPORTED_MODE", Some(mode)).await.ok();
            }

            Command::GetBatteryLevel => {
                let mut result = ResultMap::new();
                result.insert("battery", CommandResultEntry::String { s: "85%" }).unwrap();
                agent.succeed(id, Some(&result)).await.ok();
            }
        }
    }
}
```

### What the helpers publish

- `report_in_progress(id, reason)` — QoS 0, fire-and-forget. Use during
  long-running work to extend the execution past the cloud-side timeout.
- `succeed(id, result)` / `fail(id, code, desc)` / `reject(id, code, desc)`
  — QoS 1, publish then await `accepted` / `rejected` from the cloud with a
  5-second timeout. Returns `CommandError::Rejected(_)` on rejection,
  `CommandError::Timeout` if no ack arrives.

## `result` field

A command result is a map keyed by name; each entry carries one of:

- `s`   — string
- `b`   — boolean
- `bin` — binary (base64 string in JSON; raw bytes in CBOR per AWS)

Use [`ResultMap`] for the common case:

```rust
use rustot::commands::{CommandResultEntry, ResultMap};

let mut result = ResultMap::new();
result.insert("status",  CommandResultEntry::String { s: "ok"   }).unwrap();
result.insert("ok",      CommandResultEntry::Bool   { b: true   }).unwrap();
result.insert("payload", CommandResultEntry::Bin    { bin: "AAA=" }).unwrap();
agent.succeed(execution_id, Some(&result)).await?;
```

For a fixed-shape result, define your own `Serialize` struct and pass `&value`
to `agent.succeed(...)`.

[`ResultMap`]: https://docs.rs/rustot/latest/rustot/commands/type.ResultMap.html

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `commands_cbor` | yes | Encode payloads as CBOR via `minicbor-serde`; topic suffix becomes `cbor`. Disable for JSON. |

Only one format is active per build, mirroring `provision_cbor` /
`metric_cbor`.

## Limitations

- **Things only.** The unregistered `$aws/commands/clients/{ClientId}/...` form
  is not supported.
- **`CommandResultEntry::Bin` is `&str` in both formats.** AWS encodes `bin`
  as a base64 string in JSON but as a raw CBOR byte string. JSON callers must
  base64-encode binary data themselves; CBOR callers who need a true byte
  string should construct a custom `Serialize` type instead of using
  `ResultMap`.
- **No MQTT5 user properties.** Only the `json` and `cbor` AWS-defined topic
  suffixes are handled; non-JSON / non-CBOR custom payloads with MQTT5
  `content-type` user properties are out of scope.

## Testing

Unit tests cover topic format/parse, payload-shape regression guards
(`Update::encode` JSON output for representative status combinations), the
CBOR encode→decode round-trip, and `parse_command_request` happy path
plus topic-mismatch:

```bash
cargo test --lib commands::
cargo test --lib --no-default-features --features mqtt_mqttrust commands::
```

An end-to-end integration test (`tests/commands.rs`) creates a fresh Command
resource in AWS IoT, triggers `StartCommandExecution` against the test thing,
and asserts the cloud-side execution status reaches `SUCCEEDED`:

```bash
RUST_LOG=trace \
  THING_NAME=MyTestThing \
  AWS_HOSTNAME=xxxxxxxx-ats.iot.eu-west-1.amazonaws.com \
  cargo test --test commands --features "commands_cbor,log"
```

`tests/secrets/identity.pfx` must hold a valid AWS IoT identity for the
thing; `IDENTITY_PASSWORD` may be set if the file is password-protected. AWS
credentials must be available in the environment with permission to assume
the cross-account role used by the test harness.

To trigger an execution by hand from outside the test harness:

```bash
aws iot-jobs-data start-command-execution \
  --target-arn   arn:aws:iot:<region>:<account>:thing/<ThingName> \
  --command-arn  arn:aws:iot:<region>:<account>:command/<CommandName>
```

and observe the device subscribe → request → response → `accepted` flow.
