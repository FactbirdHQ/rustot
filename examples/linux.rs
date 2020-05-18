mod common;

use embedded_nal::Ipv4Addr;

use mqttrust::{MqttClient, MqttEvent, MqttOptions, Notification, Request};

use rustot::{
    jobs::{is_job_message, IotJobsData, JobAgent, JobDetails, JobStatus},
    ota::ota::{is_ota_message, OtaAgent, OtaConfig},
};

use common::file_handler::FileHandler;
use common::network::Network;
use common::timer::SysTimer;
use heapless::{consts, spsc::Queue};
use std::thread;

static mut Q: Queue<Request, consts::U10> = Queue(heapless::i::Queue::new());

fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Trace)
        .init();

    let (p, c) = unsafe { Q.split() };

    let network = Network;

    log::info!("Starting!");

    let thing_name = heapless::String::<heapless::consts::U32>::from("test_mini_2");

    // Connect to broker.hivemq.com:1883
    let mut mqtt_eventloop = MqttEvent::new(
        c,
        SysTimer::new(),
        MqttOptions::new(
            thing_name.as_str(),
            Ipv4Addr::new(52, 208, 158, 107).into(),
            8883,
        ),
    );

    let mqtt_client = MqttClient::new(p, thing_name);

    let file_handler = FileHandler::new();
    let mut job_agent = JobAgent::new();
    let mut ota_agent = OtaAgent::new(file_handler, SysTimer::new(), OtaConfig::default());

    nb::block!(mqtt_eventloop.connect(&network)).expect("Failed to connect to MQTT");

    job_agent.subscribe_to_jobs(&mqtt_client).unwrap();

    job_agent
        .describe_job_execution(&mqtt_client, "$next", None, None)
        .unwrap();

    thread::Builder::new()
        .name("eventloop".to_string())
        .spawn(move || loop {
            // ota_agent.request_timer_irq(&mqtt_client);

            match nb::block!(mqtt_eventloop.yield_event(&network)) {
                Ok(Notification::Publish(publish)) => {
                    if is_job_message(&publish.topic_name) {
                        match job_agent.handle_message(&mqtt_client, &publish) {
                            Ok(None) => {}
                            Ok(Some(job)) => {
                                log::debug!("Accepted a new JOB! {:?}", job);
                                match job.details {
                                    JobDetails::OtaJob(otajob) => {
                                        ota_agent.process_ota_job(&mqtt_client, otajob).unwrap()
                                    }
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                log::error!("[{}, {:?}]:", publish.topic_name, publish.qospid);
                                log::error!("{:?}", e);
                            }
                        }
                    } else if is_ota_message(&publish.topic_name) {
                        match ota_agent.handle_message(&mqtt_client, &publish) {
                            Ok(progress) => {
                                log::info!("OTA Progress: {}%", progress);
                                if progress == 100 {
                                    job_agent
                                        .update_job_execution(&mqtt_client, JobStatus::Succeeded)
                                        .unwrap();
                                }
                            }
                            Err(e) => {
                                log::error!("[{}, {:?}]:", publish.topic_name, publish.qospid);
                                log::error!("{:?}", e);
                            }
                        }
                    } else {
                        log::info!("Got some other incoming message {:?}", publish);
                    }
                }
                _ => {
                    // log::debug!("{:?}", n);
                }
            }
        })
        .unwrap();

    loop {
        thread::sleep(std::time::Duration::from_millis(5000));
    }
}
