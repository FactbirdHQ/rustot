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

mod agent;

pub use agent::{is_job_message, JobAgent};

use crate::consts::{
    MaxClientTokenLen, MaxJobIdLen, MaxPendingJobs, MaxRunningJobs, MaxThingNameLen,
};
use heapless::{consts, String, Vec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum JobStatus {
    #[serde(rename = "QUEUED")]
    Queued,
    #[serde(rename = "IN_PROGRESS")]
    InProgress,
    #[serde(rename = "FAILED")]
    Failed,
    #[serde(rename = "SUCCEEDED")]
    Succeeded,
    #[serde(rename = "CANCELED")]
    Canceled,
    #[serde(rename = "REJECTED")]
    Rejected,
    #[serde(rename = "REMOVED")]
    Removed,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum ErrorCode {
    /// The request was sent to a topic in the AWS IoT Jobs namespace that does
    /// not map to any API.
    InvalidTopic,
    /// The contents of the request could not be interpreted as valid
    /// UTF-8-encoded JSON.
    InvalidJson,
    /// The contents of the request were invalid. For example, this code is
    /// returned when an UpdateJobExecution request contains invalid status
    /// details. The message contains details about the error.
    InvalidRequest,
    /// An update attempted to change the job execution to a state that is
    /// invalid because of the job execution's current state (for example, an
    /// attempt to change a request in state SUCCEEDED to state IN_PROGRESS). In
    /// this case, the body of the error message also contains the
    /// executionState field.
    InvalidStateTransition,
    /// The JobExecution specified by the request topic does not exist.
    ResourceNotFound,
    /// The expected version specified in the request does not match the version
    /// of the job execution in the AWS IoT Jobs service. In this case, the body
    /// of the error message also contains the executionState field.
    VersionMismatch,
    /// There was an internal error during the processing of the request.
    InternalError,
    /// The request was throttled.
    RequestThrottled,
    /// Occurs when a command to describe a job is performed on a job that is in
    /// a terminal state.
    TerminalStateReached,
}

/// Gets detailed information about a job execution.
///
/// You can set the jobId to $next to return the next pending job execution for
/// a thing (status IN_PROGRESS or QUEUED).
///
/// Topic: $aws/things/{thingName}/jobs/{jobId}/get
#[derive(Debug, Clone, PartialEq, Serialize)]
struct DescribeJobExecutionRequest {
    /// Optional. A number that identifies a particular job execution on a
    /// particular device. If not specified, the latest job execution is
    /// returned.
    #[serde(rename = "executionNumber")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_number: Option<i64>,
    /// Optional. When set to true, the response contains the job document. The
    /// default is false.
    #[serde(rename = "includeJobDocument")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_job_document: Option<bool>,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: String<MaxClientTokenLen>,
}

/// Topic: $aws/things/{thingName}/jobs/{jobId}/get/accepted
#[derive(Debug, PartialEq, Deserialize)]
pub struct DescribeJobExecutionResponse {
    /// Contains data about a job execution.
    #[serde(rename = "execution")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<JobExecution>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: String<MaxClientTokenLen>,
}

/// Gets the list of all jobs for a thing that are not in a terminal state.
///
/// Topic: $aws/things/{thingName}/jobs/get
#[derive(Debug, Clone, PartialEq, Serialize)]
struct GetPendingJobExecutionsRequest {
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: String<MaxClientTokenLen>,
}

/// Topic (accepted): $aws/things/{thingName}/jobs/get/accepted \
/// Topic (rejected): $aws/things/{thingName}/jobs/get/rejected
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GetPendingJobExecutionsResponse {
    /// A list of JobExecutionSummary objects with status IN_PROGRESS.
    #[serde(rename = "inProgressJobs")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_progress_jobs: Option<Vec<JobExecutionSummary, MaxRunningJobs>>,
    /// A list of JobExecutionSummary objects with status QUEUED.
    #[serde(rename = "queuedJobs")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_jobs: Option<Vec<JobExecutionSummary, MaxPendingJobs>>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: String<MaxClientTokenLen>,
}

/// Contains data about a job execution.
#[derive(Debug, PartialEq, Deserialize)]
pub struct JobExecution {
    /// The estimated number of seconds that remain before the job execution
    /// status will be changed to <code>TIMED_OUT</code>.
    #[serde(rename = "approximateSecondsBeforeTimedOut")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approximate_seconds_before_timed_out: Option<i64>,
    /// A number that identifies a particular job execution on a particular
    /// device. It can be used later in commands that return or update job
    /// execution information.
    #[serde(rename = "executionNumber")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_number: Option<i64>,
    /// The content of the job document.
    #[serde(rename = "jobDocument")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_document: Option<JobDetails>,
    /// The unique identifier you assigned to this job when it was created.
    #[serde(rename = "jobId")]
    pub job_id: String<MaxJobIdLen>,
    /// The time, in seconds since the epoch, when the job execution was last
    /// updated.
    #[serde(rename = "lastUpdatedAt")]
    pub last_updated_at: i64,
    /// The time, in seconds since the epoch, when the job execution was
    /// enqueued.
    #[serde(rename = "queuedAt")]
    pub queued_at: i64,
    /// The time, in seconds since the epoch, when the job execution was
    /// started.
    #[serde(rename = "startedAt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// The status of the job execution. Can be one of: "QUEUED", "IN_PROGRESS",
    /// "FAILED", "SUCCESS", "CANCELED", "REJECTED", or "REMOVED".
    #[serde(rename = "status")]
    pub status: JobStatus,
    // / A collection of name/value pairs that describe the status of the job
    // execution.
    #[serde(rename = "statusDetails")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_details:
        Option<heapless::FnvIndexMap<String<consts::U8>, String<consts::U10>, consts::U4>>,
    // The name of the thing that is executing the job.
    #[serde(rename = "thingName")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thing_name: Option<String<MaxThingNameLen>>,
    /// The version of the job execution. Job execution versions are incremented
    /// each time they are updated by a device.
    #[serde(rename = "versionNumber")]
    pub version_number: i64,
}

/// Contains data about the state of a job execution.
#[derive(Debug, PartialEq, Deserialize)]
pub struct JobExecutionState {
    /// The status of the job execution. Can be one of: "QUEUED", "IN_PROGRESS",
    /// "FAILED", "SUCCESS", "CANCELED", "REJECTED", or "REMOVED".
    #[serde(rename = "status")]
    pub status: JobStatus,
    /// A collection of name/value pairs that describe the status of the job
    /// execution.
    #[serde(rename = "statusDetails")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_details:
        Option<heapless::FnvIndexMap<String<consts::U8>, String<consts::U10>, consts::U4>>,
    // The version of the job execution. Job execution versions are incremented
    // each time they are updated by a device.
    #[serde(rename = "versionNumber")]
    pub version_number: i64,
}

/// Contains a subset of information about a job execution.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JobExecutionSummary {
    /// A number that identifies a particular job execution on a particular
    /// device.
    #[serde(rename = "executionNumber")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_number: Option<i64>,
    /// The unique identifier you assigned to this job when it was created.
    #[serde(rename = "jobId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String<MaxJobIdLen>>,
    /// The time, in seconds since the epoch, when the job execution was last
    /// updated.
    #[serde(rename = "lastUpdatedAt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated_at: Option<i64>,
    /// The time, in seconds since the epoch, when the job execution was
    /// enqueued.
    #[serde(rename = "queuedAt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<i64>,
    /// The time, in seconds since the epoch, when the job execution started.
    #[serde(rename = "startedAt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// The version of the job execution. Job execution versions are incremented
    /// each time AWS IoT Jobs receives an update from a device.
    #[serde(rename = "versionNumber")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_number: Option<i64>,
}
/// Gets and starts the next pending job execution for a thing (status
/// IN_PROGRESS or QUEUED).
///
/// Any job executions with status IN_PROGRESS are returned first.
///
/// Job executions are returned in the order in which they were created.
///
/// If the next pending job execution is QUEUED, its state is changed to
/// IN_PROGRESS and the job execution's status details are set as specified.
///
/// If the next pending job execution is already IN_PROGRESS, its status details
/// are not changed.
///
/// If no job executions are pending, the response does not include the
/// execution field.
///
/// You can optionally create a step timer by setting a value for the
/// stepTimeoutInMinutes property. If you don't update the value of this
/// property by running UpdateJobExecution, the job execution times out when the
/// step timer expires.
///
/// Topic: $aws/things/{thingName}/jobs/start-next
#[derive(Debug, Clone, PartialEq, Serialize)]
struct StartNextPendingJobExecutionRequest {
    // / A collection of name/value pairs that describe the status of the job
    // execution. If not specified, the statusDetails are unchanged.
    // #[serde(rename = "statusDetails")] #[serde(skip_serializing_if =
    // "Option::is_none")] pub status_details:
    // Option<::std::collections::HashMap<String, String>>, Specifies the amount
    // of time this device has to finish execution of this job. If the job
    // execution status is not set to a terminal state before this timer
    // expires, or before the timer is reset (by calling
    // <code>UpdateJobExecution</code>, setting the status to
    // <code>IN_PROGRESS</code> and specifying a new timeout value in field
    // <code>stepTimeoutInMinutes</code>) the job execution status will be
    // automatically set to <code>TIMED_OUT</code>. Note that setting this
    // timeout has no effect on that job execution timeout which may have been
    // specified when the job was created (<code>CreateJob</code> using field
    // <code>timeoutConfig</code>).
    #[serde(rename = "stepTimeoutInMinutes")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_timeout_in_minutes: Option<i64>,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: String<MaxClientTokenLen>,
}

/// Topic (accepted): $aws/things/{thingName}/jobs/start-next/accepted \
/// Topic (rejected): $aws/things/{thingName}/jobs/start-next/rejected
#[derive(Debug, PartialEq, Deserialize)]
pub struct StartNextPendingJobExecutionResponse {
    /// A JobExecution object.
    #[serde(rename = "execution")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<JobExecution>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: String<MaxClientTokenLen>,
}

/// Updates the status of a job execution. You can optionally create a step
/// timer by setting a value for the stepTimeoutInMinutes property. If you don't
/// update the value of this property by running UpdateJobExecution again, the
/// job execution times out when the step timer expires.
///
/// Topic: $aws/things/{thingName}/jobs/{jobId}/update
#[derive(Debug, PartialEq, Serialize)]
struct UpdateJobExecutionRequest<'a> {
    /// Optional. A number that identifies a particular job execution on a
    /// particular device.
    #[serde(rename = "executionNumber")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_number: Option<i64>,
    /// Optional. The expected current version of the job execution. Each time
    /// you update the job execution, its version is incremented. If the version
    /// of the job execution stored in Jobs does not match, the update is
    /// rejected with a VersionMismatch error, and an ErrorResponse that
    /// contains the current job execution status data is returned. (This makes
    /// it unnecessary to perform a separate DescribeJobExecution request in
    /// order to obtain the job execution status data.)
    #[serde(rename = "expectedVersion")]
    pub expected_version: i64,
    /// Optional. When set to true, the response contains the job document. The
    /// default is false.
    #[serde(rename = "includeJobDocument")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_job_document: Option<bool>,
    /// Optional. When included and set to true, the response contains the
    /// JobExecutionState data. The default is false.
    #[serde(rename = "includeJobExecutionState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_job_execution_state: Option<bool>,
    /// The new status for the job execution (IN_PROGRESS, FAILED, SUCCESS, or
    /// REJECTED). This must be specified on every update.
    #[serde(rename = "status")]
    pub status: JobStatus,
    // /  Optional. A collection of name/value pairs that describe the status of
    // the job execution. If not specified, the statusDetails are unchanged.
    #[serde(rename = "statusDetails")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_details:
        Option<&'a heapless::FnvIndexMap<String<consts::U8>, String<consts::U10>, consts::U4>>,
    // Specifies the amount of time this device has to finish execution of this
    // job. If the job execution status is not set to a terminal state before
    // this timer expires, or before the timer is reset (by again calling
    // <code>UpdateJobExecution</code>, setting the status to
    // <code>IN_PROGRESS</code> and specifying a new timeout value in this
    // field) the job execution status will be automatically set to
    // <code>TIMED_OUT</code>. Note that setting or resetting this timeout has
    // no effect on that job execution timeout which may have been specified
    // when the job was created (<code>CreateJob</code> using field
    // <code>timeoutConfig</code>).
    #[serde(rename = "stepTimeoutInMinutes")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_timeout_in_minutes: Option<i64>,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: String<MaxClientTokenLen>,
}

