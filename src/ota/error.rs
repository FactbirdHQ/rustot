use crate::jobs::{data_types::ErrorCode, JobError};

use super::pal::OtaPalError;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum OtaError {
    NoActiveJob,
    Momentum,
    MomentumAbort,
    InvalidInterface,
    ResetFailed,
    BlockOutOfRange,
    ZeroFileSize,
    Overflow,
    DataStreamEnded,
    UnexpectedTopic,
    InvalidFile,
    UpdateRejected(ErrorCode),
    Write(
        #[cfg_attr(feature = "defmt", defmt(Debug2Format))]
        embedded_storage_async::nor_flash::NorFlashErrorKind,
    ),
    Mqtt(mqttrust::Error),
    Encoding,
    Pal,
    Timeout,
}

impl OtaError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Encoding | Self::Timeout)
    }
}

impl From<mqttrust::Error> for OtaError {
    fn from(e: mqttrust::Error) -> Self {
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
