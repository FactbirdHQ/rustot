#![cfg(all(feature = "transfer_http_reqwest", feature = "mqtt_rumqttc"))]
#![allow(async_fn_in_trait)]

mod common;

use common::credentials;
use common::file_handler::{FileHandler, State as FileHandlerState};
use serial_test::serial;

use rustot::{
    jobs::stream::{parse_job_message, JobAgent},
    mqtt::{rumqttc::RumqttcClient, Mqtt, MqttClient, MqttSubscription},
    transfer::{
        self,
        data_interface::http::{HttpInterface, ReqwestClient},
        Transfer,
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

async fn run_ota_http() -> Result<(), transfer::error::TransferError> {
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
    let job_agent = JobAgent::new(&mqtt);

    let mut sub = job_agent.subscribe().await?;
    let mut message = sub.next_message().await.unwrap();

    let execution = parse_job_message::<common::OtaJobs>(&mut message)
        .expect("Failed to parse OTA job document — check logs for details");

    let job_ctx =
        common::ota_context_from_execution::<common::file_handler::TestStatusDetails>(execution)?;

    log::info!(
        "OTA job received! Protocols: {:?}, update_data_url present: {}",
        job_ctx.protocols,
        job_ctx.update_data_url.is_some()
    );

    let mut file_handler = FileHandler::new("tests/assets/ota_file".to_owned());

    let ota_config = transfer::config::Config {
        block_size: 4096,
        ..Default::default()
    };

    // HTTP for data interface (file download via Range requests)
    let http_client = ReqwestClient::new();
    let http_interface = HttpInterface::new(http_client);

    // MQTT (rumqttc) for control, HTTP (reqwest) for data
    Transfer::perform_ota(
        &mqtt,
        &http_interface,
        &job_ctx,
        &mut file_handler,
        &ota_config,
    )
    .await?;

    assert_eq!(file_handler.plateform_state, FileHandlerState::Swap);

    log::info!("Running OTA handler second time to verify state match...");

    // Simulate image commit after bootloader swap — construct new context
    // with self_test set to "active"
    let mut status = rustot::transfer::StatusDetails::new();
    status.set_self_test("active");
    let job_ctx2 = rustot::transfer::encoding::JobContext {
        status,
        ..job_ctx.clone()
    };

    Transfer::perform_ota(
        &mqtt,
        &http_interface,
        &job_ctx2,
        &mut file_handler,
        &ota_config,
    )
    .await?;

    assert_eq!(file_handler.plateform_state, FileHandlerState::Boot);

    Ok(())
}