/// Topic (accepted): $aws/things/{thingName}/jobs/{jobId}/update/accepted \
/// Topic (rejected): $aws/things/{thingName}/jobs/{jobId}/update/rejected
#[derive(Debug, PartialEq, Deserialize)]
pub struct UpdateJobExecutionResponse {
    /// A JobExecutionState object.
    #[serde(rename = "executionState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_state: Option<JobExecutionState>,
    /// The contents of the Job Documents.
    #[serde(rename = "jobDocument")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_document: Option<JobDetails>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: String<MaxClientTokenLen>,
}

/// Sent whenever a job execution is added to or removed from the list of
/// pending job executions for a thing.
///
/// Topic: $aws/things/{thingName}/jobs/notify
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JobExecutionsChanged {
    /// A list of JobExecutionSummary objects with status IN_PROGRESS.
    #[serde(rename = "jobs")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jobs: Option<Jobs>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
}

/// Sent whenever there is a change to which job execution is next on the list
/// of pending job executions for a thing, as defined for DescribeJobExecution
/// with jobId $next. This message is not sent when the next job's execution
/// details change, only when the next job that would be returned by
/// DescribeJobExecution with jobId $next has changed. Consider job executions
/// J1 and J2 with state QUEUED. J1 is next on the list of pending job
/// executions. If the state of J2 is changed to IN_PROGRESS while the state of
/// J1 remains unchanged, then this notification is sent and contains details of
/// J2.
///
/// Topic: $aws/things/{thingName}/jobs/notify-next
#[derive(Debug, PartialEq, Deserialize)]
pub struct NextJobExecutionChanged {
    /// Contains data about a job execution.
    #[serde(rename = "execution")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<JobExecution>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Jobs {
    /// Queued jobs.
    #[serde(rename = "QUEUED")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<Vec<JobExecutionSummary, MaxRunningJobs>>,
    /// In-progress jobs.
    #[serde(rename = "IN_PROGRESS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_progress: Option<Vec<JobExecutionSummary, MaxRunningJobs>>,
}

