mod common;

use common::{clock::SysClock, credentials, network::Network};
use mqttrust_core::{bbqueue::BBBuffer, EventLoop, MqttOptions, Notification};
use native_tls::TlsConnector;
use rustot::shadows::{derive::ShadowState, Shadow};
use serde::{Deserialize, Serialize};

use smlang::statemachine;

const Q_SIZE: usize = 1024 * 6;
static mut Q: BBBuffer<Q_SIZE> = BBBuffer::new();

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
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

impl SensorType {
    pub fn next_type(&self) -> Self {
        match self {
            Self::Npn => Self::Pnp,
            Self::Pnp => Self::Analog,
            Self::Analog => Self::Npn,
        }
    }
}

statemachine! {
    transitions: {
        *Begin + Run / begin = Update(SensorType),
        Update(SensorType) + CheckState / check = Check(Option<SensorType>),
        Check(Option<SensorType>) + UpdateState / update = Update(SensorType),
    }
}

impl core::fmt::Debug for States {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Begin => write!(f, "Self::Begin"),
            Self::Update(t) => write!(f, "Self::Update({:?})", t),
            Self::Check(t) => write!(f, "Self::Check({:?})", t),
        }
    }
}

pub struct TestContext<'a> {
    shadow: Shadow<'a, SensorConf, mqttrust_core::Client<'static, 'static, Q_SIZE>, ()>,
}

impl<'a> StateMachineContext for TestContext<'a> {
    fn check(&mut self, sensor_type: &SensorType) -> Option<SensorType> {
        if matches!(sensor_type, SensorType::Analog) {
            None
        } else {
            Some(*sensor_type)
        }
    }

    fn update(&mut self, sensor_type: &Option<SensorType>) -> SensorType {
        sensor_type.unwrap().next_type()
    }

    fn begin(&mut self) -> SensorType {
        SensorType::Npn
    }
}

impl<'a> StateMachine<TestContext<'a>> {
    pub fn spin(&mut self, notification: Notification) -> bool {
        log::info!("State: {:?}", self.state());
        match (self.state(), notification) {
            (&States::Begin, Notification::Suback(_)) => {
                self.context_mut().shadow.get_shadow().unwrap();
                self.process_event(Events::Run).unwrap();
            }
            (&States::Update(desired_type), Notification::Publish(_)) => {
                self.context_mut()
                    .shadow
                    .update(|_current, desired| {
                        desired.sensor_type = Some(desired_type);
                    })
                    .unwrap();
                self.process_event(Events::CheckState).unwrap();
            }
            (&States::Check(expected_type), Notification::Publish(publish))
                if matches!(
                    publish.topic_name.as_str(),
                    "$aws/things/rustot-test/shadow/update/accepted"
                ) =>
            {
                log::info!(
                    "{:?}: {:?}",
                    publish.topic_name,
                    core::str::from_utf8(&publish.payload)
                );
                self.context_mut()
                    .shadow
                    .handle_message(&publish.topic_name, &publish.payload)
                    .unwrap();

                match expected_type {
                    Some(expected_type) => {
                        assert_eq!(self.context_mut().shadow.get().sensor_type, expected_type);
                        self.context_mut().shadow.get_shadow().unwrap();
                        self.process_event(Events::UpdateState).unwrap();
                    }
                    None => {
                        return true;
                    }
                }
            }
            (_, Notification::Publish(publish)) => {
                self.context_mut()
                    .shadow
                    .handle_message(&publish.topic_name, &publish.payload)
                    .unwrap();
            }
            _ => {}
        }

        false
    }
}

#[derive(Debug, Default, ShadowState, Serialize)]
pub struct SensorConf {
    #[unit_shadow_field]
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
        MqttOptions::new(thing_name, hostname.into(), 8883).set_clean_session(true),
    );

    let mqtt_client = mqttrust_core::Client::new(p, thing_name);

    let mut test_state = StateMachine::new(TestContext {
        shadow: Shadow::new(SensorConf::default(), &mqtt_client).unwrap(),
    });

    loop {
        if nb::block!(mqtt_eventloop.connect(&mut network)).expect("to connect to mqtt") {
            log::info!("Successfully connected to broker");
        }

        let notification = nb::block!(mqtt_eventloop.yield_event(&mut network)).unwrap();
        if test_state.spin(notification) {
            break;
        }
    }
}
