use core::convert::TryFrom;

use super::data_types::ErrorResponse;

#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    Overflow,
    NoPersistence,
    DaoRead,
    DaoWrite,
    InvalidPayload,
    WrongShadowName,
    MqttError(mqttrust::Error),
    ShadowError(ShadowError),
}

impl From<ShadowError> for Error {
    fn from(e: ShadowError) -> Self {
        Self::ShadowError(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ShadowError {
    InvalidJson,
    MissingState,
    MalformedState,
    MalformedDesired,
    MalformedReported,
    InvalidVersion,
    InvalidClientToken,
    JsonTooDeep,
    InvalidStateNode,
    Unauthorized,
    Forbidden,
    NotFound,
    VersionConflict,
    PayloadTooLarge,
    UnsupportedEncoding,
    TooManyRequests,
    InternalServerError,
}

impl ShadowError {
    pub fn http_code(&self) -> u16 {
        match self {
            ShadowError::InvalidJson
            | ShadowError::MissingState
            | ShadowError::MalformedState
            | ShadowError::MalformedDesired
            | ShadowError::MalformedReported
            | ShadowError::InvalidVersion
            | ShadowError::InvalidClientToken
            | ShadowError::JsonTooDeep
            | ShadowError::InvalidStateNode => 400,

            ShadowError::Unauthorized => 401,
            ShadowError::Forbidden => 403,
            ShadowError::NotFound => 404,
            ShadowError::VersionConflict => 409,
            ShadowError::PayloadTooLarge => 413,
            ShadowError::UnsupportedEncoding => 415,
            ShadowError::TooManyRequests => 429,
            ShadowError::InternalServerError => 500,
        }
    }
}

impl<'a> TryFrom<ErrorResponse<'a>> for ShadowError {
    type Error = ();

    fn try_from(e: ErrorResponse<'a>) -> Result<Self, Self::Error> {
        Ok(match e.code {
            400 | 404 => ShadowError::NotFound,
            401 => ShadowError::Unauthorized,
            403 => ShadowError::Forbidden,
            409 => ShadowError::VersionConflict,
            413 => ShadowError::PayloadTooLarge,
            415 => ShadowError::UnsupportedEncoding,
            429 => ShadowError::TooManyRequests,
            500 => ShadowError::InternalServerError,
            _ => return Err(()),
        })
    }
}