/// Contains information about an error that occurred during an AWS IoT Jobs
/// service operation.
#[derive(Debug, PartialEq, Deserialize)]
pub struct ErrorResponse {
    code: ErrorCode,
    /// An error message string.
    message: String<consts::U128>,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<String<MaxClientTokenLen>>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A JobExecutionState object.
    #[serde(rename = "executionState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_state: Option<JobExecutionState>,
}

/// Trait representing the capabilities of the AWS IoT Jobs Data Plane API. AWS
/// IoT Jobs Data Plane clients implement this trait.
pub trait IotJobsData {
    /// Gets detailed information about a job execution.
    ///
    /// You can set the jobId to $next to return the next pending job execution
    /// for a thing (status IN_PROGRESS or QUEUED).
    ///
    /// Topic: $aws/things/{thingName}/jobs/{jobId}/get
    fn describe_job_execution<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        job_id: &str,
        execution_number: Option<i64>,
        include_job_document: Option<bool>,
    ) -> Result<(), JobError>;

    /// Gets the list of all jobs for a thing that are not in a terminal status
    ///
    /// Topic: $aws/things/{thingName}/jobs/get
    fn get_pending_job_executions<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
    ) -> Result<(), JobError>;

    /// Gets and starts the next pending job execution for a thing (status
    /// IN_PROGRESS or QUEUED).
    ///
    /// Any job executions with status IN_PROGRESS are returned first.
    ///
    /// Job executions are returned in the order in which they were created.
    ///
    /// If the next pending job execution is QUEUED, its state is changed to
    /// IN_PROGRESS and the job execution's status details are set as specified.
    ///
    /// If the next pending job execution is already IN_PROGRESS, its status
    /// details are not changed.
    ///
    /// If no job executions are pending, the response does not include the
    /// execution field.
    ///
    /// You can optionally create a step timer by setting a value for the
    /// stepTimeoutInMinutes property. If you don't update the value of this
    /// property by running UpdateJobExecution, the job execution times out when
    /// the step timer expires.
    ///
    /// Topic: $aws/things/{thingName}/jobs/start-next
    fn start_next_pending_job_execution<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        step_timeout_in_minutes: Option<i64>,
    ) -> Result<(), JobError>;

    /// Updates the status of a job execution. You can optionally create a step
    /// timer by setting a value for the stepTimeoutInMinutes property. If you
    /// don't update the value of this property by running UpdateJobExecution
    /// again, the job execution times out when the step timer expires.
    ///
    /// Topic: $aws/things/{thingName}/jobs/{jobId}/update
    fn update_job_execution<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        status: JobStatus,
        status_details: Option<
            heapless::FnvIndexMap<String<consts::U8>, String<consts::U10>, consts::U4>,
        >,
    ) -> Result<(), JobError>;

    /// Subscribe to relevant job topics.
    ///
    /// Topics:
    /// - $aws/things/{thingName}/jobs/notify-next
    /// - $aws/things/{thingName}/jobs/$next/get/accepted
    fn subscribe_to_jobs<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
    ) -> Result<(), JobError>;

    /// Unsubscribe from relevant job topics.
    ///
    /// Topics:
    /// - $aws/things/{thingName}/jobs/notify-next
    /// - $aws/things/{thingName}/jobs/$next/get/accepted
    fn unsubscribe_from_jobs<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
    ) -> Result<(), JobError>;

    /// Handle incomming job messages and process them accordingly.
    ///
    /// Anything received on `$aws/things/{thingName}/jobs/+` will get processed
    /// by this function
    fn handle_message<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        publish: &mqttrust::PublishNotification,
    ) -> Result<Option<&JobNotification>, JobError>;
}

