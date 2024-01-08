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
    ota::{self, encoding::json::OtaJob, JobEventData, Updater},
};

#[derive(Debug, Deserialize)]
pub enum Jobs<'a> {
    #[serde(rename = "afr_ota")]
    #[serde(borrow)]
    Ota(OtaJob<'a>),
}

fn handle_job<'a, M: RawMutex, const SUBS: usize>(
    message: &'a Message<'_, M, SUBS>,
) -> Option<JobEventData<'a>> {
    match jobs::Topic::from_str(message.topic_name()) {
        Some(jobs::Topic::NotifyNext) => {
            let (execution_changed, _) =
                serde_json_core::from_slice::<NextJobExecutionChanged<Jobs>>(&message.payload())
                    .map_err(drop)?;
            let job = execution_changed.execution.ok_or(())?;
            let ota_job = job.job_document.ok_or(())?.ota_job().ok_or(())?;
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
            .map_err(drop)?;
            let job = execution_changed.execution.ok_or(())?;
            let ota_job = job.job_document.ok_or(())?.ota_job().ok_or(())?;
            Some(JobEventData {
                job_name: job.job_id,
                ota_document: ota_job,
                status_details: job.status_details,
            })
        }
        _ => None,
    }
}

pub struct FileInfo {
    pub file_path: String,
    pub filesize: usize,
    pub signature: ota::encoding::json::Signature,
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

    let state = make_static!(State::<NoopRawMutex, 2048, 4096, 2>::new());
    let (mut stack, client) = embedded_mqtt::new(state, config, network);

    let client = make_static!(client);

    let ota_fut = async {
        let jobs_subscription = client
            .subscribe(Subscribe::new(&[SubscribeTopic {
                topic_path: jobs::JobTopic::NotifyNext
                    .format::<64>(thing_name)?
                    .as_str(),
                maximum_qos: QoS::AtLeastOnce,
                no_local: false,
                retain_as_published: false,
                retain_handling: RetainHandling::SendAtSubscribeTime,
            }]))
            .await?;

        while let Some(message) = jobs_subscription.next().await {
            if let Some(job_details) = handle_job(&message) {
                // We have an OTA job, leeeets go!
                let config = ota::config::Config::default();
                let mut file_handler = FileHandler::new();
                Updater::perform_ota(client, client, job_details, &mut file_hander, config).await;
            }
        }
    };

    match select::select(stack.run(), ota_fut).await {
        select::Either::First(_) => {
            unreachable!()
        }
        select::Either::Second(result) => result.unwrap(),
    };

    // let mut expected_file = File::open("tests/assets/ota_file").unwrap();
    // let mut expected_data = Vec::new();
    // expected_file.read_to_end(&mut expected_data).unwrap();
    // let mut expected_hasher = Sha256::new();
    // expected_hasher.update(&expected_data);
    // let expected_hash = expected_hasher.finalize();

    // let file_info = file_info.unwrap();

    // log::info!(
    //     "Comparing {:?} with {:?}",
    //     "tests/assets/ota_file",
    //     file_info.file_path
    // );
    // let mut file = File::open(file_info.file_path.clone()).unwrap();
    // let mut data = Vec::new();
    // file.read_to_end(&mut data).unwrap();
    // drop(file);
    // std::fs::remove_file(file_info.file_path).unwrap();

    // assert_eq!(data.len(), file_info.filesize);

    // let mut hasher = Sha256::new();
    // hasher.update(&data);
    // assert_eq!(hasher.finalize().deref(), expected_hash.deref());

    // // Check file signature
    // match file_info.signature {
    //     ota::encoding::json::Signature::Sha1Rsa(_) => {
    //         panic!("Unexpected signature format: Sha1Rsa. Expected Sha256Ecdsa")
    //     }
    //     ota::encoding::json::Signature::Sha256Rsa(_) => {
    //         panic!("Unexpected signature format: Sha256Rsa. Expected Sha256Ecdsa")
    //     }
    //     ota::encoding::json::Signature::Sha1Ecdsa(_) => {
    //         panic!("Unexpected signature format: Sha1Ecdsa. Expected Sha256Ecdsa")
    //     }
    //     ota::encoding::json::Signature::Sha256Ecdsa(sig) => {
    //         assert_eq!(&sig, "This is my custom signature\\n")
    //     }
    // }
}
