use std::env;

use native_tls::{Certificate, Identity};
use p256::ecdsa::SigningKey;
use pkcs8::DecodePrivateKey;

pub fn identity() -> (&'static str, Identity) {
    let thing_name = option_env!("THING_NAME").unwrap_or_else(|| "rustot-test");
    let pw = env::var("IDENTITY_PASSWORD").unwrap_or_default();
    (
        thing_name,
        Identity::from_pkcs12(include_bytes!("../secrets/identity.pfx"), pw.as_str()).unwrap(),
    )
}

pub fn claim_identity() -> (&'static str, Identity) {
    let thing_name = option_env!("THING_NAME").unwrap_or_else(|| "rustot-provision");
    let pw = env::var("IDENTITY_PASSWORD").unwrap_or_default();
    (
        thing_name,
        Identity::from_pkcs12(include_bytes!("../secrets/claim_identity.pfx"), pw.as_str())
            .unwrap(),
    )
}

pub fn root_ca() -> Certificate {
    Certificate::from_pem(include_bytes!("../secrets/root-ca.pem")).unwrap()
}

pub fn signing_key() -> SigningKey {
    let pw = env::var("IDENTITY_PASSWORD").unwrap_or_default();
    SigningKey::from_pkcs8_encrypted_pem(include_str!("../secrets/sign_private.pem"), pw).unwrap()
}

pub const HOSTNAME: Option<&'static str> = option_env!("AWS_HOSTNAME");
