use crate::jobs::JobError;

use super::pal::OtaPalError;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum OtaError {
    NoActiveJob,
    SignalEventFailed,
    Momentum,
    MomentumAbort,
    InvalidInterface,
    ResetFailed,
    BlockOutOfRange,
    ZeroFileSize,
    Overflow,
    InvalidFile,
    Mqtt(embedded_mqtt::Error),
    Encoding,
    Pal,
    Timeout,
}

impl OtaError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Encoding)
    }
}

impl From<embedded_mqtt::Error> for OtaError {
    fn from(e: embedded_mqtt::Error) -> Self {
        Self::Mqtt(e)
    }
}

impl From<OtaPalError> for OtaError {
    fn from(_e: OtaPalError) -> Self {
        Self::Pal
    }
}

impl From<JobError> for OtaError {
    fn from(e: JobError) -> Self {
        match e {
            JobError::Overflow => Self::Overflow,
            JobError::Encoding => Self::Encoding,
            JobError::Mqtt(e) => Self::Mqtt(e),
        }
    }
}
