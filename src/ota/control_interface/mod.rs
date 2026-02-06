use crate::jobs::data_types::JobStatus;
use crate::ota::status_details::StatusDetailsExt;

use super::{
    encoding::{json::JobStatusReason, FileContext},
    error::OtaError,
    ProgressState,
};

pub mod mqtt;

// Interfaces required for OTA
pub trait ControlInterface {
    async fn request_job(&self) -> Result<(), OtaError>;
    async fn update_job_status<E: StatusDetailsExt>(
        &self,
        file_ctx: &FileContext,
        progress: &mut ProgressState<E>,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), OtaError>;
}
