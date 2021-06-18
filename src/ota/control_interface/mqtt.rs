use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};

use mqttrust::{QoS, SubscribeTopic};

use super::ControlInterface;
use crate::job::data_types::UpdateJobExecutionRequest;
use crate::ota::config::Config;
use crate::ota::encoding::FileContext;
use crate::{
    job::{
        data_types::{DescribeJobExecutionRequest, JobStatus},
        JobStatusReason,
    },
};

// FIXME: This can cause unit-tests to sometimes fail, due to parallel execution
static REQUEST_CNT: AtomicU32 = AtomicU32::new(0);

impl<T: mqttrust::Mqtt> ControlInterface for T {
    /// Check for next available OTA job from the job service by publishing a
    /// "get next job" message to the job service.
    fn request_job(&self) -> Result<(), ()> {
        let thing_name = self.client_id();

        // Subscribe to the OTA job notification topics
        let mut topic_path = heapless::String::new();
        topic_path
            .write_fmt(format_args!("$aws/things/{}/jobs/notify-next", thing_name))
            .map_err(drop)?;

        self.subscribe(SubscribeTopic {
            topic_path,
            qos: QoS::AtLeastOnce,
        })
        .map_err(drop)?;

        let request_cnt = REQUEST_CNT.fetch_add(1, Ordering::Relaxed);

        // Obtains a unique client token on the form
        // `{requestNumber}:{thingName}`, and increments the request counter
        let mut client_token = heapless::String::new();
        client_token
            .write_fmt(format_args!("{}:{}", request_cnt, thing_name))
            .map_err(drop)?;

        let mut topic_path = heapless::String::<64>::new();
        topic_path
            .write_fmt(format_args!("$aws/things/{}/jobs/$next/get", thing_name))
            .map_err(drop)?;

        let p = serde_json_core::to_vec::<_, 128>(&DescribeJobExecutionRequest {
            execution_number: None,
            include_job_document: None,
            client_token,
        })
        .map_err(drop)?;

        self.publish(topic_path.as_str(), &p, QoS::AtLeastOnce)
            .map_err(drop)?;
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

        let payload = serde_json_core::to_vec::<_, 512>(&UpdateJobExecutionRequest {
            execution_number: None,
            expected_version: None,
            include_job_document: None,
            include_job_execution_state: None,
            status,
            status_details: Some(&file_ctx.status_details),
            step_timeout_in_minutes: None,
            client_token: None,
        })
        .map_err(drop)?;

        // Publish the string created above
        let mut topic_path = heapless::String::<64>::new();
        topic_path
            .write_fmt(format_args!(
                "$aws/things/{}/jobs/{}/update",
                self.client_id(),
                file_ctx.stream_name.as_str()
            ))
            .map_err(drop)?;

        self.publish(topic_path.as_str(), &payload, qos)
            .map_err(drop)?;

        Ok(())
    }

    /// Perform any cleanup operations required for control plane
    fn cleanup(&self) -> Result<(), ()> {
        let mut topic_path = heapless::String::new();
        topic_path
            .write_fmt(format_args!(
                "$aws/things/{}/jobs/notify-next",
                self.client_id(),
            ))
            .map_err(drop)?;

        self.unsubscribe(topic_path).map_err(drop)?;
        Ok(())
    }
}
