use crate::jobs::data_types::JobStatus;

use super::{
    config::Config,
    encoding::{json::JobStatusReason, FileContext},
    error::OtaError,
};

pub mod mqtt;

// Interfaces required for OTA
pub trait ControlInterface {
    fn request_job(&self) -> Result<(), OtaError>;
    fn update_job_status(
        &self,
        file_ctx: &mut FileContext,
        config: &Config,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), OtaError>;
    fn cleanup(&self) -> Result<(), OtaError>;
}
