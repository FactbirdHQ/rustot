#[derive(Debug)]
pub enum Error {
    Overflow,
    InvalidPayload,
    InvalidState,
    Mqtt(mqttrust::MqttError),
    DeserializeJson(serde_json_core::de::Error),
    DeserializeCbor,
    Response(u16),
}

impl From<mqttrust::MqttError> for Error {
    fn from(e: mqttrust::MqttError) -> Self {
        Self::Mqtt(e)
    }
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

impl From<minicbor_serde::error::DecodeError> for Error {
    fn from(_e: minicbor_serde::error::DecodeError) -> Self {
        Self::DeserializeCbor
    }
}

impl<E> From<minicbor_serde::error::EncodeError<E>> for Error {
    fn from(_: minicbor_serde::error::EncodeError<E>) -> Self {
        Self::Overflow
    }
}
