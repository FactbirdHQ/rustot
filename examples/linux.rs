mod common;

use embedded_nal::Ipv4Addr;

use mqttrust_core::{Client, EventLoop, MqttOptions, Notification, Request};

use rustot::{
    jobs::{is_job_message, IotJobsData, JobAgent, JobDetails},
    ota::ota::{is_ota_message, OtaAgent, OtaConfig},
};

use common::file_handler::FileHandler;
use common::network::Network;
use common::timer::SysTimer;
use heapless::spsc::Queue;
use std::thread;

static mut Q: Queue<Request<heapless::Vec<u8, 512>>, 10> = Queue::new();

fn main() {
    let (p, c) = unsafe { Q.split() };

    let mut network = Network;

    // #[cfg(feature = "logging")]
    // log::info!("Starting!");

    let thing_name = "test_mini_2";

    // Connect to broker.hivemq.com:1883
    let mut mqtt_eventloop = EventLoop::new(
        c,
        SysTimer::new(),
        MqttOptions::new(thing_name, Ipv4Addr::new(52, 208, 158, 107).into(), 8883),
    );

    let mut mqtt_client = Client::new(p, thing_name);

    let file_handler = FileHandler::new();
    let mut job_agent = JobAgent::new(3);
    let mut ota_agent = OtaAgent::new(file_handler, SysTimer::new(), OtaConfig::default());

    nb::block!(mqtt_eventloop.connect(&mut network)).expect("Failed to connect to MQTT");

    job_agent.subscribe_to_jobs(&mut mqtt_client).unwrap();

    job_agent
        .describe_job_execution(&mut mqtt_client, "$next", None, None)
        .unwrap();

    thread::Builder::new()
        .name("eventloop".to_string())
        .spawn(move || loop {
            // ota_agent.request_timer_irq(&mqtt_client);

            match nb::block!(mqtt_eventloop.yield_event(&mut network)) {
                Ok(Notification::Publish(mut publish)) => {
                    if is_job_message(&publish.topic_name) {
                        match job_agent.handle_message(&mut mqtt_client, &publish) {
                            Ok(None) => {}
                            Ok(Some(job)) => {
                                // #[cfg(feature = "logging")]
                                // log::debug!("Accepted a new JOB! {:?}", job);
                                match job.details {
                                    JobDetails::OtaJob(otajob) => ota_agent
                                        .process_ota_job(&mut mqtt_client, &otajob)
                                        .unwrap(),
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                // #[cfg(feature = "logging")]
                                // log::error!("[{}, {:?}]:", publish.topic_name, publish.qospid);
                                // #[cfg(feature = "logging")]
                                // log::error!("{:?}", e);
                            }
                        }
                    } else if is_ota_message(&publish.topic_name) {
                        match ota_agent.handle_message(
                            &mut mqtt_client,
                            &mut job_agent,
                            &mut publish,
                        ) {
                            Ok(progress) => {
                                // #[cfg(feature = "logging")]
                                // log::info!("OTA Progress: {}%", progress);
                            }
                            Err(e) => {
                                // #[cfg(feature = "logging")]
                                // log::error!("[{}, {:?}]:", publish.topic_name, publish.qospid);
                                // #[cfg(feature = "logging")]
                                // log::error!("{:?}", e);
                            }
                        }
                    } else {
                        // #[cfg(feature = "logging")]
                        // log::info!("Got some other incoming message {:?}", publish);
                    }
                }
                _ => {
                    // #[cfg(feature = "logging")]
                    // log::debug!("{:?}", n);
                }
            }
        })
        .unwrap();

    loop {
        thread::sleep(std::time::Duration::from_millis(5000));
    }
}