enum JobTopicType {
    Notify,
    NotifyNext,
    UpdateAccepted(String<MaxJobIdLen>),
    UpdateRejected(String<MaxJobIdLen>),
    GetAccepted(String<MaxJobIdLen>),
    GetRejected(String<MaxJobIdLen>),
    Invalid,
}

impl JobTopicType {
    /// Checks if a given topic path is a valid `Jobs` topic
    ///
    /// Example:
    /// ```
    /// use heapless::{String, Vec, consts};
    ///
    /// let topic_path: String<consts::U64> = String::from("$aws/things/SomeThingName/jobs/notify-next");
    /// let topic_tokens: Vec<&str, consts::U8> = topic_path.splitn(8, '/').collect();
    ///
    /// assert!(JobTopicType::check("SomeThingName", &topic_tokens).is_some());
    /// assert_eq!(JobTopicType::check("SomeThingName", &topic_tokens), Some(JobTopicType::NotifyNext));
    /// ```
    pub fn check<'a, N: heapless::ArrayLength<&'a str>>(
        expected_thing_name: &str,
        topic_tokens: &Vec<&'a str, N>,
    ) -> Option<Self> {
        if topic_tokens.get(2) != Some(&expected_thing_name) {
            return None;
        }

        let is_job = topic_tokens.get(0) == Some(&"$aws")
            && topic_tokens.get(1) == Some(&"things")
            && topic_tokens.get(3) == Some(&"jobs");

        if !is_job {
            return None;
        }

        Some(match topic_tokens.get(4) {
            Some(&"notify-next") => JobTopicType::NotifyNext,
            Some(&"notify") => JobTopicType::Notify,
            Some(job_id) => match topic_tokens.get(5) {
                Some(&"update") if topic_tokens.get(6) == Some(&"accepted") => {
                    JobTopicType::UpdateAccepted(String::from(*job_id))
                }
                Some(&"update") if topic_tokens.get(6) == Some(&"rejected") => {
                    JobTopicType::UpdateRejected(String::from(*job_id))
                }
                Some(&"get") if topic_tokens.get(6) == Some(&"accepted") => {
                    JobTopicType::GetAccepted(String::from(*job_id))
                }
                Some(&"get") if topic_tokens.get(6) == Some(&"rejected") => {
                    JobTopicType::GetRejected(String::from(*job_id))
                }
                Some(_) | None => JobTopicType::Invalid,
            },
            None => JobTopicType::Invalid,
        })
    }
}

