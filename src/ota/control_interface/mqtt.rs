use core::fmt::Write;

use super::ControlInterface;
use crate::jobs::data_types::JobStatus;
use crate::jobs::{JobTopic, Jobs, MAX_JOB_ID_LEN, MAX_THING_NAME_LEN};
use crate::mqtt::{Mqtt, MqttClient, PublishOptions, QoS};
use crate::ota::encoding::json::JobStatusReason;
use crate::ota::encoding::FileContext;
use crate::ota::error::OtaError;
use crate::ota::ProgressState;

impl<C: MqttClient> ControlInterface for Mqtt<&'_ C> {
    /// Check for next available OTA job from the job service by publishing a
    /// "get next job" message to the job service.
    async fn request_job(&self) -> Result<(), OtaError> {
        let describe = Jobs::describe();
        let topic = describe.topic(self.0.client_id())?;

        self.0
            .publish(&topic, describe)
            .await
            .map_err(|_| OtaError::Mqtt)
    }

    /// Update the job status on the service side.
    async fn update_job_status(
        &self,
        file_ctx: &FileContext,
        progress_state: &mut ProgressState,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), OtaError> {
        progress_state
            .status_details
            .insert(
                heapless::String::try_from("self_test").unwrap(),
                heapless::String::try_from(reason.as_str()).unwrap(),
            )
            .map_err(|_| OtaError::Overflow)?;

        if let JobStatus::InProgress | JobStatus::Succeeded = status {
            let received_blocks = progress_state.total_blocks - progress_state.blocks_remaining;

            // Don't override the progress on succeeded, nor on self-test
            // active. (Cases where progress counter is lost due to device
            // restarts)
            if status != JobStatus::Succeeded && reason != JobStatusReason::SelfTestActive {
                let mut progress = heapless::String::new();
                progress
                    .write_fmt(format_args!(
                        "{}/{}",
                        received_blocks, progress_state.total_blocks
                    ))
                    .map_err(|_| OtaError::Overflow)?;

                progress_state
                    .status_details
                    .insert(heapless::String::try_from("progress").unwrap(), progress)
                    .map_err(|_| OtaError::Overflow)?;
            }
        }

        // Downgrade progress updates to QoS 0 to avoid overloading MQTT
        // buffers during active streaming. First and last updates remain QoS 1.
        let qos = if status == JobStatus::InProgress
            && progress_state.blocks_remaining != 0
            && progress_state.total_blocks != progress_state.blocks_remaining
        {
            QoS::AtMostOnce
        } else {
            QoS::AtLeastOnce
        };

        let topic = JobTopic::Update(file_ctx.job_name.as_str())
            .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>(self.0.client_id())?;

        let payload = Jobs::update(status)
            .client_token(self.0.client_id())
            .status_details(&progress_state.status_details);

        debug!("Updating job status! {:?}", status);

        self.0
            .publish_with_options(&topic, payload, PublishOptions::new().qos(qos))
            .await
            .map_err(|_| OtaError::Mqtt)?;

        Ok(())
    }
}
