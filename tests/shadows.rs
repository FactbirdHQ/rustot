//!
//! ## Integration test of `AWS IoT Shadows`
//!
//!
//! This test simulates updates of the shadow state from both device side &
//! cloud side. Cloud side updates are done by publishing directly to the shadow
//! topics, and ignoring the resulting update accepted response. Device side
//! updates are done through the shadow API provided by this crate.
//!
//! The test runs through the following update sequence:
//! 1. Setup clean starting point (`desired = null, reported = null`)
//! 2. Do a `GetShadow` request to sync empty state
//! 3. Update to initial shadow state from the device
//! 4. Assert on the initial state
//! 5. Update state from device
//! 6. Assert on shadow state
//! 7. Update state from cloud
//! 8. Assert on shadow state
//! 9. Update state from device
//! 10. Assert on shadow state
//! 11. Update state from cloud
//! 12. Assert on shadow state
//!

mod common;

use core::fmt::Write;

use common::{
    clock::SysClock,
    credentials,
    network::{Network, TlsNetwork},
};
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_mqtt::{
    transport::embedded_nal::{self, NalTransport},
    DomainBroker, Properties, Publish, QoS, State,
};
use embedded_nal::Ipv4Addr;
use mqttrust::Mqtt;
use mqttrust_core::{bbqueue::BBBuffer, EventLoop, MqttOptions, Notification};
use native_tls::TlsConnector;
use rustot::shadows::{
    derive::ShadowState, topics::Topic, Patch, Shadow, ShadowPatch, ShadowState,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use smlang::statemachine;
use static_cell::StaticCell;

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConfigId(pub u8);

impl Serialize for ConfigId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut str = heapless::String::<3>::new();
        write!(str, "{}", self.0).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&str)
    }
}

impl<'de> Deserialize<'de> for ConfigId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        heapless::String::<3>::deserialize(deserializer)?
            .parse()
            .map(ConfigId)
            .map_err(serde::de::Error::custom)
    }
}

impl From<u8> for ConfigId {
    fn from(v: u8) -> Self {
        Self(v)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkMap<K: Eq, V, const N: usize>(heapless::LinearMap<K, Option<Patch<V>>, N>);

impl<K, V, const N: usize> NetworkMap<K, V, N>
where
    K: Eq,
{
    pub fn insert(&mut self, k: impl Into<K>, v: V) -> Result<(), ()> {
        self.0.insert(k.into(), Some(Patch::Set(v))).map_err(drop)?;
        Ok(())
    }

    pub fn remove(&mut self, k: impl Into<K>) -> Result<(), ()> {
        self.0.insert(k.into(), None).map_err(drop)?;
        Ok(())
    }
}

impl<K, V, const N: usize> ShadowPatch for NetworkMap<K, V, N>
where
    K: Clone + Default + Eq + Serialize + DeserializeOwned,
    V: Clone + Default + Serialize + DeserializeOwned,
{
    type PatchState = NetworkMap<K, V, N>;

    fn apply_patch(&mut self, opt: Self::PatchState) {
        for (id, network) in opt.0.into_iter() {
            match network {
                Some(Patch::Set(v)) => {
                    self.insert(id.clone(), v.clone()).ok();
                }
                None | Some(Patch::Unset) => {
                    self.remove(id.clone()).ok();
                }
            }
        }
    }
}

const MAX_NETWORKS: usize = 5;
type KnownNetworks = NetworkMap<ConfigId, ConnectionOptions, MAX_NETWORKS>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, ShadowState)]
#[shadow("wifi")]
pub struct WifiConfig {
    pub enabled: bool,

