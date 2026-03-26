use serde::Deserialize;

use crate::mqtt::{Mqtt, MqttClient, MqttMessage, QoS};

use super::{
    data_types::{DescribeJobExecutionResponse, JobExecution, NextJobExecutionChanged},
    JobError, JobTopic, Jobs, Topic,
};

/// Helper for the AWS IoT Jobs subscribe/describe lifecycle.
///
/// Encapsulates the topic formatting, subscription creation, and initial
/// `DescribeJobExecution($next)` publish into a single `subscribe()` call.
/// The user drives the message loop on the returned subscription.
///
/// # Example
///
/// ```ignore
/// let jobs = JobAgent::new(&mqtt);
///
/// loop {
///     let mut sub = jobs.subscribe().await.unwrap();
///
///     loop {
///         let Some(mut message) = sub.next_message().await else {
///             break; // clean session — resubscribe
///         };
///
///         if let Some(execution) = parse_job_message::<MyJobs>(&mut message) {
///             match execution.job_document {
///                 Some(MyJobs::Ota(doc)) => { /* handle OTA */ }
///                 Some(MyJobs::Reset(info)) => { /* handle reset */ }
///                 None => {}
///             }
///         }
///     }
/// }
/// ```
pub struct JobAgent<'a, C: MqttClient> {
    mqtt: &'a Mqtt<&'a C>,
}

impl<'a, C: MqttClient> JobAgent<'a, C> {
    pub fn new(mqtt: &'a Mqtt<&'a C>) -> Self {
        Self { mqtt }
    }

    /// Subscribe to job notification topics and request the current pending job.
    ///
    /// Subscribes to `notify-next` (for ongoing job changes) and
    /// `$next/get/accepted` (for the describe response), then publishes
    /// `DescribeJobExecution($next)` to sync with the cloud's current state.
    ///
    /// The first message on the subscription is typically the describe response
    /// (startup sync). Subsequent messages are `notify-next` pushes.
    ///
    /// If the subscription ends (clean session / disconnect), call `subscribe()`
    /// again to re-establish.
    pub async fn subscribe(&self) -> Result<C::Subscription<'a, 2>, JobError> {
        let client_id = self.mqtt.0.client_id();

        let notify_topic = JobTopic::NotifyNext.format::<256>(client_id)?;
        let describe_topic = JobTopic::DescribeAccepted("$next").format::<256>(client_id)?;

        let sub = self
            .mqtt
            .0
            .subscribe(&[
                (notify_topic.as_str(), QoS::AtMostOnce),
                (describe_topic.as_str(), QoS::AtMostOnce),
            ])
            .await
            .map_err(|_| JobError::Mqtt)?;

        // Publish DescribeJobExecution($next) to get the current pending job
        let describe = Jobs::describe();
        let topic = describe.topic(client_id)?;
        self.mqtt
            .0
            .publish(&topic, describe)
            .await
            .map_err(|_| JobError::Mqtt)?;

        Ok(sub)
    }
}

/// Parse a job execution from an MQTT message (from `notify-next` or
/// `$next/get/accepted`).
///
/// Returns `None` if the message topic is unrecognized, the payload fails to
/// deserialize, or the execution is absent.
pub fn parse_job_message<'a, J: Deserialize<'a>>(
    message: &'a mut impl MqttMessage,
) -> Option<JobExecution<'a, J>> {
    let topic = Topic::from_str(message.topic_name())?;

    match topic {
        Topic::NotifyNext => {
            let (changed, _) =
                serde_json_core::from_slice::<NextJobExecutionChanged<J>>(message.payload_mut())
                    .ok()?;
            changed.execution
        }
        Topic::DescribeAccepted(_) => {
            let (response, _) = serde_json_core::from_slice::<DescribeJobExecutionResponse<J>>(
                message.payload_mut(),
            )
            .ok()?;
            response.execution
        }
        _ => None,
    }
}
