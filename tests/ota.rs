#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

mod common;

use std::{net::ToSocketAddrs, process};

use common::credentials;
use common::file_handler::{FileHandler, State as FileHandlerState};
use common::network::TlsNetwork;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_time::Duration;
use embedded_mqtt::{
    Config, DomainBroker, IpBroker, Message, Publish, QoS, RetainHandling, State, Subscribe,
    SubscribeTopic,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use static_cell::make_static;

use rustot::{
    jobs::{
        self,
        data_types::{DescribeJobExecutionResponse, NextJobExecutionChanged},
        JobTopic, StatusDetails,
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

fn handle_ota<'a, const SUBS: usize>(
    message: Message<'a, NoopRawMutex, SUBS>,
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
    let network = make_static!(TlsNetwork::new(hostname.to_owned(), identity));

    // Create the MQTT stack
    let broker =
        DomainBroker::<_, 128>::new(format!("{}:8883", hostname).as_str(), network).unwrap();
    let config =
        Config::new(thing_name, broker).keepalive_interval(embassy_time::Duration::from_secs(50));

    let state = make_static!(State::<NoopRawMutex, 4096, { 4096 * 10 }, 4>::new());
    let (mut stack, client) = embedded_mqtt::new(state, config, network);

    let client = make_static!(client);
    let mut file_handler = FileHandler::new("tests/assets/ota_file".to_owned());

    let ota_fut = async {
        let mut jobs_subscription = client
            .subscribe::<2>(Subscribe::new(&[
                SubscribeTopic {
                    topic_path: jobs::JobTopic::NotifyNext
                        .format::<64>(thing_name)?
                        .as_str(),
                    maximum_qos: QoS::AtLeastOnce,
                    no_local: false,
                    retain_as_published: false,
                    retain_handling: RetainHandling::SendAtSubscribeTime,
                },
                SubscribeTopic {
                    topic_path: jobs::JobTopic::DescribeAccepted("$next")
                        .format::<64>(thing_name)?
                        .as_str(),
                    maximum_qos: QoS::AtLeastOnce,
                    no_local: false,
                    retain_as_published: false,
                    retain_handling: RetainHandling::SendAtSubscribeTime,
                },
            ]))
            .await?;

        Updater::check_for_job(client).await?;

        let config = ota::config::Config::default();
        while let Some(message) = jobs_subscription.next().await {
            if let Some(mut file_ctx) = handle_ota(message, &config) {
                // We have an OTA job, leeeets go!
                Updater::perform_ota(client, client, file_ctx.clone(), &mut file_handler, &config)
                    .await?;

                assert_eq!(file_handler.plateform_state, FileHandlerState::Swap);

                // Run it twice in this particular integration test, in order to simulate image commit after bootloader swap
                file_ctx
                    .status_details
                    .insert(
                        heapless::String::try_from("self_test").unwrap(),
                        heapless::String::try_from("active").unwrap(),
                    )
                    .unwrap();
                Updater::perform_ota(client, client, file_ctx, &mut file_handler, &config).await?;

                return Ok(());
            }
        }

        Ok::<_, ota::error::OtaError>(())
    };

    match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(25),
        select::select(stack.run(), ota_fut),
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
