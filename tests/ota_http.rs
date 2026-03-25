#![cfg(all(feature = "ota_http_reqwest", feature = "mqtt_rumqttc"))]
#![allow(async_fn_in_trait)]

mod common;

use common::credentials;
use common::file_handler::{FileHandler, State as FileHandlerState};
use serial_test::serial;

use rustot::{
    mqtt::{rumqttc::RumqttcClient, Mqtt, MqttClient, MqttSubscription},
    ota::{
        self,
        data_interface::http::{HttpInterface, ReqwestClient},
        Updater,
    },
};

#[tokio::test]
#[serial]
async fn test_http_ota() {
    let _ = env_logger::Builder::from_default_env()
        .filter_module("serial_test", log::LevelFilter::Warn)
        .try_init();

    log::info!("Starting HTTP OTA test...");

    let ctx =
        match common::aws_ota::setup_with_protocols(&[aws_sdk_iot::types::Protocol::Http]).await {
            Some(ctx) => ctx,
            None => {
                log::info!(
                    "Skipping HTTP OTA test: no valid AWS credentials or role assumption failed"
                );
                return;
            }
        };

    let test_result = common::aws_ota::catch_unwind_future(run_ota_http()).await;

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

async fn run_ota_http() -> Result<(), ota::error::OtaError> {
    let thing_name = option_env!("THING_NAME").unwrap_or("rustot-test");
    let hostname = credentials::HOSTNAME.unwrap();
    let pw = std::env::var("IDENTITY_PASSWORD").unwrap_or_default();

    // Set up rumqttc with native-tls (PKCS12 client cert + PEM CA)
    let mut mqtt_options = rumqttc::MqttOptions::new(thing_name, hostname, 8883);
    mqtt_options.set_keep_alive(std::time::Duration::from_secs(50));
    mqtt_options.set_transport(rumqttc::Transport::tls_with_config(
        rumqttc::TlsConfiguration::SimpleNative {
            ca: include_bytes!("secrets/root-ca.pem").to_vec(),
            client_auth: Some((include_bytes!("secrets/identity.pfx").to_vec(), pw)),
        },
    ));

    let (rumqttc_client, _eventloop_handle) = RumqttcClient::new(mqtt_options, 10);
    rumqttc_client.wait_connected().await;

    let mqtt = Mqtt(&rumqttc_client);

    // Subscribe to job topics
    let mut jobs_subscription = rumqttc_client
        .subscribe(&[
            (
                &rustot::jobs::JobTopic::NotifyNext
                    .format::<64>(thing_name)
                    .map_err(|_| ota::error::OtaError::Overflow)?,
                rustot::mqtt::QoS::AtMostOnce,
            ),
            (
                &rustot::jobs::JobTopic::DescribeAccepted("$next")
                    .format::<64>(thing_name)
                    .map_err(|_| ota::error::OtaError::Overflow)?,
                rustot::mqtt::QoS::AtMostOnce,
            ),
        ])
        .await
        .map_err(|_| ota::error::OtaError::Mqtt)?;

    Updater::check_for_job(&mqtt).await?;

    let ota_config = ota::config::Config {
        block_size: 4096,
        ..Default::default()
    };

    let message = jobs_subscription.next_message().await.unwrap();

    let mut file_ctx = common::handle_ota(message, &ota_config)
        .expect("Failed to parse OTA job document — check logs for details");

    jobs_subscription
        .unsubscribe()
        .await
        .map_err(|_| ota::error::OtaError::Mqtt)?;

    log::info!(
        "OTA job received! Protocols: {:?}, update_data_url present: {}",
        file_ctx.protocols,
        file_ctx.update_data_url.is_some()
    );

    let mut file_handler = FileHandler::new("tests/assets/ota_file".to_owned());

    // HTTP for data interface (file download via Range requests)
    let http_client = ReqwestClient::new(reqwest::Client::new());
    let http_interface = HttpInterface::new(http_client);

    // MQTT (rumqttc) for control, HTTP (reqwest) for data
    Updater::perform_ota(
        &mqtt,
        &http_interface,
        file_ctx.clone(),
        &mut file_handler,
        &ota_config,
    )
    .await?;

    assert_eq!(file_handler.plateform_state, FileHandlerState::Swap);

    log::info!("Running OTA handler second time to verify state match...");

    // Run it twice to simulate image commit after bootloader swap
    file_ctx
        .status_details
        .insert(
            heapless::String::try_from("self_test").unwrap(),
            heapless::String::try_from("active").unwrap(),
        )
        .unwrap();

    Updater::perform_ota(
        &mqtt,
        &http_interface,
        file_ctx,
        &mut file_handler,
        &ota_config,
    )
    .await?;

    assert_eq!(file_handler.plateform_state, FileHandlerState::Boot);

    Ok(())
}
