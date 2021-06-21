use crate::jobs::data_types::JobStatus;

use super::{
    config::Config,
    encoding::{json::JobStatusReason, FileContext},
};

pub mod mqtt;

// Interfaces required for OTA
pub trait ControlInterface {
    fn request_job(&self) -> Result<(), ()>;
    fn update_job_status(
        &self,
        file_ctx: &mut FileContext,
        config: &Config,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), ()>;
    fn cleanup(&self) -> Result<(), ()>;
}
