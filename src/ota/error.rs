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
    WriteFailed,
    Mqtt,
    #[cfg(feature = "ota_http_data")]
    Http,
    Encoding,
    Pal(OtaPalError),
    Timeout,
    UserAbort,
}

impl OtaError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Encoding | Self::Timeout | Self::Momentum)
    }
}

impl From<OtaPalError> for OtaError {
    fn from(e: OtaPalError) -> Self {
        Self::Pal(e)
    }
}

impl From<JobError> for OtaError {
    fn from(e: JobError) -> Self {
        match e {
            JobError::Overflow => Self::Overflow,
            JobError::Encoding => Self::Encoding,
            JobError::Mqtt => Self::Mqtt,
            JobError::Timeout => Self::Timeout,
        }
    }
}
