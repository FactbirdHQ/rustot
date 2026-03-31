use super::ControlInterface;
use crate::jobs::data_types::{ErrorResponse, JobStatus};
use crate::jobs::{self, JobTopic, Jobs, MAX_JOB_ID_LEN, MAX_THING_NAME_LEN};
use crate::mqtt::{Mqtt, MqttClient, MqttMessage, MqttSubscription, PublishOptions, QoS};
use crate::transfer::encoding::json::JobStatusReason;
use crate::transfer::encoding::JobContext;
use crate::transfer::error::TransferError;
use crate::transfer::status_details::StatusDetailsExt;
use crate::transfer::ProgressState;

impl<C: MqttClient> ControlInterface for Mqtt<&'_ C> {
    /// Check for next available OTA job from the job service by publishing a
    /// "get next job" message to the job service.
    async fn request_job(&self) -> Result<(), TransferError> {
        let describe = Jobs::describe();
        let topic = describe.topic(self.0.client_id())?;

        self.0
            .publish(&topic, describe)
            .await
            .map_err(|_| TransferError::Mqtt)
    }

    /// Update the job status on the service side.
    async fn update_job_status<E: StatusDetailsExt>(
        &self,
        job: &JobContext<'_, E>,
        progress_state: &mut ProgressState<E>,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), TransferError> {
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
            progress_state
                .status_details
                .set_failure(detail.as_reason_str(), detail.error_code());
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

        let job_name = job.job_name;

        // For QoS 1 updates, subscribe to accepted/rejected before publishing
        // so we can verify the cloud processed the update. Critical for the
        // final Succeeded update before reboot.
        let sub = if qos == QoS::AtLeastOnce {
            let accepted_topic =
                JobTopic::UpdateAccepted(job_name)
                    .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>(self.0.client_id())?;
            let rejected_topic =
                JobTopic::UpdateRejected(job_name)
                    .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>(self.0.client_id())?;

            Some(
                self.0
                    .subscribe(&[
                        (accepted_topic.as_str(), QoS::AtMostOnce),
                        (rejected_topic.as_str(), QoS::AtMostOnce),
                    ])
                    .await
                    .map_err(|_| TransferError::Mqtt)?,
            )
        } else {
            None
        };

        let topic = JobTopic::Update(job_name)
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
            .map_err(|_| TransferError::Mqtt)?;

        // For QoS 1: wait for accepted/rejected response
        if let Some(mut sub) = sub {
            let result = loop {
                let message = match embassy_time::with_timeout(
                    embassy_time::Duration::from_secs(5),
                    sub.next_message(),
                )
                .await
                {
                    Ok(Some(msg)) => msg,
                    Ok(None) => break Err(TransferError::Mqtt),
                    Err(_) => {
                        warn!("Timeout waiting for job update accepted/rejected");
                        break Err(TransferError::Timeout);
                    }
                };

                match jobs::Topic::from_str(message.topic_name()) {
                    Some(jobs::Topic::UpdateAccepted(_)) => {
                        break Ok(());
                    }
                    Some(jobs::Topic::UpdateRejected(_)) => {
                        let (error_response, _) =
                            serde_json_core::from_slice::<ErrorResponse>(message.payload())
                                .map_err(|_| TransferError::Mqtt)?;

                        error!("Job update rejected: {:?}", error_response.message);
                        break Err(TransferError::UpdateRejected(error_response.code));
                    }
                    _ => {
                        // Not our topic, keep waiting
                        continue;
                    }
                }
            };

            let _ = sub.unsubscribe().await;
            return result;
        }

        Ok(())
    }
}
