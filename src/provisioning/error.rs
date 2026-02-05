#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    Overflow,
    InvalidPayload,
    InvalidState,
    Mqtt,
    DeserializeJson(#[cfg_attr(feature = "defmt", defmt(Debug2Format))] serde_json_core::de::Error),
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

#[cfg(feature = "provision_cbor")]
impl From<minicbor_serde::error::DecodeError> for Error {
    fn from(_e: minicbor_serde::error::DecodeError) -> Self {
        Self::DeserializeCbor
    }
}
