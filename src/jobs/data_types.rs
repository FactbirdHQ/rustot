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

use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// https://docs.aws.amazon.com/iot/latest/apireference/API_DescribeThing.html
pub const MAX_THING_NAME_LEN: usize = 128;
pub const MAX_CLIENT_TOKEN_LEN: usize = MAX_THING_NAME_LEN + 10;
pub const MAX_JOB_ID_LEN: usize = 64;
pub const MAX_STREAM_ID_LEN: usize = 64;
pub const MAX_PENDING_JOBS: usize = 4;
pub const MAX_RUNNING_JOBS: usize = 1;

pub type StatusDetails = heapless::FnvIndexMap<heapless::String<15>, heapless::String<11>, 4>;

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
pub struct DescribeJobExecutionRequest<'a> {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

/// Topic: $aws/things/{thingName}/jobs/{jobId}/get/accepted
#[derive(Debug, PartialEq, Deserialize)]
pub struct DescribeJobExecutionResponse<'a, J> {
    /// Contains data about a job execution.
    #[serde(rename = "execution")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<JobExecution<J>>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: &'a str,
}

/// Gets the list of all jobs for a thing that are not in a terminal state.
///
/// Topic: $aws/things/{thingName}/jobs/get
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GetPendingJobExecutionsRequest<'a> {
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

/// Topic (accepted): $aws/things/{thingName}/jobs/get/accepted \
/// Topic (rejected): $aws/things/{thingName}/jobs/get/rejected
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GetPendingJobExecutionsResponse<'a> {
    /// A list of JobExecutionSummary objects with status IN_PROGRESS.
    #[serde(rename = "inProgressJobs")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_progress_jobs: Option<Vec<JobExecutionSummary, MAX_RUNNING_JOBS>>,
    /// A list of JobExecutionSummary objects with status QUEUED.
    #[serde(rename = "queuedJobs")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_jobs: Option<Vec<JobExecutionSummary, MAX_PENDING_JOBS>>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: &'a str,
}

/// Contains data about a job execution.
#[derive(Debug, PartialEq, Deserialize)]
pub struct JobExecution<J> {
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
    pub job_document: Option<J>,
    /// The unique identifier you assigned to this job when it was created.
    #[serde(rename = "jobId")]
    pub job_id: String<MAX_JOB_ID_LEN>,
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
    pub status_details: Option<StatusDetails>,
    // The name of the thing that is executing the job.
    #[serde(rename = "thingName")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thing_name: Option<String<MAX_THING_NAME_LEN>>,
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
    pub status_details: Option<StatusDetails>,
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
    pub job_id: Option<String<MAX_JOB_ID_LEN>>,
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
pub struct StartNextPendingJobExecutionRequest<'a> {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

/// Topic (accepted): $aws/things/{thingName}/jobs/start-next/accepted \
/// Topic (rejected): $aws/things/{thingName}/jobs/start-next/rejected
#[derive(Debug, PartialEq, Deserialize)]
pub struct StartNextPendingJobExecutionResponse<'a, J> {
    /// A JobExecution object.
    #[serde(rename = "execution")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<JobExecution<J>>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: &'a str,
}

/// Updates the status of a job execution. You can optionally create a step
/// timer by setting a value for the stepTimeoutInMinutes property. If you don't
/// update the value of this property by running UpdateJobExecution again, the
/// job execution times out when the step timer expires.
///
/// Topic: $aws/things/{thingName}/jobs/{jobId}/update
#[derive(Debug, PartialEq, Serialize)]
pub struct UpdateJobExecutionRequest<'a> {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<i64>,
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
    pub status_details: Option<&'a StatusDetails>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

