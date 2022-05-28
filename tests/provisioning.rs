mod common;

use mqttrust::Mqtt;
use mqttrust_core::{bbqueue::BBBuffer, EventLoop, MqttOptions, Notification, PublishNotification};

use common::clock::SysClock;
use common::network::{Network, TcpSocket};
use native_tls::{Identity, TlsConnector, TlsStream};
use p256::ecdsa::signature::Signer;
use rustot::provisioning::{topics::Topic, Credentials, FleetProvisioner, Response};
use std::net::TcpStream;
use std::ops::DerefMut;

use common::credentials;

static mut Q: BBBuffer<{ 1024 * 10 }> = BBBuffer::new();

pub struct OwnedCredentials {
    certificate_id: String,
    certificate_pem: String,
    private_key: Option<String>,
}

impl<'a> From<Credentials<'a>> for OwnedCredentials {
    fn from(c: Credentials<'a>) -> Self {
        Self {
            certificate_id: c.certificate_id.to_string(),
            certificate_pem: c.certificate_pem.to_string(),
            private_key: c.private_key.map(ToString::to_string),
        }
    }
}

fn provision_credentials<'a, const L: usize>(
    hostname: &'a str,
    identity: Identity,
    mqtt_eventloop: &mut EventLoop<'a, 'a, TcpSocket<TlsStream<TcpStream>>, SysClock, 1000, L>,
    mqtt_client: &mqttrust_core::Client<L>,
) -> Result<OwnedCredentials, ()> {
    let connector = TlsConnector::builder()
        .identity(identity)
        .add_root_certificate(credentials::root_ca())
        .build()
        .unwrap();

    let mut network = Network::new_tls(connector, String::from(hostname));

    nb::block!(mqtt_eventloop.connect(&mut network))
        .expect("To connect to MQTT with claim credentials");

    log::info!("Successfully connected to broker with claim credentials");

    #[cfg(feature = "cbor")]
    let mut provisioner = FleetProvisioner::new(mqtt_client, "duoProvisioningTemplate");
    #[cfg(not(feature = "cbor"))]
    let mut provisioner = FleetProvisioner::new_json(mqtt_client, "duoProvisioningTemplate");

    provisioner
        .initialize()
        .expect("Failed to initialize FleetProvisioner");

    let mut provisioned_credentials: Option<OwnedCredentials> = None;

    let signing_key = credentials::signing_key();
    let signature = hex::encode(signing_key.sign(mqtt_client.client_id().as_bytes()));

    let result = loop {
        match mqtt_eventloop.yield_event(&mut network) {
            Ok(Notification::Publish(mut publish)) if Topic::check(publish.topic_name.as_str()) => {
                let PublishNotification {
                    topic_name,
                    payload,
                    ..
                } = publish.deref_mut();

                match provisioner.handle_message::<4>(topic_name.as_str(), payload) {
                    Ok(Response::Credentials(credentials)) => {
                        log::info!("Got credentials! {:?}", credentials);
                        provisioned_credentials = Some(credentials.into());

                        let mut parameters = heapless::IndexMap::new();
                        parameters.insert("uuid", mqtt_client.client_id()).unwrap();
                        parameters.insert("signature", &signature).unwrap();

                        provisioner
                            .register_thing::<2>(Some(parameters))
                            .expect("To successfully publish to RegisterThing");
                    }
                    Ok(Response::DeviceConfiguration(config)) => {
                        // Store Device configuration parameters, if any.

                        log::info!("Got device config! {:?}", config);

                        break Ok(());
                    }
                    Ok(Response::None) => {}
                    Err(e) => {
                        log::error!("Got provision error! {:?}", e);
                        provisioned_credentials = None;

                        break Err(());
                    }
                }
            }
            Ok(Notification::Suback(_)) => {
                log::info!("Starting provisioning");
                provisioner.begin().expect("To begin provisioning");
            }
            Ok(n) => {
                log::trace!("{:?}", n);
            }
            _ => {}
        }
    };

    // Disconnect from AWS IoT Core
    mqtt_eventloop.disconnect(&mut network);

    result.and_then(|_| provisioned_credentials.ok_or(()))
}

#[test]
fn test_provisioning() {
    env_logger::init();

    let (p, c) = unsafe { Q.try_split_framed().unwrap() };

    log::info!("Starting provisioning test...");

    let (thing_name, claim_identity) = credentials::claim_identity();

    // Connect to AWS IoT Core with provisioning claim credentials
    let hostname = credentials::HOSTNAME.unwrap();

    let mut mqtt_eventloop = EventLoop::new(
        c,
        SysClock::new(),
        MqttOptions::new(thing_name, hostname.into(), 8883).set_clean_session(true),
    );

    let mqtt_client = mqttrust_core::Client::new(p, thing_name);

    let credentials =
        provision_credentials(hostname, claim_identity, &mut mqtt_eventloop, &mqtt_client).unwrap();

    assert!(credentials.certificate_id.len() > 0);
}