#[derive(Debug)]
pub enum JobError {
    Mqtt,
    Serialize(serde_json_core::ser::Error),
    Deserialize(serde_json_core::de::Error),
    Rejected(ErrorResponse),
    Memory,
    Formatting,
    InvalidTopic,
    NoActiveJob,
}

impl From<serde_json_core::ser::Error> for JobError {
    fn from(e: serde_json_core::ser::Error) -> Self {
        JobError::Serialize(e)
    }
}
impl From<serde_json_core::de::Error> for JobError {
    fn from(e: serde_json_core::de::Error) -> Self {
        JobError::Deserialize(e)
    }
}

impl From<mqttrust::MqttClientError> for JobError {
    fn from(_: mqttrust::MqttClientError) -> Self {
        JobError::Mqtt
    }
}

/// Job document used while developing the module
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TestJob {
    operation: String<consts::U128>,
    somerandomkey: String<consts::U128>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FileDescription {
    #[serde(rename = "filepath")]
    pub filepath: String<consts::U64>,
    #[serde(rename = "filesize")]
    pub filesize: usize,
    #[serde(rename = "fileid")]
    pub fileid: u8,
    #[serde(rename = "certfile")]
    pub certfile: String<consts::U64>,
    #[serde(rename = "update_data_url")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_data_url: Option<String<consts::U64>>,
    #[serde(rename = "auth_scheme")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<String<consts::U64>>,
    #[serde(rename = "sig-sha1-rsa")]
    pub sig_sha1_rsa: String<consts::U64>,
    #[serde(rename = "attr")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum Protocol {
    #[serde(rename = "MQTT")]
    Mqtt,
    #[serde(rename = "HTTP")]
    Http,
}

/// OTA job document, compatible with FreeRTOS OTA process
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OtaJob {
    pub protocols: Vec<Protocol, consts::U2>,
    pub streamname: String<consts::U64>,
    pub files: Vec<FileDescription, consts::U1>,
}

/// All known job document that the device knows how to process.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum JobDetails {
    #[serde(rename = "afr_ota")]
    OtaJob(OtaJob),
    
    #[cfg(test)]
    #[serde(rename = "test_job")]
    TestJob(TestJob),

    #[serde(other)]
    Unknown,
}

