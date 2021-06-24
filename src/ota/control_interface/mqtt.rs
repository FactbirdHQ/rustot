use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};

use mqttrust::QoS;

use super::ControlInterface;
use crate::jobs::data_types::JobStatus;
use crate::jobs::subscribe::Topic;
use crate::jobs::Jobs;
use crate::jobs::MAX_CLIENT_TOKEN_LEN;
use crate::ota::config::Config;
use crate::ota::encoding::json::JobStatusReason;
use crate::ota::encoding::FileContext;
use crate::ota::error::OtaError;
use crate::rustot_log;

// FIXME: This can cause unit-tests to sometimes fail, due to parallel execution
static REQUEST_CNT: AtomicU32 = AtomicU32::new(0);

impl<T: mqttrust::Mqtt> ControlInterface for T {
    /// Check for next available OTA job from the job service by publishing a
    /// "get next job" message to the job service.
    fn request_job(&self) -> Result<(), OtaError> {
        // Subscribe to the OTA job notification topics
        Jobs::subscribe()
            .topic(Topic::NotifyNext, QoS::AtLeastOnce)
            .send(self)?;

        let request_cnt = REQUEST_CNT.fetch_add(1, Ordering::Relaxed);

        // Obtains a unique client token on the form
        // `{requestNumber}:{thingName}`, and increments the request counter
        let mut client_token = heapless::String::<MAX_CLIENT_TOKEN_LEN>::new();
        client_token
            .write_fmt(format_args!("{}:{}", request_cnt, self.client_id())).map_err(|_|OtaError::Overflow)?;

        Jobs::describe()
            .client_token(client_token.as_str())
            .send(self, QoS::AtLeastOnce)?;

        Ok(())
    }

    /// Update the job status on the service side with progress or completion info
    fn update_job_status(
        &self,
        file_ctx: &mut FileContext,
        config: &Config,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), OtaError> {
        file_ctx
            .status_details
            .insert(
                heapless::String::from("self_test"),
                serde_json_core::to_string(&reason).map_err(|_| OtaError::Encoding)?,
            ).map_err(|_| OtaError::Overflow)?;

        let mut qos = QoS::AtLeastOnce;

        if let (JobStatus::InProgress, JobStatusReason::Receiving) = (status, reason) {
            let total_blocks = (file_ctx.filesize + config.block_size - 1) / config.block_size;
            let received_blocks = (total_blocks - file_ctx.blocks_remaining + 1) as u32;

            // Output a status update once in a while
            if (received_blocks - 1) % config.status_update_frequency as u32 != 0 {
                return Ok(());
            }

            let mut progress = heapless::String::new();
            progress
                .write_fmt(format_args!("{}/{}", received_blocks, total_blocks))
                .map_err(|_| OtaError::Overflow)?;

            rustot_log!(info, "Updating progress: {}", progress);

            file_ctx
                .status_details
                .insert(heapless::String::from("progress"), progress)
                .map_err(|_| OtaError::Overflow)?;

            // Downgrade Progress updates to QOS 0 to avoid overloading MQTT
            // buffers during active streaming
            qos = QoS::AtMostOnce;
        }

        Jobs::update(file_ctx.job_name.as_str(), status)
            .status_details(&file_ctx.status_details)
            .send(self, qos)?;

        Ok(())
    }

    /// Perform any cleanup operations required for control plane
    fn cleanup(&self) -> Result<(), OtaError> {
        Jobs::unsubscribe().topic(Topic::NotifyNext).send(self)?;
        Ok(())
    }
}
