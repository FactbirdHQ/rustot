use crate::jobs::{JobError, data_types::ErrorCode};

use super::pal::PalError;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TransferError {
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
    #[cfg(feature = "transfer_http")]
    Http,
    Encoding,
    Pal(PalError),
    Timeout,
    UserAbort,
}

impl TransferError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Encoding | Self::Timeout | Self::Momentum)
    }
}

impl From<PalError> for TransferError {
    fn from(e: PalError) -> Self {
        Self::Pal(e)
    }
}

impl From<JobError> for TransferError {
    fn from(e: JobError) -> Self {
        match e {
            JobError::Overflow => Self::Overflow,
            JobError::Encoding => Self::Encoding,
            JobError::Mqtt => Self::Mqtt,
            JobError::Timeout => Self::Timeout,
        }
    }
}