#[derive(Debug, PartialEq)]
pub struct JobNotification {
    pub job_id: String<MaxJobIdLen>,
    pub version_number: i64,
    pub status: JobStatus,
    pub details: JobDetails,
    pub status_details:
        Option<heapless::FnvIndexMap<String<consts::U8>, String<consts::U10>, consts::U4>>,
}

#[cfg(test)]
mod test {
    use super::*;
    use core::cell::RefCell;
    use heapless::{consts, ArrayLength, Vec};
    use serde_json_core::{from_slice, to_string, to_vec};

    pub struct MockClient<L: ArrayLength<u8>, P: ArrayLength<u8>> {
        client_id: String<L>,
        pub calls: RefCell<Vec<mqttrust::Request<Vec<u8, P>>, consts::U15>>,
    }

    impl<L: ArrayLength<u8>, P: ArrayLength<u8>> MockClient<L, P> {
        pub fn new(client_id: String<L>) -> Self {
            MockClient {
                client_id,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl<L: ArrayLength<u8>, P: ArrayLength<u8>> mqttrust::Mqtt<Vec<u8, P>> for MockClient<L, P> {
        fn client_id(&self) -> &str {
            &self.client_id
        }

        fn send(
            &mut self,
            request: mqttrust::Request<Vec<u8, P>>,
        ) -> Result<(), mqttrust::MqttClientError> {
            self.calls
                .borrow_mut()
                .push(request)
                .map_err(|_| mqttrust::MqttClientError::Full)?;
            Ok(())
        }
        type Error = mqttrust::MqttClientError;
    }

    #[test]
    fn handle_message_notify_next() {
        let thing_name = "test_thing";
        let payload = br#"
        {
            "timestamp": 1587471560,
            "execution": {
                "jobId": "mini",
                "status": "QUEUED",
                "queuedAt": 1587471559,
                "lastUpdatedAt": 1587471559,
                "versionNumber": 1,
                "executionNumber": 1,
                "jobDocument": {
                    "test_job": {
                        "operation": "test",
                        "somerandomkey": "random_value"
                    }
                }
            }
        }
        "#;

        let mut mqtt: MockClient<consts::U128, consts::U512> =
            MockClient::new(String::from(thing_name));
        let mut job_agent = JobAgent::new(3);
        let notification = job_agent
            .handle_message(
                &mut mqtt,
                &mqttrust::PublishNotification {
                    dup: false,
                    qospid: mqttrust::QosPid::AtMostOnce,
                    retain: false,
                    topic_name: String::from("$aws/things/test_thing/jobs/notify-next"),
                    payload: Vec::from_slice(payload).unwrap(),
                },
            )
            .unwrap();

        assert_eq!(notification, None);
        assert_eq!(
            mqtt.calls.borrow().get(0),
            Some(
                &mqttrust::PublishRequest::new(
                    String::from("$aws/things/test_thing/jobs/mini/update"),
                    to_vec::<consts::U512, _>(&UpdateJobExecutionRequest {
                        execution_number: None,
                        expected_version: 1,
                        include_job_document: Some(true),
                        include_job_execution_state: Some(true),
                        status: JobStatus::InProgress,
                        status_details: None,
                        step_timeout_in_minutes: None,
                        client_token: String::from("0:test_thing"),
                    })
                    .unwrap()
                )
                .qos(mqttrust::QoS::AtMostOnce)
                .into()
            )
        );
    }

    #[test]
    fn serialize_requests() {
        {
            let req = DescribeJobExecutionRequest {
                execution_number: Some(1),
                include_job_document: Some(true),
                client_token: String::from("test_client:token"),
            };
            assert_eq!(
                to_string::<consts::U512, _>(&req).unwrap().as_str(),
                r#"{"executionNumber":1,"includeJobDocument":true,"clientToken":"test_client:token"}"#
            );
        }

        {
            let req = GetPendingJobExecutionsRequest {
                client_token: String::from("test_client:token_pending"),
            };
            assert_eq!(
                &to_string::<consts::U512, _>(&req).unwrap(),
                r#"{"clientToken":"test_client:token_pending"}"#
            );
        }

        {
            let req = StartNextPendingJobExecutionRequest {
                client_token: String::from("test_client:token_next_pending"),
                step_timeout_in_minutes: Some(50),
            };
            assert_eq!(
                &to_string::<consts::U512, _>(&req).unwrap(),
                r#"{"stepTimeoutInMinutes":50,"clientToken":"test_client:token_next_pending"}"#
            );
            let req_none = StartNextPendingJobExecutionRequest {
                client_token: String::from("test_client:token_next_pending"),
                step_timeout_in_minutes: None,
            };
            assert_eq!(
                &to_string::<consts::U512, _>(&req_none).unwrap(),
                r#"{"clientToken":"test_client:token_next_pending"}"#
            );
        }

        {
            let req = UpdateJobExecutionRequest {
                client_token: String::from("test_client:token_update"),
                step_timeout_in_minutes: Some(50),
                execution_number: Some(5),
                expected_version: 2,
                include_job_document: Some(true),
                include_job_execution_state: Some(true),
                status_details: None,
                status: JobStatus::Failed,
            };
            assert_eq!(
                &to_string::<consts::U512, _>(&req).unwrap(),
                r#"{"executionNumber":5,"expectedVersion":2,"includeJobDocument":true,"includeJobExecutionState":true,"status":"FAILED","stepTimeoutInMinutes":50,"clientToken":"test_client:token_update"}"#
            );
        }
    }

    #[test]
    fn deserialize_next_job_execution_changed() {
        let payload = br#"
        {
            "timestamp": 1587471560,
            "execution": {
                "jobId": "mini",
                "status": "QUEUED",
                "queuedAt": 1587471559,
                "lastUpdatedAt": 1587471559,
                "versionNumber": 1,
                "executionNumber": 1,
                "jobDocument": {
                    "test_job": {
                        "operation": "test",
                        "somerandomkey": "random_value"
                    }
                }
            }
        }
        "#;

        let (response, _) = from_slice::<NextJobExecutionChanged>(payload).unwrap();

        assert_eq!(
            response,
            NextJobExecutionChanged {
                execution: Some(JobExecution {
                    execution_number: Some(1),
                    job_document: Some(JobDetails::TestJob(TestJob {
                        operation: String::from("test"),
                        somerandomkey: String::from("random_value")
                    })),
                    job_id: String::from("mini"),
                    last_updated_at: 1587471559,
                    queued_at: 1587471559,
                    status: JobStatus::Queued,
                    version_number: 1,
                    approximate_seconds_before_timed_out: None,
                    status_details: None,
                    started_at: None,
                    thing_name: None,
                }),
                timestamp: 1587471560,
            }
        );
    }

    #[test]
    fn deserialize_get_pending_job_executions_response() {
        let payload = br#"{
                "clientToken": "0:client_name",
                "timestamp": 1587381778,
                "inProgressJobs": []
            }"#;

        let (response, _) = from_slice::<GetPendingJobExecutionsResponse>(payload).unwrap();

        assert_eq!(
            response,
            GetPendingJobExecutionsResponse {
                in_progress_jobs: Some(Vec::<JobExecutionSummary, MaxPendingJobs>::new()),
                queued_jobs: None,
                timestamp: 1587381778,
                client_token: String::from("0:client_name"),
            }
        );

        let payload = br#"{
                "clientToken": "0:client_name",
                "timestamp": 1587381778,
                "inProgressJobs": [],
                "queuedJobs": [
                    {
                        "executionNumber": 1,
                        "jobId": "test",
                        "lastUpdatedAt": 1587036256,
                        "queuedAt": 1587036256,
                        "versionNumber": 1
                    }
                ]
            }"#;

        let mut queued_jobs: Vec<JobExecutionSummary, MaxPendingJobs> = Vec::new();
        queued_jobs
            .push(JobExecutionSummary {
                execution_number: Some(1),
                job_id: Some(String::from("test")),
                last_updated_at: Some(1587036256),
                queued_at: Some(1587036256),
                started_at: None,
                version_number: Some(1),
            })
            .unwrap();

        let (response, _) = from_slice::<GetPendingJobExecutionsResponse>(payload).unwrap();

        assert_eq!(
            response,
            GetPendingJobExecutionsResponse {
                in_progress_jobs: Some(Vec::<JobExecutionSummary, MaxPendingJobs>::new()),
                queued_jobs: Some(queued_jobs),
                timestamp: 1587381778,
                client_token: String::from("0:client_name"),
            }
        );
    }