    pub known_networks: KnownNetworks,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionOptions {
    pub ssid: heapless::String<64>,
    pub password: Option<heapless::String<64>>,

    pub ip: Option<Ipv4Addr>,
    pub subnet: Option<Ipv4Addr>,
    pub gateway: Option<Ipv4Addr>,
}

#[derive(Debug, Clone)]
pub enum UpdateAction {
    Insert(u8, ConnectionOptions),
    Remove(u8),
    Enabled(bool),
}

statemachine! {
    transitions: {
        *Begin + Delete = DeleteShadow,
        DeleteShadow + Get = GetShadow,
        GetShadow + Load / load_initial = LoadShadow(Option<KnownNetworks>),
        LoadShadow(Option<KnownNetworks>) + CheckInitial / check_initial = Check(Option<KnownNetworks>),
        UpdateFromDevice(UpdateAction) + CheckState / check = Check(Option<KnownNetworks>),
        UpdateFromCloud(UpdateAction) + Ack = AckUpdate,
        AckUpdate + CheckState / check_cloud = Check(Option<KnownNetworks>),
        Check(Option<KnownNetworks>) + UpdateStateFromDevice / get_next_device = UpdateFromDevice(UpdateAction),
        Check(Option<KnownNetworks>) + UpdateStateFromCloud / get_next_cloud = UpdateFromCloud(UpdateAction),
    }
}

impl core::fmt::Debug for States {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Begin => write!(f, "Self::Begin"),
            Self::DeleteShadow => write!(f, "Self::DeleteShadow"),
            Self::GetShadow => write!(f, "Self::GetShadow"),
            Self::AckUpdate => write!(f, "Self::AckUpdate"),
            Self::LoadShadow(t) => write!(f, "Self::LoadShadow({:?})", t),
            Self::UpdateFromDevice(t) => write!(f, "Self::UpdateFromDevice({:?})", t),
            Self::UpdateFromCloud(t) => write!(f, "Self::UpdateFromCloud({:?})", t),
            Self::Check(t) => write!(f, "Self::Check({:?})", t),
        }
    }
}

fn asserts(id: usize) -> ConnectionOptions {
    match id {
        0 => ConnectionOptions {
            ssid: heapless::String::from("MySSID"),
            password: None,
            ip: None,
            subnet: None,
            gateway: None,
        },
        1 => ConnectionOptions {
            ssid: heapless::String::from("MyProtectedSSID"),
            password: Some(heapless::String::from("SecretPass")),
            ip: None,
            subnet: None,
            gateway: None,
        },
        2 => ConnectionOptions {
            ssid: heapless::String::from("CloudSSID"),
            password: Some(heapless::String::from("SecretCloudPass")),
            ip: Some(Ipv4Addr::new(1, 2, 3, 4)),
            subnet: None,
            gateway: None,
        },
        _ => panic!("Unknown assert ID"),
    }
}

pub struct TestContext<'a> {
    shadow: Shadow<'a, WifiConfig, mqttrust_core::Client<'static, 'static, Q_SIZE>>,
    update_cnt: u8,
}

impl<'a> StateMachineContext for TestContext<'a> {
    fn check_initial(
        &mut self,
        _last_update_action: &Option<KnownNetworks>,
    ) -> Option<KnownNetworks> {
        self.check(&UpdateAction::Remove(0))
    }

    fn check_cloud(&mut self) -> Option<KnownNetworks> {
        self.check(&UpdateAction::Remove(0))
    }

    fn check(&mut self, _last_update_action: &UpdateAction) -> Option<KnownNetworks> {
        let mut known_networks = KnownNetworks::default();

        match self.update_cnt {
            0 => {
                // After load_initial
                known_networks.insert(0, asserts(0)).unwrap();
                known_networks.insert(1, asserts(1)).unwrap();
            }
            1 => {
                // After get_next_device
                known_networks.remove(0).unwrap();
                known_networks.insert(1, asserts(1)).unwrap();
            }
            2 => {
                // After get_next_cloud
                known_networks.remove(0).unwrap();
                known_networks.insert(1, asserts(1)).unwrap();
                known_networks.insert(2, asserts(2)).unwrap();
            }
            3 => {
                // After get_next_device
                known_networks.insert(0, asserts(0)).unwrap();
                known_networks.insert(1, asserts(1)).unwrap();
                known_networks.insert(2, asserts(2)).unwrap();
            }
            4 => {
                // After get_next_cloud
                known_networks.insert(0, asserts(0)).unwrap();
                known_networks.insert(1, asserts(1)).unwrap();
                known_networks.remove(2).unwrap();
            }
            5 => return None,
            _ => {}
        }

        Some(known_networks)
    }

    fn get_next_device(&mut self, _: &Option<KnownNetworks>) -> UpdateAction {
        self.update_cnt += 1;
        match self.update_cnt {
            1 => UpdateAction::Remove(0),
            3 => UpdateAction::Insert(0, asserts(0)),
            5 => UpdateAction::Remove(0),
            _ => panic!("Unexpected update counter in `get_next_device`"),
        }
    }

