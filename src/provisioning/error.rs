#[derive(Debug)]
pub enum Error {
    Overflow,
    InvalidPayload,
    InvalidState,
    Mqtt,
    DeserializeJson(serde_json_core::de::Error),
    DeserializeCbor,
    CertificateStorage,
    Response(u16),
}

impl From<serde_json_core::ser::Error> for Error {
    fn from(_: serde_json_core::ser::Error) -> Self {
        Self::Overflow
    }
}

impl From<serde_json_core::de::Error> for Error {
    fn from(e: serde_json_core::de::Error) -> Self {
        Self::DeserializeJson(e)
    }
}

impl From<serde_cbor::Error> for Error {
    fn from(_e: serde_cbor::Error) -> Self {
        Self::DeserializeCbor
    }
}
