use serde::{Deserialize, Serialize, de::IgnoredAny};

use crate::mqtt::{
    MaxJsonSize, Mqtt, MqttClient, MqttMessage, MqttSubscription, PublishOptions, QoS,
};

use super::{
    JobError, JobTopic, Jobs, MAX_JOB_ID_LEN, MAX_THING_NAME_LEN, Topic,
    data_types::{DescribeJobExecutionResponse, JobExecution, JobStatus, NextJobExecutionChanged},
    update::Update,
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
///         // Rejects jobs whose document doesn't deserialize into `MyJobs`.
///         if let Ok(Some(execution)) = jobs.parse_or_reject::<MyJobs>(&message).await {
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
                (notify_topic.as_str(), QoS::AtLeastOnce),
                (describe_topic.as_str(), QoS::AtLeastOnce),
            ])
            .await
            .map_err(|_| JobError::Mqtt)?;

        // Publish DescribeJobExecution($next) to get the current pending job
        let describe = Jobs::describe();
        let topic = describe.topic(client_id)?;
        self.mqtt
            .0
            .publish_with_options(
                &topic,
                describe,
                PublishOptions::new().qos(QoS::AtLeastOnce),
            )
            .await
            .map_err(|_| JobError::Mqtt)?;

        Ok(sub)
    }

    /// Report progress on a job.
    ///
    /// Publishes an `IN_PROGRESS` status update for the given job. The
    /// `status_details` can be any serializable type describing current
    /// progress.
    ///
    /// QoS 1 so a transient broker disconnect mid-job doesn't silently
    /// drop the progress update.
    pub async fn report_progress<S: MaxJsonSize>(
        &self,
        job_id: &str,
        status_details: &S,
    ) -> Result<(), JobError> {
        let client_id = self.mqtt.0.client_id();

        let topic = JobTopic::Update(job_id)
            .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>(client_id)?;

        let payload = Jobs::update(JobStatus::InProgress)
            .client_token(client_id)
            .status_details(status_details);

        self.mqtt
            .0
            .publish_with_options(&topic, payload, PublishOptions::new().qos(QoS::AtLeastOnce))
            .await
            .map_err(|_| JobError::Mqtt)
    }

    /// Mark a job as successfully completed.
    ///
    /// Publishes a `SUCCEEDED` status update and waits for the cloud to
    /// acknowledge. Optional `status_details` can carry final result
    /// information.
    pub async fn succeed_job<S: MaxJsonSize>(
        &self,
        job_id: &str,
        status_details: Option<&S>,
    ) -> Result<(), JobError> {
        match status_details {
            Some(details) => {
                let payload = Jobs::update(JobStatus::Succeeded).status_details(details);
                self.publish_and_wait(job_id, payload).await
            }
            None => {
                let payload = Jobs::update(JobStatus::Succeeded);
                self.publish_and_wait(job_id, payload).await
            }
        }
    }

    /// Mark a job as failed.
    ///
    /// Publishes a `FAILED` status update and waits for the cloud to
    /// acknowledge. Optional `status_details` can carry failure information.
    pub async fn fail_job<S: MaxJsonSize>(
        &self,
        job_id: &str,
        status_details: Option<&S>,
    ) -> Result<(), JobError> {
        match status_details {
            Some(details) => {
                let payload = Jobs::update(JobStatus::Failed).status_details(details);
                self.publish_and_wait(job_id, payload).await
            }
            None => {
                let payload = Jobs::update(JobStatus::Failed);
                self.publish_and_wait(job_id, payload).await
            }
        }
    }

    /// Reject a job with a reason string.
    ///
    /// Publishes a `REJECTED` status update for the given job and waits for
    /// the cloud to acknowledge. The reason is placed in `statusDetails` as
    /// `{"reason": "<value>"}`.
    ///
    /// This is job-agnostic — it works for any AWS IoT Jobs execution, not
    /// only OTA jobs.
    pub async fn reject_job(&self, job_id: &str, reason: &str) -> Result<(), JobError> {
        let details = RejectDetails { reason };
        let payload = Jobs::update(JobStatus::Rejected).status_details(&details);
        self.publish_and_wait(job_id, payload).await
    }

    /// Parse a job execution from `message`, rejecting it if its document
    /// cannot be deserialized into `J`.
    ///
    /// A job whose document the device cannot understand would otherwise
    /// stay queued and block every later job. Returns `Ok(None)` for messages
    /// without a job execution and after rejecting an invalid one.
    ///
    /// `message` stays borrowed while the rejection is published. With
    /// clients that hold a receive-buffer grant per message (e.g.
    /// `mqttrust`), pass an [`OwnedMessage`](crate::mqtt::OwnedMessage) copy.
    pub async fn parse_or_reject<'m, J: Deserialize<'m>>(
        &self,
        message: &'m impl MqttMessage,
    ) -> Result<Option<JobExecution<'m, J>>, JobError> {
        match parse_job_message(message) {
            Ok(execution) => Ok(execution),
            Err(invalid) => {
                warn!("Rejecting job {}: invalid job document", invalid.job_id);
                self.reject_job(invalid.job_id, "invalid job document")
                    .await?;
                Ok(None)
            }
        }
    }

    /// Publish a job update with QoS 1 and wait for the cloud to acknowledge.
    async fn publish_and_wait<S: MaxJsonSize>(
        &self,
        job_id: &str,
        payload: Update<'_, S>,
    ) -> Result<(), JobError> {
        let client_id = self.mqtt.0.client_id();

        let accepted_topic = JobTopic::UpdateAccepted(job_id)
            .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>(client_id)?;
        let rejected_topic = JobTopic::UpdateRejected(job_id)
            .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>(client_id)?;

        let mut sub = self
            .mqtt
            .0
            .subscribe(&[
                (accepted_topic.as_str(), QoS::AtLeastOnce),
                (rejected_topic.as_str(), QoS::AtLeastOnce),
            ])
            .await
            .map_err(|_| JobError::Mqtt)?;

        let topic = JobTopic::Update(job_id)
            .format::<{ MAX_THING_NAME_LEN + MAX_JOB_ID_LEN + 25 }>(client_id)?;

        let payload = payload.client_token(client_id);

        self.mqtt
            .0
            .publish_with_options(&topic, payload, PublishOptions::new().qos(QoS::AtLeastOnce))
            .await
            .map_err(|_| JobError::Mqtt)?;

        let result = loop {
            let message = match embassy_time::with_timeout(
                embassy_time::Duration::from_secs(5),
                sub.next_message(),
            )
            .await
            {
                Ok(Some(msg)) => msg,
                Ok(None) => break Err(JobError::Mqtt),
                Err(_) => break Err(JobError::Timeout),
            };

            match Topic::from_str(message.topic_name()) {
                Some(Topic::UpdateAccepted(_)) => break Ok(()),
                Some(Topic::UpdateRejected(_)) => break Err(JobError::Mqtt),
                _ => continue,
            }
        };

        let _ = sub.unsubscribe().await;
        result
    }
}

