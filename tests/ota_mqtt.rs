#![cfg(feature = "transfer_mqtt")]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

mod common;

use common::credentials;
use common::file_handler::{FileHandler, State as FileHandlerState};
use common::network::TlsNetwork;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use mqttrust::transport::embedded_nal::NalTransport;
use mqttrust::{Config, DomainBroker, State};
use serial_test::serial;
use static_cell::StaticCell;

use aws_credential_types::provider::SharedCredentialsProvider;
use rustot::{
    jobs::stream::{parse_job_message, JobAgent},
    mqtt::{Mqtt, OwnedMessage},
    transfer::{self, pal::PalError, Transfer},
};

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn test_mqtt_ota() {
    let _ = env_logger::Builder::from_default_env()
        .filter_module("serial_test", log::LevelFilter::Warn)
        .try_init();

    log::info!("Starting OTA test...");

    let ctx = match common::aws_ota::setup().await {
        Some(ctx) => ctx,
        None => {
            log::info!("Skipping OTA test: no valid AWS credentials or role assumption failed");
            return;
        }
    };

    let test_result = common::aws_ota::catch_unwind_future(run_ota_happy_path()).await;

    // Capture cloud-side status before cleanup changes it
    let cloud_status = ctx.describe_job_execution().await;

    // Always cleanup
    ctx.cleanup().await;

    match test_result {
        Ok(inner) => {
            inner.unwrap();
            // Assert cloud-side job succeeded
            let (status, details) = cloud_status.expect("Failed to describe job execution");
            assert_eq!(
                status,
                aws_sdk_iot::types::JobExecutionStatus::Succeeded,
                "Expected cloud job status SUCCEEDED, got {:?} with details: {:?}",
                status,
                details
            );
        }
        Err(panic) => {
            if let Ok((status, details)) = &cloud_status {
                log::error!(
                    "Test panicked! Cloud job status at panic: {:?}, details: {:?}",
                    status,
                    details
                );
            }
            std::panic::resume_unwind(panic);
        }
    }
}

async fn run_ota_happy_path() -> Result<(), transfer::error::TransferError> {
    let (thing_name, identity) = credentials::identity();

    let hostname = credentials::HOSTNAME.unwrap();

    static NETWORK: StaticCell<TlsNetwork> = StaticCell::new();
    let network = NETWORK.init(TlsNetwork::new(hostname.to_owned(), identity));

    // Create the MQTT stack
    let broker = DomainBroker::<_, 128>::new_with_port(hostname, 8883, network).unwrap();
    let config = Config::builder()
        .client_id(thing_name.try_into().unwrap())
        .keepalive_interval(embassy_time::Duration::from_secs(50))
        .build();

    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 20 }>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = mqttrust::new(state, config);

    let mut file_handler = FileHandler::new("tests/assets/ota_file".to_owned());

    let ota_fut = async {
        let mqtt = Mqtt(&client);
        let job_agent = JobAgent::new(&mqtt);

        let mut sub = job_agent.subscribe().await?;
        let message = sub.next_message().await.unwrap();
        let mut owned = OwnedMessage::<256, 4096>::from_ref(&message).unwrap();
        drop(message);
        sub.unsubscribe().await.unwrap();

        let execution = parse_job_message::<common::OtaJobs>(&mut owned)
            .expect("Failed to parse OTA job document");

        let job_ctx = common::ota_context_from_execution::<common::file_handler::TestStatusDetails>(
            execution,
        )?;

        let ota_config = transfer::config::Config {
            block_size: 4096,
            ..Default::default()
        };

        // We have an OTA job, leeeets go!
        Transfer::perform_ota(&mqtt, &mqtt, &job_ctx, &mut file_handler, &ota_config).await?;

        assert_eq!(file_handler.plateform_state, FileHandlerState::Swap);

        log::info!("Running OTA handler second time to verify state match...");

        // Run it twice in this particular integration test, in order to
        // simulate image commit after bootloader swap.
        // Construct a new context with self_test set to "active".
        let mut status = rustot::transfer::StatusDetails::new();
        status.set_self_test("active");
        let job_ctx2 = rustot::transfer::encoding::JobContext {
            status,
            ..job_ctx.clone()
        };

        Transfer::perform_ota(&mqtt, &mqtt, &job_ctx2, &mut file_handler, &ota_config).await?;

        Ok::<_, transfer::error::TransferError>(())
    };

    let mut transport = NalTransport::new(network, broker);

    match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(120),
        select::select(stack.run(&mut transport), ota_fut),
    )
    .await
    .unwrap()
    {
        select::Either::First(_) => {
            unreachable!()
        }
        select::Either::Second(result) => result.unwrap(),
    };

    assert_eq!(file_handler.plateform_state, FileHandlerState::Boot);

    Ok(())
}