    #[test]
    fn deserialize_describe_job_execution_response() {
        let payload = br#"{
                "clientToken": "0:client_name",
                "timestamp": 1587381778,
                "execution": {
                    "jobId": "test",
                    "status": "QUEUED",
                    "queuedAt": 1587036256,
                    "lastUpdatedAt": 1587036256,
                    "versionNumber": 1,
                    "executionNumber": 1,
                    "jobDocument": {
                        "test_job": {
                            "operation": "test",
                            "somerandomkey": "random_value"
                        }
                    }
                }
            }"#;

        let (response, _) = from_slice::<DescribeJobExecutionResponse>(payload).unwrap();

        assert_eq!(
            response,
            DescribeJobExecutionResponse {
                execution: Some(JobExecution {
                    execution_number: Some(1),
                    job_document: Some(JobDetails::TestJob(TestJob {
                        operation: String::from("test"),
                        somerandomkey: String::from("random_value")
                    })),
                    job_id: String::from("test"),
                    last_updated_at: 1587036256,
                    queued_at: 1587036256,
                    status_details: None,
                    status: JobStatus::Queued,
                    version_number: 1,
                    approximate_seconds_before_timed_out: None,
                    started_at: None,
                    thing_name: None,
                }),
                timestamp: 1587381778,
                client_token: String::from("0:client_name"),
            }
        );
    }

