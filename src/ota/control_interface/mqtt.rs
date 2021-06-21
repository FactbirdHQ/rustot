use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};

use mqttrust::QoS;

use super::ControlInterface;
use crate::jobs::data_types::JobStatus;
use crate::jobs::data_types::MAX_CLIENT_TOKEN_LEN;
use crate::jobs::Jobs;
use crate::jobs::Topics;
use crate::ota::config::Config;
use crate::ota::encoding::json::JobStatusReason;
use crate::ota::encoding::FileContext;

// FIXME: This can cause unit-tests to sometimes fail, due to parallel execution
static REQUEST_CNT: AtomicU32 = AtomicU32::new(0);

impl<T: mqttrust::Mqtt> ControlInterface for T {
    /// Check for next available OTA job from the job service by publishing a
    /// "get next job" message to the job service.
    fn request_job(&self) -> Result<(), ()> {
        // Subscribe to the OTA job notification topics
        Jobs::subscribe(self, Topics::NOTIFY_NEXT, None)?;

        let request_cnt = REQUEST_CNT.fetch_add(1, Ordering::Relaxed);

        // Obtains a unique client token on the form
        // `{requestNumber}:{thingName}`, and increments the request counter
        let mut client_token = heapless::String::<MAX_CLIENT_TOKEN_LEN>::new();
        client_token
            .write_fmt(format_args!("{}:{}", request_cnt, self.client_id()))
            .map_err(drop)?;

        Jobs::describe_next(self, Some(client_token.as_str()))?;

        Ok(())
    }

    /// Update the job status on the service side with progress or completion info
    fn update_job_status(
        &self,
        file_ctx: &mut FileContext,
        config: &Config,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), ()> {
        file_ctx
            .status_details
            .insert(
                heapless::String::from("self_test"),
                serde_json_core::to_string(&reason).map_err(drop)?,
            )
            .map_err(drop)?;

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
                .map_err(drop)?;

            file_ctx
                .status_details
                .insert(heapless::String::from("progress"), progress)
                .map_err(drop)?;

            // Downgrade Progress updates to QOS 0 to avoid overloading MQTT
            // buffers during active streaming
            qos = QoS::AtMostOnce;
        }

        Jobs::update(
            self,
            file_ctx.stream_name.as_str(),
            status,
            Some(&file_ctx.status_details),
            qos,
        )?;

        Ok(())
    }

    /// Perform any cleanup operations required for control plane
    fn cleanup(&self) -> Result<(), ()> {
        Jobs::unsubscribe(self, Topics::NOTIFY_NEXT, None)?;
        Ok(())
    }
}