/// Topic (accepted): $aws/things/{thingName}/jobs/{jobId}/update/accepted \
/// Topic (rejected): $aws/things/{thingName}/jobs/{jobId}/update/rejected
#[derive(Debug, PartialEq, Deserialize)]
pub struct UpdateJobExecutionResponse<'a, J> {
    /// A JobExecutionState object.
    #[serde(rename = "executionState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_state: Option<JobExecutionState>,
    /// The contents of the Job Documents.
    #[serde(rename = "jobDocument")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_document: Option<J>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: &'a str,
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
pub struct NextJobExecutionChanged<J> {
    /// Contains data about a job execution.
    #[serde(rename = "execution")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<JobExecution<J>>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Jobs {
    /// Queued jobs.
    #[serde(rename = "QUEUED")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<Vec<JobExecutionSummary, MAX_RUNNING_JOBS>>,
    /// In-progress jobs.
    #[serde(rename = "IN_PROGRESS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_progress: Option<Vec<JobExecutionSummary, MAX_RUNNING_JOBS>>,
}

/// Contains information about an error that occurred during an AWS IoT Jobs
/// service operation.
#[derive(Debug, PartialEq, Deserialize)]
pub struct ErrorResponse {
    code: ErrorCode,
    /// An error message string.
    message: String<128>,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<String<MAX_CLIENT_TOKEN_LEN>>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A JobExecutionState object.
    #[serde(rename = "executionState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_state: Option<JobExecutionState>,
}

#[cfg(test)]
mod test {
    use super::*;
    use heapless::Vec;
    use serde_json_core::{from_slice, to_string};

    /// Job document used while developing the module
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct TestJob {
        operation: String<128>,
        somerandomkey: String<128>,
    }

    /// All known job document that the device knows how to process.
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub enum JobDetails {
        #[cfg(test)]
        #[serde(rename = "test_job")]
        TestJob(TestJob),

        #[serde(other)]
        Unknown,
    }

    #[test]
    fn serialize_requests() {
        {
            let req = DescribeJobExecutionRequest {
                execution_number: Some(1),
                include_job_document: Some(true),
                client_token: Some("test_client:token"),
            };
            assert_eq!(
                to_string::<_, 512>(&req).unwrap().as_str(),
                r#"{"executionNumber":1,"includeJobDocument":true,"clientToken":"test_client:token"}"#
            );
        }

        {
            let req = GetPendingJobExecutionsRequest {
                client_token: Some("test_client:token_pending"),
            };
            assert_eq!(
                &to_string::<_, 512>(&req).unwrap(),
                r#"{"clientToken":"test_client:token_pending"}"#
            );
        }

        {
            let req = StartNextPendingJobExecutionRequest {
                client_token: Some("test_client:token_next_pending"),
                step_timeout_in_minutes: Some(50),
            };
            assert_eq!(
                &to_string::<_, 512>(&req).unwrap(),
                r#"{"stepTimeoutInMinutes":50,"clientToken":"test_client:token_next_pending"}"#
            );
            let req_none = StartNextPendingJobExecutionRequest {
                client_token: Some("test_client:token_next_pending"),
                step_timeout_in_minutes: None,
            };
            assert_eq!(
                &to_string::<_, 512>(&req_none).unwrap(),
                r#"{"clientToken":"test_client:token_next_pending"}"#
            );
        }

        {
            let req = UpdateJobExecutionRequest {
                client_token: Some("test_client:token_update"),
                step_timeout_in_minutes: Some(50),
                execution_number: Some(5),
                expected_version: Some(2),
                include_job_document: Some(true),
                include_job_execution_state: Some(true),
                status_details: None,
                status: JobStatus::Failed,
            };
            assert_eq!(
                &to_string::<_, 512>(&req).unwrap(),
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

        let (response, _) = from_slice::<NextJobExecutionChanged<JobDetails>>(payload).unwrap();

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
                in_progress_jobs: Some(Vec::<JobExecutionSummary, MAX_RUNNING_JOBS>::new()),
                queued_jobs: None,
                timestamp: 1587381778,
                client_token: "0:client_name",
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

        let mut queued_jobs: Vec<JobExecutionSummary, MAX_PENDING_JOBS> = Vec::new();
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
                in_progress_jobs: Some(Vec::<JobExecutionSummary, MAX_RUNNING_JOBS>::new()),
                queued_jobs: Some(queued_jobs),
                timestamp: 1587381778,
                client_token: "0:client_name",
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

        let (response, _) =
            from_slice::<DescribeJobExecutionResponse<JobDetails>>(payload).unwrap();

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
                client_token: "0:client_name",
            }
        );
    }
}
