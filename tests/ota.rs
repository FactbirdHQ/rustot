#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

mod common;

use std::{net::ToSocketAddrs, process};

use common::credentials;
use common::file_handler::FileHandler;
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

fn handle_job<'a, M: RawMutex, const SUBS: usize>(
    message: &'a Message<'_, M, SUBS>,
) -> Option<JobEventData<'a>> {
    match jobs::Topic::from_str(message.topic_name()) {
        Some(jobs::Topic::NotifyNext) => {
            let (execution_changed, _) =
                serde_json_core::from_slice::<NextJobExecutionChanged<Jobs>>(&message.payload())
                    .ok()?;
            let job = execution_changed.execution?;
            let ota_job = job.job_document?.ota_job()?;
            Some(JobEventData {
                job_name: job.job_id,
                ota_document: ota_job,
                status_details: job.status_details,
            })
        }
        Some(jobs::Topic::DescribeAccepted(_)) => {
            let (execution_changed, _) = serde_json_core::from_slice::<
                DescribeJobExecutionResponse<Jobs>,
            >(&message.payload())
            .ok()?;
            let job = execution_changed.execution?;
            let ota_job = job.job_document?.ota_job()?;
            Some(JobEventData {
                job_name: job.job_id,
                ota_document: ota_job,
                status_details: job.status_details,
            })
        }
        _ => None,
    }
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

        while let Some(message) = jobs_subscription.next().await {
            let config = ota::config::Config::default();
            if let Some(mut file_ctx) = match jobs::Topic::from_str(message.topic_name()) {
                Some(jobs::Topic::NotifyNext) => {
                    let (execution_changed, _) = serde_json_core::from_slice::<
                        NextJobExecutionChanged<Jobs>,
                    >(&message.payload())
                    .ok()
                    .unwrap();
                    let job = execution_changed.execution.unwrap();
                    let ota_job = job.job_document.unwrap().ota_job().unwrap();
                    FileContext::new_from(
                        JobEventData {
                            job_name: job.job_id,
                            ota_document: ota_job,
                            status_details: job.status_details,
                        },
                        0,
                        &config,
                    )
                    .ok()
                }
                Some(jobs::Topic::DescribeAccepted(_)) => {
                    let (execution_changed, _) = serde_json_core::from_slice::<
                        DescribeJobExecutionResponse<Jobs>,
                    >(&message.payload())
                    .ok()
                    .unwrap();
                    let job = execution_changed.execution.unwrap();
                    let ota_job = job.job_document.unwrap().ota_job().unwrap();
                    FileContext::new_from(
                        JobEventData {
                            job_name: job.job_id,
                            ota_document: ota_job,
                            status_details: job.status_details,
                        },
                        0,
                        &config,
                    )
                    .ok()
                }
                _ => None,
            } {
                drop(message);
                // We have an OTA job, leeeets go!
                let mut file_handler = FileHandler::new("tests/assets/ota_file".to_owned());
                Updater::perform_ota(client, client, file_ctx, &mut file_handler, config).await?;

                return Ok(());
            }
        }

        Ok::<_, ota::error::OtaError>(())
    };

    match select::select(stack.run(), ota_fut).await {
        select::Either::First(_) => {
            unreachable!()
        }
        select::Either::Second(result) => result.unwrap(),
    };
}