    fn get_next_cloud(&mut self, _: &Option<KnownNetworks>) -> UpdateAction {
        self.update_cnt += 1;

        match self.update_cnt {
            2 => UpdateAction::Insert(2, asserts(2)),
            4 => UpdateAction::Remove(2),
            _ => panic!("Unexpected update counter in `get_next_cloud`"),
        }
    }

    fn load_initial(&mut self) -> Option<KnownNetworks> {
        let mut known_networks = KnownNetworks::default();
        known_networks.insert(0, asserts(0)).unwrap();
        known_networks.insert(1, asserts(1)).unwrap();
        Some(known_networks)
    }
}

impl<'a> StateMachine<TestContext<'a>> {
    pub fn spin(
        &mut self,
        notification: Notification,
        mqtt_client: &embedded_mqtt::MqttClient<'a, M, 1>,
    ) -> bool {
        log::info!("State: {:?}", self.state());
        match (self.state(), notification) {
            (&States::Begin, Notification::Suback(_)) => {
                self.process_event(Events::Delete).unwrap();
            }
            (&States::DeleteShadow, Notification::Suback(_)) => {
                mqtt_client
                    .publish(Publish {
                        dup: false,
                        qos: embedded_mqtt::QoS::AtLeastOnce,
                        retain: false,
                        pid: None,
                        topic_name: &Topic::Update
                            .format::<128>(
                                mqtt_client.client_id(),
                                <WifiConfig as ShadowState>::NAME,
                            )
                            .unwrap(),
                        payload: b"{\"state\":{\"desired\":null,\"reported\":null}}",
                        properties: embedded_mqtt::Properties::Slice(&[]),
                    })
                    .await
                    .unwrap();

                self.process_event(Events::Get).unwrap();
            }
            (&States::GetShadow, Notification::Publish(publish))
                if matches!(
                    publish.topic_name.as_str(),
                    "$aws/things/rustot-test/shadow/name/wifi/update/accepted"
                ) =>
            {
                self.context_mut().shadow.get_shadow().unwrap();
                self.process_event(Events::Load).unwrap();
            }
            (&States::LoadShadow(ref initial_map), Notification::Publish(publish))
                if matches!(
                    publish.topic_name.as_str(),
                    "$aws/things/rustot-test/shadow/name/wifi/get/accepted"
                ) =>
            {
                let initial_map = initial_map.clone();

                self.context_mut()
                    .shadow
                    .update(|_current, desired| {
                        desired.known_networks = Some(initial_map.unwrap());
                    })
                    .unwrap();
                self.process_event(Events::CheckInitial).unwrap();
            }
            (&States::UpdateFromDevice(ref update_action), Notification::Publish(publish))
                if matches!(
                    publish.topic_name.as_str(),
                    "$aws/things/rustot-test/shadow/name/wifi/get/accepted"
                ) =>
            {
                let action = update_action.clone();
                self.context_mut()
                    .shadow
                    .update(|current, desired| match action {
                        UpdateAction::Insert(id, options) => {
                            let mut desired_map = current.known_networks.clone();
                            desired_map.insert(id, options).unwrap();
                            desired.known_networks = Some(desired_map);
                        }
                        UpdateAction::Remove(id) => {
                            let mut desired_map = current.known_networks.clone();
                            desired_map.remove(id).unwrap();
                            desired.known_networks = Some(desired_map);
                        }
                        UpdateAction::Enabled(en) => {
                            desired.enabled = Some(en);
                        }
                    })
                    .unwrap();
                self.process_event(Events::CheckState).unwrap();
            }
            (&States::UpdateFromCloud(ref update_action), Notification::Publish(publish))
                if matches!(
                    publish.topic_name.as_str(),
                    "$aws/things/rustot-test/shadow/name/wifi/get/accepted"
                ) =>
            {
                let desired_known_networks = match update_action {
                    UpdateAction::Insert(id, options) => format!(
                        "\"known_networks\": {{\"{}\":{{\"set\":{}}}}}",
                        id,
                        serde_json_core::to_string::<_, 256>(options).unwrap()
                    ),
                    UpdateAction::Remove(id) => {
                        format!("\"known_networks\": {{\"{}\":\"unset\"}}", id)
                    }
                    &UpdateAction::Enabled(en) => format!("\"enabled\": {}", en),
                };

                let payload = format!(
                    "{{\"state\":{{\"desired\":{{{}}}, \"reported\":{}}}}}",
                    desired_known_networks,
                    serde_json_core::to_string::<_, 512>(self.context().shadow.get()).unwrap()
                );

                log::debug!("Update from cloud: {:?}", payload);

                mqtt_client
                    .publish(Publish {
                        dup: false,
                        qos: QoS::AtLeastOnce,
                        retain: false,
                        pid: None,
                        topic_name: &Topic::Update
                            .format::<128>(
                                mqtt_client.client_id(),
                                <WifiConfig as ShadowState>::NAME,
                            )
                            .unwrap(),
                        payload: payload.as_bytes(),
                        properties: Properties::Slice(&[]),
                    })
                    .unwrap();
                self.process_event(Events::Ack).unwrap();
            }
            (&States::AckUpdate, Notification::Publish(publish))
                if matches!(
                    publish.topic_name.as_str(),
                    "$aws/things/rustot-test/shadow/name/wifi/update/delta"
                ) =>
            {
                self.context_mut()
                    .shadow
                    .handle_message(&publish.topic_name, &publish.payload)
                    .unwrap();

                self.process_event(Events::CheckState).unwrap();
            }
            (&States::Check(ref expected_map), Notification::Publish(publish))
                if matches!(
                    publish.topic_name.as_str(),
                    "$aws/things/rustot-test/shadow/name/wifi/update/accepted"
                        | "$aws/things/rustot-test/shadow/name/wifi/update/delta"
                ) =>
            {
                let expected = expected_map.clone();
                self.context_mut()
                    .shadow
                    .handle_message(&publish.topic_name, &publish.payload)
                    .unwrap();

                match expected {
                    Some(expected_map) => {
                        assert_eq!(self.context().shadow.get().known_networks, expected_map);
                        self.context_mut().shadow.get_shadow().unwrap();
                        let event = if self.context().update_cnt % 2 == 0 {
                            Events::UpdateStateFromDevice
                        } else {
                            Events::UpdateStateFromCloud
                        };
                        self.process_event(event).unwrap();
                    }
                    None => return true,
                }
            }
            (_, Notification::Publish(publish)) => {
                log::warn!("TOPIC: {}", publish.topic_name);
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

#[tokio::test(flavor = "current_thread")]
async fn test_shadows() {
    env_logger::init();

    log::info!("Starting shadows test...");

    let (thing_name, identity) = credentials::identity();

    let hostname = credentials::HOSTNAME.unwrap();

    static NETWORK: StaticCell<TlsNetwork> = StaticCell::new();
    let network = NETWORK.init(TlsNetwork::new(hostname.to_owned(), identity));

    // Create the MQTT stack
    let broker =
        DomainBroker::<_, 128>::new(format!("{}:8883", hostname).as_str(), &network).unwrap();
    let config = embedded_mqtt::Config::new(thing_name, broker)
        .keepalive_interval(embassy_time::Duration::from_secs(50));

    let mut state = State::<NoopRawMutex, 4096, { 4096 * 10 }, 4>::new();
    let (mut stack, client) = embedded_mqtt::new(&mut state, config);

    let mqtt_client = client;

    let shadow = Shadow::new(WifiConfig::default(), &mqtt_client).unwrap();

    // loop {
    //     if nb::block!(mqtt_eventloop.connect(&mut network)).expect("to connect to mqtt") {
    //         log::info!("Successfully connected to broker");
    //     }

    //     match mqtt_eventloop.yield_event(&mut network) {
    //         Ok(notification) => {
    //             if test_state.spin(notification, &mqtt_client) {
    //                 break;
    //             }
    //         }
    //         Err(_) => {}
    //     }
    // }

    // cloud_updater(mqtt_client);

    let shadows_fut = async {
        shadow.next_update().await;
        todo!()
    };

    let mut transport = NalTransport::new(network);

    match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(25),
        select::select(stack.run(transport), shadows_fut),
    )
    .await
    .unwrap()
    {
        select::Either::First(_) => {
            unreachable!()
        }
        select::Either::Second(result) => result.unwrap(),
    };
}
