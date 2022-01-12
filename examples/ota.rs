mod common;

use jobs::data_types::NextJobExecutionChanged;
use mqttrust_core::bbqueue::BBBuffer;
use mqttrust_core::PublishNotification;
use mqttrust_core::{EventLoop, MqttOptions, Notification};

use native_tls::TlsConnector;
use serde::Deserialize;

use common::clock::SysClock;
use common::file_handler::FileHandler;
use common::network::Network;
use ota::encoding::json::OtaJob;
use rustot::jobs::data_types::DescribeJobExecutionResponse;
use rustot::jobs::{self, StatusDetails};
use rustot::ota;
use rustot::ota::agent::OtaAgent;
use std::thread;

use crate::common::credentials;

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

fn main() {
    env_logger::init();

    let (p, c) = unsafe { Q.try_split_framed().unwrap() };

    let hostname = credentials::HOSTNAME.unwrap();

    let connector = TlsConnector::builder()
        .identity(credentials::identity())
        .add_root_certificate(credentials::root_ca())
        .build()
        .unwrap();

    let mut network = Network::new_tls(connector, String::from(hostname));

    let thing_name = "rustot-test";

    log::info!("Starting OTA example...");

    let mut mqtt_eventloop = EventLoop::new(
        c,
        SysClock::new(),
        MqttOptions::new(thing_name, hostname.into(), 8883),
    );

    let mqtt_client = mqttrust_core::Client::new(p, thing_name);

    let file_handler = FileHandler::new();

    nb::block!(mqtt_eventloop.connect(&mut network)).expect("Failed to connect to MQTT");

    log::info!("Successfully connected to broker");

    thread::Builder::new()
        .name("eventloop".to_string())
        .spawn(move || {
            let mut ota_agent =
                OtaAgent::builder(&mqtt_client, &mqtt_client, SysClock::new(), file_handler)
                    .build();

            ota_agent.init();

            let mut cnt = 0;
            let mut suspended = false;

            loop {
                ota_agent.timer_callback().expect("Failed timer callback!");

                match mqtt_eventloop.yield_event(&mut network) {
                    Ok(Notification::Publish(mut publish)) => {
                        // Check if the received file is a jobs topic, that we
                        // want to react to.
                        match handle_ota(&mut publish) {
                            Ok(OtaUpdate::JobUpdate(job_id, job_doc, status_details)) => {
                                log::debug!("Received job! Starting OTA! {:?}", job_doc.streamname);
                                ota_agent
                                    .job_update(job_id, &job_doc, status_details.as_ref())
                                    .expect("Failed to start OTA job");
                            }
                            Ok(OtaUpdate::Data(payload)) => {
                                ota_agent.handle_message(payload).ok();
                                cnt += 1;

                                if cnt > 1000 && !suspended {
                                    log::info!("Suspending current OTA Job");
                                    ota_agent.suspend().ok();
                                    suspended = true;
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

                if suspended && cnt < 1200 {
                    cnt += 1;
                    thread::sleep(std::time::Duration::from_millis(200));
                    if cnt >= 1200 {
                        log::info!("Resuming OTA Job");
                        ota_agent.resume().ok();
                    }
                }
                ota_agent.process_event().ok();
            }
        })
        .unwrap();

    loop {
        thread::sleep(std::time::Duration::from_millis(5000));
    }
}
