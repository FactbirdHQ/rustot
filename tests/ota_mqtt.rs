#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

mod common;

use common::credentials;
use common::file_handler::{FileHandler, State as FileHandlerState};
use common::network::TlsNetwork;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_mqtt::transport::embedded_nal::NalTransport;
use embedded_mqtt::{
    Config, DomainBroker, Message, SliceBufferProvider, State, Subscribe, SubscribeTopic,
};
use futures::StreamExt;
use serde::Deserialize;
use static_cell::StaticCell;

use rustot::{
    jobs::{
        self,
        data_types::{DescribeJobExecutionResponse, NextJobExecutionChanged},
    },
    ota::{
        self,
        encoding::{json::OtaJob, FileContext},
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
                serde_json_core::from_slice::<NextJobExecutionChanged<Jobs>>(&message.payload())
                    .ok()?;
            execution_changed.execution?
        }
        Some(jobs::Topic::DescribeAccepted(_)) => {
            let (execution_changed, _) = serde_json_core::from_slice::<
                DescribeJobExecutionResponse<Jobs>,
            >(&message.payload())
            .ok()?;
            execution_changed.execution?
        }
        _ => return None,
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
async fn test_mqtt_ota() {
    env_logger::init();

    log::info!("Starting OTA test...");

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

    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 10 }>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = embedded_mqtt::new(state, config);

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
            .await?;

        Updater::check_for_job(&client).await?;

        let config = ota::config::Config::default();

        let message = jobs_subscription.next().await.unwrap();

        if let Some(mut file_ctx) = handle_ota(message, &config) {
            // Nested subscriptions are a problem for embedded-mqtt, so unsubscribe here
            jobs_subscription.unsubscribe().await.unwrap();

            // We have an OTA job, leeeets go!
            Updater::perform_ota(
                &client,
                &client,
                file_ctx.clone(),
                &mut file_handler,
                &config,
            )
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

            Updater::perform_ota(&client, &client, file_ctx, &mut file_handler, &config).await?;

            return Ok(());
        }

        Ok::<_, ota::error::OtaError>(())
    };

    let mut transport = NalTransport::new(network, broker);

    match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(25),
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
}
