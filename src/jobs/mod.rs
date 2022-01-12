//! # Iot Jobs Data
//!
//! ## Programming Devices to Work with Jobs
//!
//! The examples in this section use MQTT to illustrate how a device works with
//! the AWS IoT Jobs service. Alternatively, you could use the corresponding API
//! or CLI commands. For these examples, we assume a device called MyThing
//! subscribes to the following MQTT topics:
//!
//! $aws/things/{MyThing}/jobs/notify (or
//! $aws/things/{MyThing}/jobs/notify-next)
//!
//! $aws/things/{MyThing}/jobs/get/accepted
//!
//! $aws/things/{MyThing}/jobs/get/rejected
//!
//! $aws/things/{MyThing}/jobs/{jobId}/get/accepted
//!
//! $aws/things/{MyThing}/jobs/{jobId}/get/rejected
//!
//! If you are using Code-signing for AWS IoT your device code must verify the
//! signature of your code file. The signature is in the job document in the
//! codesign property.
//!
//! ## Workflow:
//! 1. When a device first comes online, it should subscribe to the device's
//!    notify-next topic.
//! 2. Call the DescribeJobExecution MQTT API with jobId $next to get the next
//!    job, its job document, and other details, including any state saved in
//!    statusDetails. If the job document has a code file signature, you must
//!    verify the signature before proceeding with processing the job request.
//! 3. Call the UpdateJobExecution MQTT API to update the job status. Or, to
//!    combine this and the previous step in one call, the device can call
//!    StartNextPendingJobExecution.
//! 4. (Optional) You can add a step timer by setting a value for
//!    stepTimeoutInMinutes when you call either UpdateJobExecution or
//!    StartNextPendingJobExecution.
//! 5. Perform the actions specified by the job document using the
//!    UpdateJobExecution MQTT API to report on the progress of the job.
//! 6. Continue to monitor the job execution by calling the DescribeJobExecution
//!    MQTT API with this jobId. If the job execution is canceled or deleted
//!    while the device is running the job, the device should be capable of
//!    recovering to a valid state.
//! 7. Call the UpdateJobExecution MQTT API when finished with the job to update
//!    the job status and report success or failure.
//! 8. Because this job's execution status has been changed to a terminal state,
//!    the next job available for execution (if any) changes. The device is
//!    notified that the next pending job execution has changed. At this point,
//!    the device should continue as described in step 2.
//!
//! If the device remains online, it continues to receive a notifications of the
//! next pending job execution, including its job execution data, when it
//! completes a job or a new pending job execution is added. When this occurs,
//! the device continues as described in step 2.
//!
//! If the device is unable to execute the job, it should call the
//! UpdateJobExecution MQTT API to update the job status to REJECTED.
//!
//!
//! ## Jobs Notifications
//! The AWS IoT Jobs service publishes MQTT messages to reserved topics when
//! jobs are pending or when the first job execution in the list changes.
//! Devices can keep track of pending jobs by subscribing to these topics.
//!
//! Job notifications are published to MQTT topics as JSON payloads. There are
//! two kinds of notifications:
//!
//! A ListNotification contains a list of no more than 10 pending job
//! executions. The job executions in this list have status values of either
//! IN_PROGRESS or QUEUED. They are sorted by status (IN_PROGRESS job executions
//! before QUEUED job executions) and then by the times when they were queued.
//!
//! A ListNotification is published whenever one of the following criteria is
//! met.
//!
//! A new job execution is queued or changes to a non-terminal status
//! (IN_PROGRESS or QUEUED).
//!
//! An old job execution changes to a terminal status (FAILED, SUCCEEDED,
//! CANCELED, TIMED_OUT, REJECTED, or REMOVED).
//!
//! A NextNotification contains summary information about the one job execution
//! that is next in the queue.
//!
//! A NextNotification is published whenever the first job execution in the list
//! changes.
//!
//! A new job execution is added to the list as QUEUED, and it is the first one
//! in the list.
//!
//! The status of an existing job execution that was not the first one in the
//! list changes from QUEUED to IN_PROGRESS and becomes the first one in the
//! list. (This happens when there are no other IN_PROGRESS job executions in
//! the list or when the job execution whose status changes from QUEUED to
//! IN_PROGRESS was queued earlier than any other IN_PROGRESS job execution in
//! the list.)
//!
//! The status of the job execution that is first in the list changes to a
//! terminal status and is removed from the list.
pub mod data_types;
pub mod describe;
pub mod get_pending;
pub mod start_next;
pub mod subscribe;
pub mod unsubscribe;
pub mod update;

use core::fmt::Write;

