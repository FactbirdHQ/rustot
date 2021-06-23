mod common;

use jobs::data_types::NextJobExecutionChanged;
use mqttrust_core::{Client, EventLoop, MqttOptions, Notification, OwnedRequest};
use serde::Deserialize;

use common::file_handler::FileHandler;
use common::network::Network;
use common::timer::SysClock;
use heapless::spsc::Queue;
use ota::encoding::json::OtaJob;
use rustot::jobs;
use rustot::jobs::data_types::DescribeJobExecutionResponse;
use rustot::jobs::data_types::JobExecution;
use rustot::ota;
use rustot::ota::agent::OtaAgent;
use std::thread;

static mut Q: Queue<OwnedRequest<128, 512>, 10> = Queue::new();

#[derive(Debug, Deserialize)]
pub enum Jobs {
    #[serde(rename = "afr_ota")]
    Ota(OtaJob),
}

fn main() {
    env_logger::init();

    let (p, c) = unsafe { Q.split() };

    let mut network = Network;

    let thing_name = "rustot-test";

    let mut mqtt_eventloop = EventLoop::new(
        c,
        SysClock::new(),
        MqttOptions::new(
            thing_name,
            "a69ih9fwq4cti-ats.iot.eu-west-1.amazonaws.com".into(),
            8883,
        ),
    );

    let mqtt_client = Client::new(p, thing_name);

    let file_handler = FileHandler::new();

    nb::block!(mqtt_eventloop.connect(&mut network)).expect("Failed to connect to MQTT");

    thread::Builder::new()
        .name("eventloop".to_string())
        .spawn(move || {
            let mut ota_agent =
                OtaAgent::builder(&mqtt_client, &mqtt_client, SysClock::new(), file_handler)
                    .build();

            ota_agent.init();

            loop {
                ota_agent.timer_callback();

                match mqtt_eventloop.yield_event(&mut network) {
                    Ok(Notification::Publish(mut publish)) => {
                        // Check if the received file is a jobs topic, that we
                        // want to react to.
                        match jobs::Topic::from_str(publish.topic_name.as_str()) {
                            Some(jobs::Topic::NotifyNext) => {
                                log::debug!("Got job update!");
                                if let Ok(NextJobExecutionChanged {
                                    execution:
                                        Some(JobExecution {
                                            job_id,
                                            job_document: Some(Jobs::Ota(job_doc)),
                                            status_details,
                                            ..
                                        }),
                                    ..
                                }) =
                                    NextJobExecutionChanged::<Jobs>::from_payload(&publish.payload)
                                {
                                    ota_agent
                                        .job_update(job_id.as_str(), job_doc, status_details)
                                        .expect("Failed to start OTA job");
                                }
                            }
                            Some(jobs::Topic::DescribeAccepted(_)) => {
                                if let Ok((
                                    DescribeJobExecutionResponse {
                                        execution:
                                            Some(JobExecution {
                                                job_document: Some(Jobs::Ota(job_doc)),
                                                status_details,
                                                job_id,
                                                ..
                                            }),
                                        ..
                                    },
                                    _,
                                )) = serde_json_core::from_slice(&publish.payload)
                                {
                                    log::debug!(
                                        "Received job! Starting OTA! {:?}",
                                        job_doc.streamname
                                    );
                                    ota_agent
                                        .job_update(job_id.as_str(), job_doc, status_details)
                                        .expect("Failed to start OTA job");
                                }
                            }
                            Some(topic) => {
                                log::debug!("Got some other job update! {:?}", topic);
                                log::debug!("{}", unsafe {
                                    core::str::from_utf8_unchecked(&publish.payload)
                                });
                            }
                            _ => {}
                        }

                        match ota::Topic::from_str(publish.topic_name.as_str()) {
                            Some(ota::Topic::Data(_, _)) => {
                                ota_agent
                                    .handle_message(&mut publish.payload)
                                    .expect("Failed to handle stream data");
                            }
                            Some(topic) => {
                                log::debug!("Got some other ota update! {:?}", topic);
                            }
                            _ => {}
                        }
                    }
                    Ok(n) => {
                        log::trace!("{:?}", n);
                    }
                    _ => {}
                }
                ota_agent.process_event().ok();
            }
        })
        .unwrap();

    loop {
        thread::sleep(std::time::Duration::from_millis(5000));
    }
}
