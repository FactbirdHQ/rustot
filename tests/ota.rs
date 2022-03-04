mod common;

use mqttrust_core::{bbqueue::BBBuffer, EventLoop, MqttOptions, Notification, PublishNotification};
use native_tls::TlsConnector;
use rustot::ota::state::States;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, ops::Deref};

use common::{clock::SysClock, credentials, file_handler::FileHandler, network::Network};
use rustot::{
    jobs::{
        self,
        data_types::{DescribeJobExecutionResponse, NextJobExecutionChanged},
        StatusDetails,
    },
    ota::{self, agent::OtaAgent, encoding::json::OtaJob},
};

static mut Q: BBBuffer<{ 1024 * 6 }> = BBBuffer::new();

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

enum OtaUpdate<'a> {
    JobUpdate(&'a str, OtaJob<'a>, Option<StatusDetails>),
    Data(&'a mut [u8]),
}

fn handle_ota<'a>(publish: &'a mut PublishNotification) -> Result<OtaUpdate<'a>, ()> {
    match jobs::Topic::from_str(publish.topic_name.as_str()) {
        Some(jobs::Topic::NotifyNext) => {
            let (execution_changed, _) =
                serde_json_core::from_slice::<NextJobExecutionChanged<Jobs>>(&publish.payload)
                    .map_err(drop)?;
            let job = execution_changed.execution.ok_or(())?;
            let ota_job = job.job_document.ok_or(())?.ota_job().ok_or(())?;
            return Ok(OtaUpdate::JobUpdate(
                job.job_id,
                ota_job,
                job.status_details,
            ));
        }
        Some(jobs::Topic::DescribeAccepted(_)) => {
            let (execution_changed, _) =
                serde_json_core::from_slice::<DescribeJobExecutionResponse<Jobs>>(&publish.payload)
                    .map_err(drop)?;
            let job = execution_changed.execution.ok_or(())?;
            let ota_job = job.job_document.ok_or(())?.ota_job().ok_or(())?;
            return Ok(OtaUpdate::JobUpdate(
                job.job_id,
                ota_job,
                job.status_details,
            ));
        }
        _ => {}
    }

    match ota::Topic::from_str(publish.topic_name.as_str()) {
        Some(ota::Topic::Data(_, _)) => {
            return Ok(OtaUpdate::Data(&mut publish.payload));
        }
        _ => {}
    }
    Err(())
}

pub struct FileInfo {
    pub file_path: String,
    pub filesize: usize,
    pub signature: ota::encoding::json::Signature,
}

#[test]
fn test_mqtt_ota() {
    // Make sure this times out in case something went wrong setting up the OTA
    // job in AWS IoT before starting.
    timebomb::timeout_ms(test_mqtt_ota_inner, 100_000)
}

fn test_mqtt_ota_inner() {
    env_logger::init();

    let (p, c) = unsafe { Q.try_split_framed().unwrap() };

    log::info!("Starting OTA test...");

    let hostname = credentials::HOSTNAME.unwrap();
    let (thing_name, identity) = credentials::identity();

    let connector = TlsConnector::builder()
        .identity(identity)
        .add_root_certificate(credentials::root_ca())
        .build()
        .unwrap();

    let mut network = Network::new_tls(connector, String::from(hostname));

    let mut mqtt_eventloop = EventLoop::new(
        c,
        SysClock::new(),
        MqttOptions::new(thing_name, hostname.into(), 8883).set_clean_session(true),
    );

    let mqtt_client = mqttrust_core::Client::new(p, thing_name);

    let file_handler = FileHandler::new();

    let mut ota_agent =
        OtaAgent::builder(&mqtt_client, &mqtt_client, SysClock::new(), file_handler).build();

    let mut file_info = None;

    loop {
        match mqtt_eventloop.connect(&mut network) {
            Ok(true) => {
                log::info!("Successfully connected to broker");
                ota_agent.init();
            }
            Ok(false) => {}
            Err(nb::Error::WouldBlock) => continue,
            Err(e) => panic!("{:?}", e),
        }

        match mqtt_eventloop.yield_event(&mut network) {
            Ok(Notification::Publish(mut publish)) => {
                // Check if the received file is a jobs topic, that we
                // want to react to.
                match handle_ota(&mut publish) {
                    Ok(OtaUpdate::JobUpdate(job_id, job_doc, status_details)) => {
                        log::debug!("Received job! Starting OTA! {:?}", job_doc.streamname);

                        let file = &job_doc.files[0];
                        file_info.replace(FileInfo {
                            file_path: file.filepath.to_string(),
                            filesize: file.filesize,
                            signature: file.signature(),
                        });
                        ota_agent
                            .job_update(job_id, &job_doc, status_details.as_ref())
                            .expect("Failed to start OTA job");
                    }
                    Ok(OtaUpdate::Data(payload)) => {
                        log::info!("GOT DATA!");
                        if ota_agent.handle_message(payload).is_err() {
                            match ota_agent.state() {
                                States::CreatingFile => log::info!("State: CreatingFile"),
                                States::Ready => log::info!("State: Ready"),
                                States::RequestingFileBlock => {
                                    log::info!("State: RequestingFileBlock")
                                }
                                States::RequestingJob => log::info!("State: RequestingJob"),
                                States::Restarting => log::info!("State: Restarting"),
                                States::Suspended => log::info!("State: Suspended"),
                                States::WaitingForFileBlock => {
                                    log::info!("State: WaitingForFileBlock")
                                }
                                States::WaitingForJob => log::info!("State: WaitingForJob"),
                            }
                        }
                    }
                    Err(_) => {}
                }
            }
            Ok(n) => {
                log::trace!("{:?}", n);
            }
            _ => {}
        }

        ota_agent.timer_callback().expect("Failed timer callback!");

        match ota_agent.process_event() {
            // Use the restarting state to indicate finished
            Ok(States::Restarting) => break,
            _ => {}
        }
    }

    let mut expected_file = File::open("assets/ota_file").unwrap();
    let mut expected_data = Vec::new();
    expected_file.read_to_end(&mut expected_data).unwrap();
    let mut expected_hasher = Sha256::new();
    expected_hasher.update(&expected_data);
    let expected_hash = expected_hasher.finalize();

    let file_info = file_info.unwrap();

    let mut file = File::open(format!("assets/{}", file_info.file_path)).unwrap();
    let mut data = Vec::new();
    file.read_to_end(&mut data).unwrap();

    assert_eq!(data.len(), file_info.filesize);

    let mut hasher = Sha256::new();
    hasher.update(&data);
    assert_eq!(hasher.finalize().deref(), expected_hash.deref());

    // Check file signature
    match file_info.signature {
        ota::encoding::json::Signature::Sha1Rsa(_) => panic!(),
        ota::encoding::json::Signature::Sha256Rsa(_) => panic!(),
        ota::encoding::json::Signature::Sha1Ecdsa(_) => panic!(),
        ota::encoding::json::Signature::Sha256Ecdsa(sig) => {
            assert_eq!(&sig, "This is my custom signature")
        }
    }
}
