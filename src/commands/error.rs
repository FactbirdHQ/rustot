#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum CommandError {
    Overflow,
    InvalidPayload,
    Mqtt,
    Timeout,
    Encoding,
    DeserializeJson(#[cfg_attr(feature = "defmt", defmt(Debug2Format))] serde_json_core::de::Error),
    #[cfg(feature = "commands_cbor")]
    DeserializeCbor,
    Rejected(u16),
}

impl From<serde_json_core::ser::Error> for CommandError {
    fn from(_: serde_json_core::ser::Error) -> Self {
        Self::Overflow
    }
}

impl From<serde_json_core::de::Error> for CommandError {
    fn from(e: serde_json_core::de::Error) -> Self {
        Self::DeserializeJson(e)
    }
}

#[cfg(feature = "commands_cbor")]
impl From<minicbor_serde::error::DecodeError> for CommandError {
    fn from(_: minicbor_serde::error::DecodeError) -> Self {
        Self::DeserializeCbor
    }
}
