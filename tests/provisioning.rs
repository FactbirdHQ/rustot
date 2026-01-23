#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

mod common;

use common::credentials;
use common::network::TlsNetwork;
use ecdsa::Signature;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use mqttrust::{transport::embedded_nal::NalTransport, Config, DomainBroker, State};
use p256::{ecdsa::signature::Signer, NistP256};
use rustot::provisioning::{CredentialHandler, Credentials, Error, FleetProvisioner};
use serde::{Deserialize, Serialize};
use static_cell::StaticCell;

pub struct OwnedCredentials {
    pub certificate_id: String,
    pub certificate_pem: String,
    pub private_key: Option<String>,
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

pub struct CredentialDAO {
    pub creds: Option<OwnedCredentials>,
}

impl CredentialHandler for CredentialDAO {
    async fn store_credentials(&mut self, credentials: Credentials<'_>) -> Result<(), Error> {
        log::info!("Provisioned credentials: {:#?}", credentials);

        self.creds.replace(credentials.into());

        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct Parameters<'a> {
    uuid: &'a str,
    signature: &'a str,
}

#[derive(Debug, Deserialize, PartialEq)]
struct DeviceConfig {
    #[serde(rename = "SoftwareId")]
    software_id: heapless::String<64>,
}

#[tokio::test(flavor = "current_thread")]
async fn test_provisioning() {
    env_logger::init();

    log::info!("Starting provisioning test...");

    let (thing_name, claim_identity) = credentials::claim_identity();

    // Connect to AWS IoT Core with provisioning claim credentials
    let hostname = credentials::HOSTNAME.unwrap();
    let template_name =
        std::env::var("TEMPLATE_NAME").unwrap_or_else(|_| "duoProvisioningTemplate".to_string());

    static NETWORK: StaticCell<TlsNetwork> = StaticCell::new();
    let network = NETWORK.init(TlsNetwork::new(hostname.to_owned(), claim_identity));

    // Create the MQTT stack
    let broker = DomainBroker::<_, 128>::new_with_port(hostname, 8883, network).unwrap();
    let config = Config::builder()
        .client_id(thing_name.try_into().unwrap())
        .keepalive_interval(embassy_time::Duration::from_secs(50))
        .build();

    static STATE: StaticCell<State<NoopRawMutex, 2048, 4096>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = mqttrust::new(state, config);

    let signing_key = credentials::signing_key();
    let signature: Signature<NistP256> = signing_key.sign(thing_name.as_bytes());
    let hex_signature: String = hex::encode(signature.to_bytes());

    let parameters = Parameters {
        uuid: thing_name,
        signature: &hex_signature,
    };

    let mut credential_handler = CredentialDAO { creds: None };

    #[cfg(not(feature = "provision_cbor"))]
    let provision_fut = FleetProvisioner::provision::<DeviceConfig, _>(
        &client,
        &template_name,
        Some(parameters),
        &mut credential_handler,
    );
    #[cfg(feature = "provision_cbor")]
    let provision_fut = FleetProvisioner::provision_cbor::<DeviceConfig, _>(
        &client,
        &template_name,
        Some(parameters),
        &mut credential_handler,
    );

    let mut transport = NalTransport::new(network, broker);

    let device_config = match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(15),
        select::select(stack.run(&mut transport), provision_fut),
    )
    .await
    .unwrap()
    {
        select::Either::First(_) => {
            unreachable!()
        }
        select::Either::Second(result) => result.unwrap(),
    };
    assert_eq!(
        device_config,
        Some(DeviceConfig {
            software_id: heapless::String::try_from("82b3509e0e924e06ab1bdb1cf1625dcb").unwrap()
        })
    );
    assert!(!credential_handler.creds.unwrap().certificate_id.is_empty());
}
