#![cfg(feature = "commands_cbor")]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

//! ## Integration test of `AWS IoT Commands`
//!
//! Drives the full device-side flow against a real AWS IoT endpoint:
//!
//! 1. Setup creates a fresh `Command` resource (under the `AWS-IoT` namespace)
//!    in the target account.
//! 2. The device subscribes to the wildcard request topic.
//! 3. The harness triggers `StartCommandExecution` against the test thing.
//! 4. The device parses the command, publishes `IN_PROGRESS`, then `SUCCEEDED`
//!    with a small `result` map, and awaits the cloud's `accepted` ack.
//! 5. The harness polls `GetCommandExecution` until terminal and asserts
//!    `SUCCEEDED`. Cleanup deletes the Command resource.

mod common;

use aws_sdk_iot::types::CommandExecutionStatus;
use common::aws_commands;
use common::credentials;
use common::network::TlsNetwork;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use mqttrust::{Config, DomainBroker, State, transport::embedded_nal::NalTransport};
use rustot::commands::{
    CommandAgent, CommandResultEntry, ResultMap, parse_command_request,
};
use rustot::mqtt::Mqtt;
use serde::Deserialize;
use serial_test::serial;
use static_cell::StaticCell;

/// Payload schema delivered to the device for this test.
#[derive(Debug, Deserialize, PartialEq)]
struct TestCommand<'a> {
    action: &'a str,
    value: i32,
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn test_commands() {
    let _ = env_logger::Builder::from_default_env()
        .filter_module("serial_test", log::LevelFilter::Warn)
        .try_init();

    log::info!("Starting Commands integration test...");

    // Build the CBOR payload the device will receive (same minicbor-serde
    // path the device uses to encode its responses).
    let payload = {
        let mut buf = [0u8; 256];
        let mut ser = minicbor_serde::Serializer::new(minicbor::encode::write::Cursor::new(
            &mut buf[..],
        ));
        serde::Serialize::serialize(
            &serde_json::json!({ "action": "echo", "value": 42 }),
            &mut ser,
        )
        .expect("encode CBOR test payload");
        let len = ser.into_encoder().writer().position();
        buf[..len].to_vec()
    };

    let ctx = match aws_commands::setup(payload, "application/cbor").await {
        Some(ctx) => ctx,
        None => {
            log::info!("Skipping Commands test: no valid AWS credentials or setup failed");
            return;
        }
    };

    let test_result = aws_commands::catch_unwind_future(run_happy_path(&ctx)).await;

    ctx.cleanup().await;

    match test_result {
        Ok(inner) => inner.expect("Device-side flow failed"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn run_happy_path(ctx: &aws_commands::CommandTestContext) -> Result<(), String> {
    let (thing_name, identity) = credentials::identity();
    let hostname = credentials::HOSTNAME.unwrap();

    static NETWORK: StaticCell<TlsNetwork> = StaticCell::new();
    let network = NETWORK.init(TlsNetwork::new(hostname.to_owned(), identity));

    let broker =
        DomainBroker::<_, 128>::new_with_port(hostname, 8883, network).map_err(|_| "broker")?;
    let config = Config::builder()
        .client_id(thing_name.try_into().map_err(|_| "client_id")?)
        .keepalive_interval(embassy_time::Duration::from_secs(50))
        .build();

    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 4 }>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = mqttrust::new(state, config);

    // (executionId, terminal_status) shared between device and harness tasks.
    let exec_id_cell: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::new();

    let device_fut = async {
        let mqtt = Mqtt(&client);
        let agent = CommandAgent::new(&mqtt);

        let mut sub = agent.subscribe().await.map_err(|e| format!("subscribe: {e:?}"))?;
        log::info!("Device subscribed; triggering execution");

        // Now that we're listening, ask the cloud to send the command.
        let exec_id = ctx.start_execution().await?;
        exec_id_cell.set(exec_id.clone()).ok();

        let mut msg = embassy_time::with_timeout(
            embassy_time::Duration::from_secs(30),
            sub.next_message(),
        )
        .await
        .map_err(|_| "timed out waiting for command request")?
        .ok_or("subscription closed before command arrived")?;

        let req = parse_command_request::<TestCommand>(&mut msg)
            .ok_or("failed to parse command request")?;

        log::info!(
            "Device received command: executionId={} payload={:?}",
            req.execution_id.as_str(),
            req.payload,
        );

        if req.execution_id.as_str() != exec_id.as_str() {
            return Err(format!(
                "executionId mismatch: cloud said {}, topic said {}",
                exec_id,
                req.execution_id.as_str()
            ));
        }
        if req.payload != (TestCommand { action: "echo", value: 42 }) {
            return Err(format!("unexpected payload: {:?}", req.payload));
        }

        // Optional progress report.
        agent
            .report_in_progress(req.execution_id.as_str(), None)
            .await
            .map_err(|e| format!("report_in_progress: {e:?}"))?;

        // Build a result echoing the action back, then succeed.
        let mut result = ResultMap::new();
        result
            .insert("echoed", CommandResultEntry::String { s: "echo" })
            .map_err(|_| "result insert")?;
        agent
            .succeed(req.execution_id.as_str(), Some(&result))
            .await
            .map_err(|e| format!("succeed: {e:?}"))?;

        log::info!("Device succeeded execution {}", req.execution_id.as_str());
        Ok::<(), String>(())
    };

    let mut transport = NalTransport::new(network, broker);

    let device_outcome = match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(60),
        select::select(stack.run(&mut transport), device_fut),
    )
    .await
    .map_err(|_| "outer test timeout (60s)")?
    {
        select::Either::First(_) => unreachable!("MQTT stack returned"),
        select::Either::Second(result) => result,
    };
    device_outcome?;

    // Cloud-side assertion.
    let exec_id = exec_id_cell
        .get()
        .ok_or("device task did not record an executionId")?;
    let status = ctx
        .wait_for_terminal(exec_id, std::time::Duration::from_secs(15))
        .await?;
    assert_eq!(
        status,
        CommandExecutionStatus::Succeeded,
        "cloud-side status mismatch (executionId={exec_id})"
    );

    Ok(())
}
