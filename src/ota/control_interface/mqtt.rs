use core::fmt::Write;

use bitmaps::{Bits, BitsImpl};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_mqtt::{DeferredPayload, EncodingError, Publish, QoS, Subscribe, SubscribeTopic};
use futures::StreamExt as _;

use super::ControlInterface;
use crate::jobs::data_types::{ErrorResponse, JobStatus, UpdateJobExecutionResponse};
use crate::jobs::{JobError, JobTopic, Jobs, MAX_JOB_ID_LEN, MAX_THING_NAME_LEN};
use crate::ota::config::Config;
use crate::ota::encoding::json::JobStatusReason;
use crate::ota::encoding::{self, FileContext};
use crate::ota::error::OtaError;

impl<'a, M: RawMutex, const SUBS: usize> ControlInterface for embedded_mqtt::MqttClient<'a, M, SUBS>
where
    BitsImpl<{ SUBS }>: Bits,
{
    /// Check for next available OTA job from the job service by publishing a
    /// "get next job" message to the job service.
    async fn request_job(&self) -> Result<(), OtaError> {
        // FIXME: Serialize directly into the publish payload through `DeferredPublish` API
        let mut buf = [0u8; 512];
        let (topic, payload_len) = Jobs::describe().topic_payload(self.client_id(), &mut buf)?;

        self.publish(
            Publish::builder()
                .topic_name(&topic)
                .payload(&buf[..payload_len])
                .build(),
        )
        .await?;

        Ok(())
    }

    /// Update the job status on the service side with progress or completion
    /// info
    async fn update_job_status(
        &self,
        file_ctx: &mut FileContext,
        config: &Config,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), OtaError> {
        file_ctx
            .status_details
            .insert(
                heapless::String::try_from("self_test").unwrap(),
                heapless::String::try_from(reason.as_str()).unwrap(),
            )
            .map_err(|_| OtaError::Overflow)?;

        let mut qos = QoS::AtLeastOnce;

        if let (JobStatus::InProgress, _) | (JobStatus::Succeeded, _) = (status, reason) {
            let total_blocks =
                ((file_ctx.filesize + config.block_size - 1) / config.block_size) as u32;
            let received_blocks = total_blocks - file_ctx.blocks_remaining as u32;

            // Output a status update once in a while. Always update first and
            // last status
            if file_ctx.blocks_remaining != 0
                && received_blocks != 0
                && received_blocks % config.status_update_frequency != 0
            {
                return Ok(());
            }

            // Don't override the progress on succeeded, nor on self-test
            // active. (Cases where progress counter is lost due to device
            // restarts)
            if status != JobStatus::Succeeded && reason != JobStatusReason::SelfTestActive {
                let mut progress = heapless::String::new();
                progress
                    .write_fmt(format_args!("{}/{}", received_blocks, total_blocks))
                    .map_err(|_| OtaError::Overflow)?;

                file_ctx
                    .status_details
                    .insert(heapless::String::try_from("progress").unwrap(), progress)
                    .map_err(|_| OtaError::Overflow)?;
            }

            // Downgrade progress updates to QOS 0 to avoid overloading MQTT
            // buffers during active streaming. But make sure to always send and await ack for first update and last update
            if status == JobStatus::InProgress
                && file_ctx.blocks_remaining != 0
                && received_blocks != 0
            {
                qos = QoS::AtMostOnce;
            }
        }

        let mut sub = self
            .subscribe::<2>(
                Subscribe::builder()
                    .topics(&[
                        SubscribeTopic::builder()
                            .topic_path(
                                JobTopic::UpdateAccepted(file_ctx.job_name.as_str())
                                    .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 34 }>(
                                        self.client_id(),
                                    )?
                                    .as_str(),
                            )
                            .build(),
                        SubscribeTopic::builder()
                            .topic_path(
                                JobTopic::UpdateRejected(file_ctx.job_name.as_str())
                                    .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 34 }>(
                                        self.client_id(),
                                    )?
                                    .as_str(),
                            )
                            .build(),
                    ])
                    .build(),
            )
            .await?;

        let topic = JobTopic::Update(file_ctx.job_name.as_str())
            .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>(self.client_id())?;
        let payload = DeferredPayload::new(
            |buf| {
                Jobs::update(status)
                    .client_token(self.client_id())
                    .status_details(&file_ctx.status_details)
                    .payload(buf)
                    .map_err(|_| EncodingError::BufferSize)
            },
            512,
        );

        self.publish(
            Publish::builder()
                .qos(qos)
                .topic_name(&topic)
                .payload(payload)
                .build(),
        )
        .await?;

        loop {
            let message = sub.next().await.ok_or(JobError::Encoding)?;

            // Check if topic is GetAccepted
            match crate::jobs::Topic::from_str(message.topic_name()) {
                Some(crate::jobs::Topic::UpdateAccepted(_)) => {
                    // Check client token
                    let (response, _) = serde_json_core::from_slice::<
                        UpdateJobExecutionResponse<encoding::json::OtaJob<'_>>,
                    >(message.payload())
                    .map_err(|_| JobError::Encoding)?;

                    if response.client_token != Some(self.client_id()) {
                        error!(
                            "Unexpected client token received: {}, expected: {}",
                            response.client_token.unwrap_or("None"),
                            self.client_id()
                        );
                        continue;
                    }

                    return Ok(());
                }
                Some(crate::jobs::Topic::UpdateRejected(_)) => {
                    let (error_response, _) =
                        serde_json_core::from_slice::<ErrorResponse>(message.payload())
                            .map_err(|_| JobError::Encoding)?;

                    if error_response.client_token != Some(self.client_id()) {
                        continue;
                    }

                    return Err(OtaError::UpdateRejected(error_response.code));
                }
                _ => {
                    error!("Expected Topic name GetRejected or GetAccepted but got something else");
                }
            }
        }
    }
}