/// Test OTA failure path - simulates a signature check failure during close_file
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn test_mqtt_ota_signature_failure() {
    let _ = env_logger::Builder::from_default_env()
        .filter_module("serial_test", log::LevelFilter::Warn)
        .try_init();

    log::info!("Starting OTA signature failure test...");

    let ctx = match common::aws_ota::setup().await {
        Some(ctx) => ctx,
        None => {
            log::info!("Skipping OTA signature failure test: no valid AWS credentials or role assumption failed");
            return;
        }
    };

    let test_result = common::aws_ota::catch_unwind_future(run_ota_signature_failure()).await;

    // Capture cloud-side status before cleanup
    let cloud_status = ctx.describe_job_execution().await;

    // Always cleanup
    ctx.cleanup().await;

    match test_result {
        Ok(inner) => {
            inner.unwrap();
            // Assert cloud-side job failed (signature check failure)
            let (status, details) = cloud_status.expect("Failed to describe job execution");
            assert_eq!(
                status,
                aws_sdk_iot::types::JobExecutionStatus::Failed,
                "Expected cloud job status FAILED, got {:?} with details: {:?}",
                status,
                details
            );
        }
        Err(panic) => {
            if let Ok((status, details)) = &cloud_status {
                log::error!(
                    "Test panicked! Cloud job status at panic: {:?}, details: {:?}",
                    status,
                    details
                );
            }
            std::panic::resume_unwind(panic);
        }
    }
}

/// Test OTA cancel mid-download — verifies device detects cancellation.
///
/// When an in-progress OTA job is force-cancelled from the cloud, the device
/// execution transitions to CANCELED and AWS IoT sends a `notify-next`
/// notification. The device detects this via its subscription to the
/// notify-next topic and aborts the download with `UserAbort`.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn test_mqtt_ota_cancel() {
    let _ = env_logger::Builder::from_default_env()
        .filter_module("serial_test", log::LevelFilter::Warn)
        .try_init();

    log::info!("Starting OTA cancel test...");

    let ctx = match common::aws_ota::setup().await {
        Some(ctx) => ctx,
        None => {
            log::info!(
                "Skipping OTA cancel test: no valid AWS credentials or role assumption failed"
            );
            return;
        }
    };

    let job_id = ctx.job_id().to_owned();
    let iot_creds = ctx.iot_creds();
    let region = ctx.region().clone();

    let test_result =
        common::aws_ota::catch_unwind_future(run_ota_cancel(job_id, iot_creds, region)).await;

    // Capture cloud-side status before cleanup
    let cloud_status = ctx.describe_job_execution().await;

    // Always cleanup
    ctx.cleanup().await;

    match test_result {
        Ok(inner) => {
            inner.unwrap();
            // Assert cloud-side job is CANCELED (force-cancel transitions immediately)
            let (status, details) = cloud_status.expect("Failed to describe job execution");
            assert_eq!(
                status,
                aws_sdk_iot::types::JobExecutionStatus::Canceled,
                "Expected cloud job status CANCELED, got {:?} with details: {:?}",
                status,
                details
            );
        }
        Err(panic) => {
            if let Ok((status, details)) = &cloud_status {
                log::error!(
                    "Test panicked! Cloud job status at panic: {:?}, details: {:?}",
                    status,
                    details
                );
            }
            std::panic::resume_unwind(panic);
        }
    }
}