#[derive(Serialize)]
struct RejectDetails<'a> {
    reason: &'a str,
}

impl MaxJsonSize for RejectDetails<'_> {
    // `{"reason":"..."}` framing + a generous reason string.
    const MAX_JSON_SIZE: usize = 1024;
}

/// A job execution whose document could not be deserialized.
///
/// The execution exists in the cloud and stays queued, blocking every later
/// job, until it is rejected (see [`JobAgent::parse_or_reject`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidJobDocument<'a> {
    pub job_id: &'a str,
}

/// Parse a job execution from an MQTT message (from `notify-next` or
/// `$next/get/accepted`).
///
/// Returns `Ok(None)` if the topic is not a jobs topic, the execution is
/// absent, or the message is not a job execution at all. Returns
/// `Err(InvalidJobDocument)` if there is an execution but its job document
/// does not deserialize into `J`.
pub fn parse_job_message<'a, J: Deserialize<'a>>(
    message: &'a impl MqttMessage,
) -> Result<Option<JobExecution<'a, J>>, InvalidJobDocument<'a>> {
    let notify = match Topic::from_str(message.topic_name()) {
        Some(Topic::NotifyNext) => true,
        Some(Topic::DescribeAccepted(_)) => false,
        _ => return Ok(None),
    };

    if let Some(execution) = parse_execution::<J>(notify, message.payload()) {
        return Ok(execution);
    }

    // Parse again ignoring the document, to tell an invalid document (which
    // must be rejected) apart from a malformed message.
    match parse_execution::<IgnoredAny>(notify, message.payload()) {
        Some(Some(execution)) => Err(InvalidJobDocument {
            job_id: execution.job_id,
        }),
        _ => {
            warn!("Ignoring malformed job message on {}", message.topic_name());
            Ok(None)
        }
    }
}

fn parse_execution<'a, J: Deserialize<'a>>(
    notify: bool,
    payload: &'a [u8],
) -> Option<Option<JobExecution<'a, J>>> {
    if notify {
        serde_json_core::from_slice::<NextJobExecutionChanged<J>>(payload)
            .ok()
            .map(|(changed, _)| changed.execution)
    } else {
        serde_json_core::from_slice::<DescribeJobExecutionResponse<J>>(payload)
            .ok()
            .map(|(response, _)| response.execution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mqtt::mock::MockMessage;

    #[derive(Debug, Deserialize, PartialEq)]
    enum TestJobs {
        #[serde(rename = "known")]
        Known(u32),
    }

    const NOTIFY_NEXT: &str = "$aws/things/thing/jobs/notify-next";
    const DESCRIBE_ACCEPTED: &str = "$aws/things/thing/jobs/$next/get/accepted";

    fn execution(document: &str) -> String {
        format!(
            r#"{{"timestamp":1,"execution":{{"jobId":"job-1","status":"QUEUED","queuedAt":1,"lastUpdatedAt":1,"versionNumber":1,"executionNumber":1,"jobDocument":{document}}}}}"#
        )
    }

    #[test]
    fn valid_document_parses() {
        for topic in [NOTIFY_NEXT, DESCRIBE_ACCEPTED] {
            let message = MockMessage::new(topic, execution(r#"{"known":7}"#));
            let execution = parse_job_message::<TestJobs>(&message).unwrap().unwrap();
            assert_eq!(execution.job_id, "job-1");
            assert_eq!(execution.job_document, Some(TestJobs::Known(7)));
        }
    }

    #[test]
    fn invalid_document_reports_job_id() {
        for document in [r#"{"unknown":{"a":1}}"#, r#"{"known":"not a number"}"#] {
            let message = MockMessage::new(NOTIFY_NEXT, execution(document));
            assert_eq!(
                parse_job_message::<TestJobs>(&message).unwrap_err(),
                InvalidJobDocument { job_id: "job-1" }
            );
        }
    }

    #[test]
    fn no_execution_is_not_an_error() {
        let message = MockMessage::new(NOTIFY_NEXT, r#"{"timestamp":1}"#);
        assert_eq!(parse_job_message::<TestJobs>(&message), Ok(None));
    }

    #[test]
    fn malformed_message_is_ignored() {
        let message = MockMessage::new(NOTIFY_NEXT, "not json");
        assert_eq!(parse_job_message::<TestJobs>(&message), Ok(None));

        let message = MockMessage::new("$aws/things/thing/shadow/update", execution("{}"));
        assert_eq!(parse_job_message::<TestJobs>(&message), Ok(None));
    }
}
