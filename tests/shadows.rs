mod common;

use common::{clock::SysClock, credentials, network::Network};
use heapless::String;
use mqttrust::Mqtt;
use mqttrust_core::{bbqueue::BBBuffer, EventLoop, MqttOptions, Notification, PublishNotification};
use native_tls::TlsConnector;
use rustot::shadows::{derive::ShadowState, Shadow, ShadowState};
use serde::{Deserialize, Serialize};

use smlang::statemachine;

static mut Q: BBBuffer<{ 1024 * 6 }> = BBBuffer::new();

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SensorType {
    Npn,
    Pnp,
    Analog,
}

impl Default for SensorType {
    fn default() -> Self {
        Self::Npn
    }
}

statemachine! {
    transitions: {
        *Init + Subscribed = Ready,
    }
}

pub struct TestContext {}

impl StateMachineContext for TestContext {}

#[derive(Debug, Default, Serialize, ShadowState)]
pub struct SensorConf {
    sensor_type: SensorType,
}

#[test]
fn test_shadows() {
    env_logger::init();

    let (p, c) = unsafe { Q.try_split_framed().unwrap() };

    log::info!("Starting shadows test...");

    let hostname = credentials::HOSTNAME.unwrap();
    let (thing_name, identity) = credentials::identity();

    let connector = TlsConnector::builder()
        .identity(identity)
        .add_root_certificate(credentials::root_ca())
        .build()
        .unwrap();

    let mut network = Network::new_tls(connector, std::string::String::from(hostname));

    let mut mqtt_eventloop = EventLoop::new(
        c,
        SysClock::new(),
        MqttOptions::new(thing_name, hostname.into(), 8883),
    );

    let mqtt_client = mqttrust_core::Client::new(p, thing_name);

    let mut sensor_conf_handler = Shadow::new(SensorConf::default(), &mqtt_client).unwrap();

    let mut test_state = StateMachine::new(TestContext {});

    loop {
        match mqtt_eventloop.connect(&mut network) {
            Ok(true) => {
                log::info!("Successfully connected to broker");
                sensor_conf_handler.get_shadow().unwrap();
            }
            Ok(false) => {}
            Err(nb::Error::WouldBlock) => continue,
            Err(e) => panic!("{:?}", e),
        }

        match mqtt_eventloop.yield_event(&mut network) {
            Ok(Notification::Suback(_)) => {
                log::info!("Suback");
                test_state.process_event(Events::Subscribed);
            }
            Ok(Notification::Publish(mut publish)) => {
                log::info!(
                    "received: {:?}",
                    core::str::from_utf8(&publish.payload).ok()
                );
                if let Some(update_state) = sensor_conf_handler
                    .handle_message(&publish.topic_name, &publish.payload)
                    .unwrap()
                {
                    log::info!(
                        "Setting sensor to {:?} from the cloud",
                        update_state.sensor_type
                    );
                }
            }
            Ok(n) => {
                log::trace!("{:?}", n);
            }
            _ => {}
        }
    }
}