async fn run_ota_cancel(
    job_id: String,
    iot_creds: SharedCredentialsProvider,
    region: aws_config::Region,
) -> Result<(), transfer::error::TransferError> {
    let (thing_name, identity) = credentials::identity();

    let hostname = credentials::HOSTNAME.unwrap();

    static NETWORK: StaticCell<TlsNetwork> = StaticCell::new();
    let network = NETWORK.init(TlsNetwork::new(hostname.to_owned(), identity));

    // Create the MQTT stack
    let broker =
        DomainBroker::<_, 128>::new(format!("{}:8883", hostname).as_str(), network).unwrap();
    let config = Config::builder()
        .client_id(thing_name.try_into().unwrap())
        .keepalive_interval(embassy_time::Duration::from_secs(50))
        .build();

    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 20 }>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = mqttrust::new(state, config);

    let mut file_handler = FileHandler::new("tests/assets/ota_file".to_owned());

    let ota_fut = async {
        let mqtt = Mqtt(&client);
        let job_agent = JobAgent::new(&mqtt);

        let mut sub = job_agent.subscribe().await?;
        let message = sub.next_message().await.unwrap();
        let mut owned = OwnedMessage::<256, 4096>::from_ref(&message).unwrap();
        drop(message);
        sub.unsubscribe().await.unwrap();

        let execution = parse_job_message::<common::OtaJobs>(&mut owned)
            .expect("Failed to parse OTA job document");

        let job_ctx = common::ota_context_from_execution::<common::file_handler::TestStatusDetails>(
            execution,
        )?;

        // Use small block_size + max_blocks_per_request=1 to slow down the
        // download, giving us a comfortable window to cancel the job
        // mid-transfer. With block_size=1024 the 386KB file needs ~378 blocks
        // (~29s at 1 block/request), so cancelling at 3s leaves plenty of
        // blocks remaining for the device to detect the cancellation.
        let ota_config = transfer::config::Config {
            block_size: 1024,
            max_blocks_per_request: 1,
            ..Default::default()
        };

        // Run OTA and cancel concurrently
        let ota_future =
            Transfer::perform_ota(&mqtt, &mqtt, &job_ctx, &mut file_handler, &ota_config);

        let cancel_future = async {
            // Wait for download to start, then force-cancel
            embassy_time::Timer::after(embassy_time::Duration::from_secs(3)).await;
            log::info!("Force-cancelling job {} from cloud...", job_id);
            common::aws_ota::force_cancel_job(&job_id, &iot_creds, &region)
                .await
                .expect("Failed to force-cancel job");
        };

        // Wrap in a timeout — if perform_ota doesn't detect cancel, it
        // will complete the download normally (proving the device doesn't
        // handle cancellation)
        let result = embassy_time::with_timeout(
            embassy_time::Duration::from_secs(60),
            embassy_futures::join::join(ota_future, cancel_future),
        )
        .await;

        match result {
            Ok((ota_result, ())) => {
                // perform_ota returned — it should have detected the cancel
                // and returned an error, not completed successfully
                assert!(
                    ota_result.is_err(),
                    "perform_ota should detect job cancellation and return an error, \
                     but it completed successfully — the device did not handle the cancel"
                );
                log::info!(
                    "perform_ota detected cancellation as expected: {:?}",
                    ota_result.err()
                );
            }
            Err(_timeout) => {
                panic!(
                    "perform_ota timed out at 60s — device neither completed \
                     nor detected the cancellation"
                );
            }
        }

        Ok::<_, transfer::error::TransferError>(())
    };

    let mut transport = NalTransport::new(network, broker);

    match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(120),
        select::select(stack.run(&mut transport), ota_fut),
    )
    .await
    .unwrap()
    {
        select::Either::First(_) => {
            unreachable!()
        }
        select::Either::Second(result) => result.unwrap(),
    };

    Ok(())
}

/// Test rejecting a job with a reason via JobAgent::reject_job.
///
/// Creates an OTA job (reusing the OTA test infrastructure), but instead of
/// performing the OTA, immediately rejects it with a reason string. Verifies
/// that the cloud-side job status is REJECTED and the reason appears in
/// statusDetails.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn test_mqtt_job_reject() {
    let _ = env_logger::Builder::from_default_env()
        .filter_module("serial_test", log::LevelFilter::Warn)
        .try_init();

    log::info!("Starting job reject test...");

    let ctx = match common::aws_ota::setup().await {
        Some(ctx) => ctx,
        None => {
            log::info!(
                "Skipping job reject test: no valid AWS credentials or role assumption failed"
            );
            return;
        }
    };

    let test_result = common::aws_ota::catch_unwind_future(run_job_reject()).await;

    let cloud_status = ctx.describe_job_execution().await;

    ctx.cleanup().await;

    match test_result {
        Ok(inner) => {
            inner.unwrap();
            let (status, details) = cloud_status.expect("Failed to describe job execution");
            assert_eq!(
                status,
                aws_sdk_iot::types::JobExecutionStatus::Rejected,
                "Expected cloud job status REJECTED, got {:?} with details: {:?}",
                status,
                details
            );
            assert_eq!(
                details.get("reason").map(|s| s.as_str()),
                Some("unsupported job type"),
                "Expected reject reason in status details, got: {:?}",
                details
            );
        }
        Err(panic) => {
            if let Ok((status, details)) = &cloud_status {
                log::error!(
                    "Test panicked! Cloud job status at panic: {:?}, details: {:?}",
                    status,
                    details
                );
            }
            std::panic::resume_unwind(panic);
        }
    }
}

