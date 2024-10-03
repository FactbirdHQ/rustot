use crate::jobs::data_types::JobStatus;

use super::{
    config::Config,
    encoding::{json::JobStatusReason, FileContext},
    error::OtaError,
    ProgressState,
};

pub mod mqtt;

// Interfaces required for OTA
pub trait ControlInterface {
    async fn request_job(&self) -> Result<(), OtaError>;
    async fn update_job_status(
        &self,
        file_ctx: &FileContext,
        progress: &mut ProgressState,
        config: &Config,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), OtaError>;
}
