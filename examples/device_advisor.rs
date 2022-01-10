mod common;

use jobs::data_types::NextJobExecutionChanged;
use mqttrust::{Mqtt, QoS, SubscribeTopic};
use mqttrust_core::bbqueue::BBBuffer;
use mqttrust_core::{EventError, PublishNotification};
use mqttrust_core::{EventLoop, MqttOptions, Notification};

use serde::Deserialize;

use common::file_handler::FileHandler;
use common::network::Network;
use common::timer::SysClock;
use ota::encoding::json::OtaJob;
use rustot::jobs::data_types::DescribeJobExecutionResponse;
use rustot::jobs::{self, StatusDetails, MAX_JOB_ID_LEN};
use rustot::ota;
use rustot::ota::agent::OtaAgent;
use std::thread;

use crate::common::credentials;

static mut Q: BBBuffer<{ 1024 * 6 }> = BBBuffer::new();

#[derive(Debug, Deserialize)]
pub enum Jobs {
    #[serde(rename = "afr_ota")]
    Ota(OtaJob),
}

impl Jobs {
    pub fn ota_job(self) -> Option<OtaJob> {
        match self {
            Jobs::Ota(ota_job) => Some(ota_job),
        }
    }
}

enum OtaUpdate {
    JobUpdate(
        heapless::String<MAX_JOB_ID_LEN>,
        OtaJob,
        Option<StatusDetails>,
    ),
    Data,
}

fn handle_ota(publish: &PublishNotification) -> Result<OtaUpdate, ()> {
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
            return Ok(OtaUpdate::Data);
        }
        _ => {}
    }
    Err(())
}

fn main() {
    env_logger::init();

    let (p, c) = unsafe { Q.try_split_framed().unwrap() };

    let mut network = Network;

    let thing_name = "rustot";

    log::info!("Starting OTA example...");

    let mut mqtt_eventloop = EventLoop::new(
        c,
        SysClock::new(),
        MqttOptions::new(thing_name, credentials::HOSTNAME.into(), 8883),
    );

    let mqtt_client = mqttrust_core::Client::new(p, thing_name);

    let file_handler = FileHandler::new();

    thread::Builder::new()
        .name("eventloop".to_string())
        .spawn(move || {
            // let mut ota_agent =
            //     OtaAgent::builder(&mqtt_client, &mqtt_client, SysClock::new(), file_handler)
            //         .build();

            // ota_agent.init();

            let mut cnt = 0;
            let mut suspended = false;

            loop {
                loop {
                    match nb::block!(mqtt_eventloop.connect(&mut network)) {
                        Err(_) => {}
                        Ok(true) => {
                            log::info!("Successfully connected to broker");
                            mqtt_client
                                .subscribe(&[SubscribeTopic {
                                    topic_path: "rustot/device/advisor",
                                    qos: QoS::AtLeastOnce,
                                }])
                                .unwrap();

                            mqtt_client
                                .publish(
                                    "rustot/device/advisor/hello",
                                    b"Hello from rustot",
                                    QoS::AtLeastOnce,
                                )
                                .unwrap();
                            break;
                        }
                        Ok(false) => {
                            break;
                        }
                    }
                }

                // ota_agent.timer_callback().expect("Failed timer callback!");

                match mqtt_eventloop.yield_event(&mut network) {
                    Ok(Notification::Publish(mut publish)) => {
                        // Check if the received file is a jobs topic, that we
                        // want to react to.
                        // match handle_ota(&publish) {
                        //     Ok(OtaUpdate::JobUpdate(job_id, job_doc, status_details)) => {
                        //         log::debug!("Received job! Starting OTA! {:?}", job_doc.streamname);
                        //         ota_agent
                        //             .job_update(job_id.as_str(), &job_doc, status_details.as_ref())
                        //             .expect("Failed to start OTA job");
                        //     }
                        //     Ok(OtaUpdate::Data) => {
                        //         ota_agent.handle_message(&mut publish.payload).ok();
                        //         cnt += 1;

                        //         if cnt > 1000 && !suspended {
                        //             log::info!("Suspending current OTA Job");
                        //             ota_agent.suspend().ok();
                        //             suspended = true;
                        //         }
                        //     }
                        //     Err(_) => {}
                        // }
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
                        // ota_agent.resume().ok();
                    }
                }
                // ota_agent.process_event().ok();
            }
        })
        .unwrap();

    loop {
        thread::sleep(std::time::Duration::from_millis(5000));
    }
}
