use crate::jobs::data_types::JobStatus;
use crate::transfer::status_details::StatusDetailsExt;

use super::{
    ProgressState,
    encoding::{JobContext, json::JobStatusReason},
    error::TransferError,
};

pub mod mqtt;

// Interfaces required for OTA
pub trait ControlInterface {
    async fn request_job(&self) -> Result<(), TransferError>;

    /// Publish a job execution update.
    ///
    /// `progress.status` is already filled in for `status` and `reason`;
    /// implementations only transport it.
    async fn update_job_status<E: StatusDetailsExt, const N: usize>(
        &self,
        job: &JobContext<'_, E, N>,
        progress: &ProgressState<E, N>,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), TransferError>;
}
