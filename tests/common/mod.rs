#[allow(dead_code)]
pub mod aws_commands;
#[allow(dead_code)]
pub mod aws_ota;
pub mod credentials;
pub mod file_handler;
pub mod network;

use rustot::{
    jobs::data_types::JobExecution,
    transfer::{
        StatusDetailsExt,
        encoding::{JobContext, afr_ota::OtaJob},
        error::TransferError,
    },
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub enum OtaJobs<'a, const N: usize = 1> {
    #[serde(rename = "afr_ota")]
    #[serde(borrow)]
    Ota(OtaJob<'a, N>),
}

/// Convert a parsed [`JobExecution`] into a [`JobContext`].
///
/// Extracts the OTA document from the job execution and constructs the
/// context needed by [`Transfer::perform_ota`].
#[allow(dead_code)]
pub fn ota_context_from_execution<'a, E: StatusDetailsExt, const N: usize>(
    execution: JobExecution<'a, OtaJobs<'a, N>>,
) -> Result<JobContext<'a, E, N>, TransferError> {
    let ota_doc = match execution.job_document {
        Some(OtaJobs::Ota(doc)) => doc,
        None => return Err(TransferError::NoActiveJob),
    };

    JobContext::new(
        &ota_doc,
        execution.job_id,
        execution.status_details.as_ref(),
    )
}
