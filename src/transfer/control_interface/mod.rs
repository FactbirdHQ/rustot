use crate::jobs::data_types::JobStatus;
use crate::transfer::status_details::StatusDetailsExt;

use super::{
    encoding::{json::JobStatusReason, JobContext},
    error::TransferError,
    ProgressState,
};

pub mod mqtt;

// Interfaces required for OTA
pub trait ControlInterface {
    async fn request_job(&self) -> Result<(), TransferError>;
    async fn update_job_status<E: StatusDetailsExt>(
        &self,
        job: &JobContext<'_, E>,
        progress: &mut ProgressState<E>,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), TransferError>;
}
