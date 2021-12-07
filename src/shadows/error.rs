use core::convert::TryFrom;
use core::fmt::Display;
use core::str::FromStr;

use heapless::String;
use mqttrust::MqttError;

use super::data_types::ErrorResponse;

#[derive(Debug, Clone)]
pub enum Error {
    Overflow,
    InvalidPayload,
    WrongShadowName,
    Mqtt(MqttError),
    ShadowError(ShadowError),
}

impl From<MqttError> for Error {
    fn from(e: MqttError) -> Self {
        Self::Mqtt(e)
    }
}

impl From<ShadowError> for Error {
    fn from(e: ShadowError) -> Self {
        Self::ShadowError(e)
    }
}

#[derive(Debug, Clone)]
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
    NoNamedShadow(String<64>),
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
            ShadowError::NotFound | ShadowError::NoNamedShadow(_) => 404,
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
            400 | 404 => Self::from_str(e.message)?,
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

impl Display for ShadowError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidJson => write!(f, "Invalid JSON"),
            Self::MissingState => write!(f, "Missing required node: state"),
            Self::MalformedState => write!(f, "State node must be an object"),
            Self::MalformedDesired => write!(f, "Desired node must be an object"),
            Self::MalformedReported => write!(f, "Reported node must be an object"),
            Self::InvalidVersion => write!(f, "Invalid version"),
            Self::InvalidClientToken => write!(f, "Invalid clientToken"),
            Self::JsonTooDeep => {
                write!(f, "JSON contains too many levels of nesting; maximum is 6")
            }
            Self::InvalidStateNode => write!(f, "State contains an invalid node"),
            Self::Unauthorized => write!(f, "Unauthorized"),
            Self::Forbidden => write!(f, "Forbidden"),
            Self::NotFound => write!(f, "Thing not found"),
            Self::NoNamedShadow(shadow_name) => {
                write!(f, "No shadow exists with name: {}", shadow_name)
            }
            Self::VersionConflict => write!(f, "Version conflict"),
            Self::PayloadTooLarge => write!(f, "The payload exceeds the maximum size allowed"),
            Self::UnsupportedEncoding => write!(
                f,
                "Unsupported documented encoding; supported encoding is UTF-8"
            ),
            Self::TooManyRequests => write!(f, "The Device Shadow service will generate this error message when there are more than 10 in-flight requests on a single connection"),
            Self::InternalServerError => write!(f, "Internal service failure"),
        }
    }
}

// TODO: This seems like an extremely brittle way of doing this??!
impl FromStr for ShadowError {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim() {
            "Invalid JSON" => Self::InvalidJson,
            "Missing required node: state" => Self::MissingState,
            "State node must be an object" => Self::MalformedState,
            "Desired node must be an object" => Self::MalformedDesired,
            "Reported node must be an object" => Self::MalformedReported,
            "Invalid version" => Self::InvalidVersion,
            "Invalid clientToken" => Self::InvalidClientToken,
            "JSON contains too many levels of nesting; maximum is 6" => Self::JsonTooDeep,
            "State contains an invalid node" => Self::InvalidStateNode,
            "Unauthorized" => Self::Unauthorized,
            "Forbidden" => Self::Forbidden,
            "Thing not found" => Self::NotFound,
            // TODO:
            "No shadow exists with name: " => Self::NoNamedShadow(String::new()),
            "Version conflict" => Self::VersionConflict,
            "The payload exceeds the maximum size allowed" => Self::PayloadTooLarge,
            "Unsupported documented encoding; supported encoding is UTF-8" => {
                Self::UnsupportedEncoding
            }
            "The Device Shadow service will generate this error message when there are more than 10 in-flight requests on a single connection" => Self::TooManyRequests,
            "Internal service failure" => Self::InternalServerError,
            _ => return Err(()),
        })
    }
}
