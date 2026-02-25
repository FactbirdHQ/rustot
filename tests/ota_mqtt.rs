#![cfg(feature = "ota_mqtt_data")]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

mod common;

use common::credentials;
use common::file_handler::{FileHandler, State as FileHandlerState};
use common::network::TlsNetwork;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use mqttrust::transport::embedded_nal::NalTransport;
use mqttrust::{
    Config, DomainBroker, Message, SliceBufferProvider, State, Subscribe, SubscribeTopic,
};
use serde::Deserialize;
use serial_test::serial;
use static_cell::StaticCell;

use rustot::{
    jobs::{
        self,
        data_types::{DescribeJobExecutionResponse, NextJobExecutionChanged},
    },
    mqtt::Mqtt,
    ota::{
        self,
        encoding::{json::OtaJob, FileContext},
        pal::OtaPalError,
        JobEventData, Updater,
    },
};

#[derive(Debug, Deserialize)]
pub enum Jobs<'a> {
    #[serde(rename = "afr_ota")]
    #[serde(borrow)]
    Ota(OtaJob<'a>),
}

impl<'a> Jobs<'a> {
    pub fn ota_job(self) -> Option<OtaJob<'a>> {
        match self {
            Jobs::Ota(ota_job) => Some(ota_job),
        }
    }
}

fn handle_ota<'a>(
    message: Message<'a, NoopRawMutex, SliceBufferProvider<'a>>,
    config: &ota::config::Config,
) -> Option<FileContext> {
    let job = match jobs::Topic::from_str(message.topic_name()) {
        Some(jobs::Topic::NotifyNext) => {
            let (execution_changed, _) =
                serde_json_core::from_slice::<NextJobExecutionChanged<Jobs>>(message.payload())
                    .ok()?;
            execution_changed.execution?
        }
        Some(jobs::Topic::DescribeAccepted(_)) => {
            let (execution_changed, _) = serde_json_core::from_slice::<
                DescribeJobExecutionResponse<Jobs>,
            >(message.payload())
            .ok()?;

            if execution_changed.execution.is_none() {
                if std::env::var("CI").is_ok() {
                    panic!("No OTA jobs queued?");
                }
                return None;
            }

            execution_changed.execution?
        }
        _ => {
            return None;
        }
    };

    let ota_job = job.job_document?.ota_job()?;

    FileContext::new_from(
        JobEventData {
            job_name: job.job_id,
            ota_document: ota_job,
            status_details: job.status_details,
        },
        0,
        config,
    )
    .ok()
}

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

async fn run_ota_happy_path() -> Result<(), ota::error::OtaError> {
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
        let mut jobs_subscription = client
            .subscribe::<2>(
                Subscribe::builder()
                    .topics(&[
                        SubscribeTopic::builder()
                            .topic_path(
                                jobs::JobTopic::NotifyNext
                                    .format::<64>(thing_name)?
                                    .as_str(),
                            )
                            .build(),
                        SubscribeTopic::builder()
                            .topic_path(
                                jobs::JobTopic::DescribeAccepted("$next")
                                    .format::<64>(thing_name)?
                                    .as_str(),
                            )
                            .build(),
                    ])
                    .build(),
            )
            .await
            .map_err(|_| ota::error::OtaError::Mqtt)?;

        let mqtt = Mqtt(&client);
        Updater::check_for_job(&mqtt).await?;

        let mut config = ota::config::Config::default();
        config.block_size = 4096;

        let message = jobs_subscription.next_message().await.unwrap();

        if let Some(mut file_ctx) = handle_ota(message, &config) {
            // Nested subscriptions are a problem for mqttrust, so unsubscribe here
            jobs_subscription.unsubscribe().await.unwrap();

            // We have an OTA job, leeeets go!
            Updater::perform_ota(&mqtt, &mqtt, file_ctx.clone(), &mut file_handler, &config)
                .await?;

            assert_eq!(file_handler.plateform_state, FileHandlerState::Swap);

            log::info!("Running OTA handler second time to verify state match...");

            // Run it twice in this particular integration test, in order to
            // simulate image commit after bootloader swap
            file_ctx
                .status_details
                .insert(
                    heapless::String::try_from("self_test").unwrap(),
                    heapless::String::try_from("active").unwrap(),
                )
                .unwrap();

            Updater::perform_ota(&mqtt, &mqtt, file_ctx, &mut file_handler, &config).await?;

            return Ok(());
        }

        Ok::<_, ota::error::OtaError>(())
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

async fn run_ota_signature_failure() -> Result<(), ota::error::OtaError> {
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
        .with_close_failure(OtaPalError::SignatureCheckFailed);

    let ota_fut = async {
        let mut jobs_subscription = client
            .subscribe::<2>(
                Subscribe::builder()
                    .topics(&[
                        SubscribeTopic::builder()
                            .topic_path(
                                jobs::JobTopic::NotifyNext
                                    .format::<64>(thing_name)?
                                    .as_str(),
                            )
                            .build(),
                        SubscribeTopic::builder()
                            .topic_path(
                                jobs::JobTopic::DescribeAccepted("$next")
                                    .format::<64>(thing_name)?
                                    .as_str(),
                            )
                            .build(),
                    ])
                    .build(),
            )
            .await
            .map_err(|_| ota::error::OtaError::Mqtt)?;

        let mqtt = Mqtt(&client);
        Updater::check_for_job(&mqtt).await?;

        let config = ota::config::Config::default();

        let message = jobs_subscription.next_message().await.unwrap();

        if let Some(file_ctx) = handle_ota(message, &config) {
            jobs_subscription.unsubscribe().await.unwrap();

            // This should fail with SignatureCheckFailed
            let result =
                Updater::perform_ota(&mqtt, &mqtt, file_ctx, &mut file_handler, &config).await;

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

            return Ok(());
        }

        Ok::<_, ota::error::OtaError>(())
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
