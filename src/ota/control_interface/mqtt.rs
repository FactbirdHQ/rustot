use super::ControlInterface;
use crate::jobs::data_types::JobStatus;
use crate::jobs::{JobTopic, Jobs, MAX_JOB_ID_LEN, MAX_THING_NAME_LEN};
use crate::mqtt::{Mqtt, MqttClient, PublishOptions, QoS};
use crate::ota::encoding::json::JobStatusReason;
use crate::ota::encoding::FileContext;
use crate::ota::error::OtaError;
use crate::ota::status_details::StatusDetailsExt;
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
    async fn update_job_status<E: StatusDetailsExt>(
        &self,
        file_ctx: &FileContext,
        progress_state: &mut ProgressState<E>,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), OtaError> {
        // Set the self_test status field
        progress_state.status_details.set_self_test(reason.as_str());

        // Add progress tracking for in-progress and failed updates
        if let JobStatus::InProgress | JobStatus::Succeeded | JobStatus::Failed = status {
            // Don't override the progress on succeeded, nor on self-test
            // active. (Cases where progress counter is lost due to device
            // restarts)
            if status != JobStatus::Succeeded && reason != JobStatusReason::SelfTestActive {
                let received_blocks = progress_state.total_blocks - progress_state.blocks_remaining;
                progress_state
                    .status_details
                    .set_progress(received_blocks, progress_state.total_blocks);
            }
        }

        // Add failure detail if present (for Rejected/Aborted reasons)
        if let Some(detail) = reason.detail() {
            progress_state.status_details.set_failure(detail);
        } else {
            // Clear any previous failure details for non-failure reasons
            progress_state.status_details.clear_failure();
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

        let combined = progress_state
            .status_details
            .with_extra(&progress_state.extra_status);
        let payload = Jobs::update(status)
            .client_token(self.0.client_id())
            .status_details(&combined);

        debug!("Updating job status! {:?}", status);

        self.0
            .publish_with_options(&topic, payload, PublishOptions::new().qos(qos))
            .await
            .map_err(|_| OtaError::Mqtt)?;

        Ok(())

        // loop {
        //     let message = match with_timeout(
        //         embassy_time::Duration::from_secs(1),
        //         sub.next_message(),
        //     )
        //     .await
        //     {
        //         Ok(res) => res.ok_or(JobError::Encoding)?,
        //         Err(_) => return Err(OtaError::Timeout),
        //     };

        //     // Check if topic is GetAccepted
        //     match crate::jobs::Topic::from_str(message.topic_name()) {
        //         Some(crate::jobs::Topic::UpdateAccepted(_)) => {
        //             // Check client token
        //             let (response, _) = serde_json_core::from_slice::<
        //                 UpdateJobExecutionResponse<encoding::json::OtaJob<'_>>,
        //             >(message.payload())
        //             .map_err(|_| JobError::Encoding)?;

        //             if response.client_token != Some(self.0.client_id()) {
        //                 error!(
        //                     "Unexpected client token received: {}, expected: {}",
        //                     response.client_token.unwrap_or("None"),
        //                     self.0.client_id()
        //                 );
        //                 continue;
        //             }

        //             return Ok(());
        //         }
        //         Some(crate::jobs::Topic::UpdateRejected(_)) => {
        //             let (error_response, _) =
        //                 serde_json_core::from_slice::<ErrorResponse>(message.payload())
        //                     .map_err(|_| JobError::Encoding)?;

        //             if error_response.client_token != Some(self.0.client_id()) {
        //                 error!(
        //                     "Unexpected client token received: {}, expected: {}",
        //                     error_response.client_token.unwrap_or("None"),
        //                     self.0.client_id()
        //                 );
        //                 continue;
        //             }

        //             error!("OTA Update rejected: {:?}", error_response.message);

        //             return Err(OtaError::UpdateRejected(error_response.code));
        //         }
        //         _ => {
        //             error!("Expected Topic name GetRejected or GetAccepted but got something else");
        //         }
        //     }
        // }
    }
}
