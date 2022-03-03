use std::env;

use native_tls::{Certificate, Identity};
use p256::ecdsa::SigningKey;
use pkcs8::DecodePrivateKey;

pub fn identity() -> (&'static str, Identity) {
    let pw = env::var("DEVICE_ADVISOR_PASSWORD").unwrap_or_default();
    (
        "rustot-test",
        Identity::from_pkcs12(include_bytes!("../secrets/identity.pfx"), pw.as_str()).unwrap(),
    )
}

pub fn claim_identity() -> (&'static str, Identity) {
    let pw = env::var("DEVICE_ADVISOR_PASSWORD").unwrap_or_default();
    (
        "rustot-provision",
        Identity::from_pkcs12(include_bytes!("../secrets/claim_identity.pfx"), pw.as_str())
            .unwrap(),
    )
}

pub fn root_ca() -> Certificate {
    Certificate::from_pem(include_bytes!("../secrets/root-ca.pem")).unwrap()
}

pub fn signing_key() -> SigningKey {
    let pw = env::var("DEVICE_ADVISOR_PASSWORD").unwrap_or_default();
    SigningKey::from_pkcs8_encrypted_pem(include_str!("../secrets/sign_private.pem"), pw).unwrap()
}

pub const HOSTNAME: Option<&'static str> = option_env!("AWS_HOSTNAME");