    #[test]
    fn deserialize_ota_response() {
        let payload = br#"{
            "clientToken": "0:client_name",
            "timestamp": 1587541592,
            "execution": {
              "jobId": "AFR_OTA-ota_test2",
              "status": "QUEUED",
              "queuedAt": 1587535224,
              "lastUpdatedAt": 1587535224,
              "versionNumber": 1,
              "executionNumber": 1,
              "jobDocument": {
                "afr_ota": {
                  "protocols": [
                    "MQTT"
                  ],
                  "streamname": "AFR_OTA-9ddd6d15-cefc-494d-9979-688f4571c0b8",
                  "files": [
                    {
                      "filepath": "image",
                      "filesize": 181584,
                      "fileid": 0,
                      "certfile": "codesign",
                      "sig-sha1-rsa": "test"
                    }
                  ]
                }
              }
            }
          }"#;

        let (response, _) = from_slice::<DescribeJobExecutionResponse>(payload).unwrap();

        let mut files = Vec::new();
        files
            .push(FileDescription {
                filepath: String::from("image"),
                filesize: 181584,
                fileid: 0,
                certfile: String::from("codesign"),
                sig_sha1_rsa: String::from("test"),
                update_data_url: None,
                auth_scheme: None,
                attr: None,
            })
            .unwrap();

        let mut protocols = Vec::new();
        protocols.push(Protocol::Mqtt).unwrap();

        assert_eq!(
            response,
            DescribeJobExecutionResponse {
                execution: Some(JobExecution {
                    execution_number: Some(1),
                    job_document: Some(JobDetails::OtaJob(OtaJob {
                        self_test: None,
                        protocols,
                        streamname: String::from("AFR_OTA-9ddd6d15-cefc-494d-9979-688f4571c0b8"),
                        files
                    })),
                    job_id: String::from("AFR_OTA-ota_test2"),
                    last_updated_at: 1587535224,
                    queued_at: 1587535224,
                    status_details: None,
                    status: JobStatus::Queued,
                    version_number: 1,
                    approximate_seconds_before_timed_out: None,
                    started_at: None,
                    thing_name: None,
                }),
                timestamp: 1587541592,
                client_token: String::from("0:client_name"),
            }
        );
    }

    #[test]
    #[ignore]
    // Temp. ignored. Progress for serde(other) can be tracked at:
    // https://github.com/serde-rs/serde/issues/912
    fn deserialize_unknown_job_document() {
        let payload = br#"{
                "clientToken": "0:client_name",
                "timestamp": 1587381778,
                "execution": {
                    "jobId": "test",
                    "status": "QUEUED",
                    "queuedAt": 1587036256,
                    "lastUpdatedAt": 1587036256,
                    "versionNumber": 1,
                    "executionNumber": 1,
                    "jobDocument": {
                        "some_other_job": {
                            "operation": "test",
                            "somerandomkey": "random_value"
                        }
                    }
                }
            }"#;

        let (response, _) = from_slice::<DescribeJobExecutionResponse>(payload).unwrap();

        assert_eq!(
            response,
            DescribeJobExecutionResponse {
                execution: Some(JobExecution {
                    execution_number: Some(1),
                    job_document: Some(JobDetails::Unknown),
                    job_id: String::from("test"),
                    last_updated_at: 1587036256,
                    queued_at: 1587036256,
                    status: JobStatus::Queued,
                    version_number: 1,
                    approximate_seconds_before_timed_out: None,
                    started_at: None,
                    thing_name: None,
                    status_details: None,
                }),
                timestamp: 1587381778,
                client_token: String::from("0:client_name"),
            }
        );
    }
}