async fn run_job_reject() -> Result<(), transfer::error::TransferError> {
    let (thing_name, identity) = credentials::identity();

    let hostname = credentials::HOSTNAME.unwrap();

    static NETWORK: StaticCell<TlsNetwork> = StaticCell::new();
    let network = NETWORK.init(TlsNetwork::new(hostname.to_owned(), identity));

    let broker = DomainBroker::<_, 128>::new_with_port(hostname, 8883, network).unwrap();
    let config = Config::builder()
        .client_id(thing_name.try_into().unwrap())
        .keepalive_interval(embassy_time::Duration::from_secs(50))
        .build();

    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 20 }>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = mqttrust::new(state, config);

    let reject_fut = async {
        let mqtt = Mqtt(&client);
        let job_agent = JobAgent::new(&mqtt);

        let mut sub = job_agent.subscribe().await?;
        let message = sub.next_message().await.unwrap();
        let mut owned = OwnedMessage::<256, 4096>::from_ref(&message).unwrap();
        drop(message);
        sub.unsubscribe().await.unwrap();

        let execution =
            parse_job_message::<common::OtaJobs>(&mut owned).expect("Failed to parse job document");

        job_agent
            .reject_job(execution.job_id, "unsupported job type")
            .await?;

        Ok::<_, transfer::error::TransferError>(())
    };

    let mut transport = NalTransport::new(network, broker);

    match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(30),
        select::select(stack.run(&mut transport), reject_fut),
    )
    .await
    .unwrap()
    {
        select::Either::First(_) => {
            unreachable!()
        }
        select::Either::Second(result) => result.unwrap(),
    };

    Ok(())
}

async fn run_ota_signature_failure() -> Result<(), transfer::error::TransferError> {
    let (thing_name, identity) = credentials::identity();

    let hostname = credentials::HOSTNAME.unwrap();

    static NETWORK: StaticCell<TlsNetwork> = StaticCell::new();
    let network = NETWORK.init(TlsNetwork::new(hostname.to_owned(), identity));

    // Create the MQTT stack
    let broker =
        DomainBroker::<_, 128>::new(format!("{}:8883", hostname).as_str(), network).unwrap();
    let config = Config::builder()
        .client_id(thing_name.try_into().unwrap())
        .keepalive_interval(embassy_time::Duration::from_secs(50))
        .build();

    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 20 }>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = mqttrust::new(state, config);

    // Configure file handler to fail with SignatureCheckFailed
    let mut file_handler = FileHandler::new("tests/assets/ota_file".to_owned())
        .with_close_failure(PalError::SignatureCheckFailed);

    let ota_fut = async {
        let mqtt = Mqtt(&client);
        let job_agent = JobAgent::new(&mqtt);

        let mut sub = job_agent.subscribe().await?;
        let message = sub.next_message().await.unwrap();
        let mut owned = OwnedMessage::<256, 4096>::from_ref(&message).unwrap();
        drop(message);
        sub.unsubscribe().await.unwrap();

        let execution = parse_job_message::<common::OtaJobs>(&mut owned)
            .expect("Failed to parse OTA job document");

        let job_ctx = common::ota_context_from_execution::<common::file_handler::TestStatusDetails>(
            execution,
        )?;

        let ota_config = transfer::config::Config {
            block_size: 4096,
            ..Default::default()
        };

        // This should fail with SignatureCheckFailed
        let result =
            Transfer::perform_ota(&mqtt, &mqtt, &job_ctx, &mut file_handler, &ota_config).await;

        // Verify the OTA failed as expected
        assert!(
            result.is_err(),
            "OTA should have failed with SignatureCheckFailed"
        );
        log::info!("OTA failed as expected: {:?}", result.err());

        // Platform state should still be Boot (not Swap) since we failed
        assert_eq!(
            file_handler.plateform_state,
            FileHandlerState::Boot,
            "Platform state should remain Boot after failure"
        );

        Ok::<_, transfer::error::TransferError>(())
    };

    let mut transport = NalTransport::new(network, broker);

    match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(120),
        select::select(stack.run(&mut transport), ota_fut),
    )
    .await
    .unwrap()
    {
        select::Either::First(_) => {
            unreachable!()
        }
        select::Either::Second(result) => result.unwrap(),
    };

    Ok(())
}
