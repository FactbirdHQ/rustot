#[allow(dead_code)]
pub mod aws_ota;
pub mod credentials;
pub mod file_handler;
pub mod network;

use rustot::{
    jobs::data_types::JobExecution,
    ota::{
        encoding::{json::OtaJob, OtaJobContext},
        error::OtaError,
        JobEventData, StatusDetailsExt,
    },
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub enum OtaJobs<'a> {
    #[serde(rename = "afr_ota")]
    #[serde(borrow)]
    Ota(OtaJob<'a>),
}

/// Convert a parsed [`JobExecution`] into an [`OtaJobContext`].
///
/// Extracts the OTA document from the job execution and constructs the
/// context needed by [`Updater::perform_ota`].
#[allow(dead_code)]
pub fn ota_context_from_execution<'a, E: StatusDetailsExt>(
    execution: JobExecution<'a, OtaJobs<'a>>,
    extra_status: E,
) -> Result<OtaJobContext<'a, E>, OtaError> {
    let ota_doc = match execution.job_document {
        Some(OtaJobs::Ota(doc)) => doc,
        None => return Err(OtaError::NoActiveJob),
    };

    OtaJobContext::new_from(
        JobEventData {
            job_name: execution.job_id,
            ota_document: ota_doc,
            status_details: execution.status_details,
        },
        0,
        extra_status,
    )
}