use self::{
    data_types::JobStatus, describe::Describe, get_pending::GetPending, start_next::StartNext,
    subscribe::Subscribe, unsubscribe::Unsubscribe, update::Update,
};
pub use subscribe::Topic;

/// https://docs.aws.amazon.com/iot/latest/apireference/API_DescribeThing.html
pub const MAX_THING_NAME_LEN: usize = 128;
pub const MAX_CLIENT_TOKEN_LEN: usize = MAX_THING_NAME_LEN + 10;
pub const MAX_JOB_ID_LEN: usize = 64;
pub const MAX_STREAM_ID_LEN: usize = MAX_JOB_ID_LEN;
pub const MAX_PENDING_JOBS: usize = 1;
pub const MAX_RUNNING_JOBS: usize = 1;

pub type StatusDetails = heapless::FnvIndexMap<heapless::String<15>, heapless::String<11>, 4>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    Overflow,
    Encoding,
    Mqtt(mqttrust::MqttError),
}

impl From<mqttrust::MqttError> for JobError {
    fn from(e: mqttrust::MqttError) -> Self {
        Self::Mqtt(e)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobTopic<'a> {
    // Outgoing Topics
    GetNext,
    GetPending,
    StartNext,
    Get(&'a str),
    Update(&'a str),

    // Incoming Topics
    Notify,
    NotifyNext,
    GetAccepted,
    GetRejected,
    StartNextAccepted,
    StartNextRejected,
    DescribeAccepted(&'a str),
    DescribeRejected(&'a str),
    UpdateAccepted(&'a str),
    UpdateRejected(&'a str),
}

impl<'a> JobTopic<'a> {
    const PREFIX: &'static str = "$aws/things";

    pub fn check(s: &'a str) -> bool {
        s.starts_with(Self::PREFIX)
    }

    pub fn format<const L: usize>(&self, client_id: &str) -> Result<heapless::String<L>, JobError> {
        let mut topic_path = heapless::String::new();
        match self {
            Self::GetNext => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/$next/get",
                Self::PREFIX,
                client_id
            )),
            Self::GetPending => {
                topic_path.write_fmt(format_args!("{}/{}/jobs/get", Self::PREFIX, client_id))
            }
            Self::StartNext => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/start-next",
                Self::PREFIX,
                client_id
            )),
            Self::Get(job_id) => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/{}/get",
                Self::PREFIX,
                client_id,
                job_id
            )),
            Self::Update(job_id) => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/{}/update",
                Self::PREFIX,
                client_id,
                job_id
            )),

            Self::Notify => {
                topic_path.write_fmt(format_args!("{}/{}/jobs/notify", Self::PREFIX, client_id))
            }
            Self::NotifyNext => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/notify-next",
                Self::PREFIX,
                client_id
            )),
            Self::GetAccepted => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/get/accepted",
                Self::PREFIX,
                client_id
            )),
            Self::GetRejected => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/get/rejected",
                Self::PREFIX,
                client_id
            )),
            Self::StartNextAccepted => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/start-next/accepted",
                Self::PREFIX,
                client_id
            )),
            Self::StartNextRejected => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/start-next/rejected",
                Self::PREFIX,
                client_id
            )),
            Self::DescribeAccepted(job_id) => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/{}/get/accepted",
                Self::PREFIX,
                client_id,
                job_id
            )),
            Self::DescribeRejected(job_id) => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/{}/get/rejected",
                Self::PREFIX,
                client_id,
                job_id
            )),
            Self::UpdateAccepted(job_id) => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/{}/update/accepted",
                Self::PREFIX,
                client_id,
                job_id
            )),
            Self::UpdateRejected(job_id) => topic_path.write_fmt(format_args!(
                "{}/{}/jobs/{}/update/rejected",
                Self::PREFIX,
                client_id,
                job_id
            )),
        }
        .map_err(|_| JobError::Overflow)?;

        Ok(topic_path)
    }
}

pub struct Jobs;

impl Jobs {
    pub fn get_pending<'a>() -> GetPending<'a> {
        GetPending::new()
    }

    pub fn start_next<'a>() -> StartNext<'a> {
        StartNext::new()
    }

    pub fn describe<'a>() -> Describe<'a> {
        Describe::new()
    }

    pub fn update(job_id: &str, status: JobStatus) -> Update {
        Update::new(job_id, status)
    }

    pub fn subscribe<'a, const N: usize>() -> Subscribe<'a, N> {
        Subscribe::new()
    }

    pub fn unsubscribe<'a, const N: usize>() -> Unsubscribe<'a, N> {
        Unsubscribe::new()
    }
}
