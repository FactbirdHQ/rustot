use crate::jobs::JobError;

use super::pal::OtaPalError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    Mqtt(mqttrust::MqttError),
    Encoding,
    Pal,
    Timer,
}

impl OtaError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Encoding)
    }
}

impl From<mqttrust::MqttError> for OtaError {
    fn from(e: mqttrust::MqttError) -> Self {
        Self::Mqtt(e)
    }
}

impl<E> From<OtaPalError<E>> for OtaError {
    fn from(_e: OtaPalError<E>) -> Self {
        Self::Pal
    }
}

impl From<JobError> for OtaError {
    fn from(e: JobError) -> Self {
        match e {
            JobError::Overflow => Self::Overflow,
            JobError::Encoding => Self::Encoding,
            JobError::Mqtt(m) => Self::Mqtt(m),
        }
    }
}
