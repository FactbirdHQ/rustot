use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

use super::{MAX_JOB_ID_LEN, MAX_PENDING_JOBS, MAX_RUNNING_JOBS, StatusDetails};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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

/// Topic (accepted): $aws/things/{thingName}/jobs/{jobId}/get/accepted \
/// Topic (rejected): $aws/things/{thingName}/jobs/{jobId}/get/rejected
#[derive(Debug, PartialEq, Deserialize)]
pub struct DescribeJobExecutionResponse<'a, J> {
    /// Contains data about a job execution.
    #[serde(rename = "execution")]
    pub execution: Option<JobExecution<'a, J>>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
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
    pub client_token: Option<&'a str>,
}

/// Contains data about a job execution.
#[derive(Debug, PartialEq, Deserialize)]
pub struct JobExecution<'a, J> {
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
    pub job_id: &'a str,
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
    #[serde(
        borrow,
        default,
        deserialize_with = "lenient_status_details::deserialize"
    )]
    pub status_details: Option<StatusDetails<'a>>,
    // The name of the thing that is executing the job.
    #[serde(rename = "thingName")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thing_name: Option<&'a str>,
    /// The version of the job execution. Job execution versions are incremented
    /// each time they are updated by a device.
    #[serde(rename = "versionNumber")]
    pub version_number: i64,
}

/// Contains data about the state of a job execution.
#[derive(Debug, PartialEq, Deserialize)]
pub struct JobExecutionState<'a> {
    /// The status of the job execution. Can be one of: "QUEUED", "IN_PROGRESS",
    /// "FAILED", "SUCCESS", "CANCELED", "REJECTED", or "REMOVED".
    #[serde(rename = "status")]
    pub status: JobStatus,
    /// A collection of name/value pairs that describe the status of the job
    /// execution.
    #[serde(rename = "statusDetails")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(
        borrow,
        default,
        deserialize_with = "lenient_status_details::deserialize"
    )]
    pub status_details: Option<StatusDetails<'a>>,
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

/// Topic (accepted): $aws/things/{thingName}/jobs/start-next/accepted \
/// Topic (rejected): $aws/things/{thingName}/jobs/start-next/rejected
#[derive(Debug, PartialEq, Deserialize)]
pub struct StartNextPendingJobExecutionResponse<'a, J> {
    /// A JobExecution object.
    #[serde(rename = "execution")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<JobExecution<'a, J>>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    pub client_token: Option<&'a str>,
}

/// Topic (accepted): $aws/things/{thingName}/jobs/{jobId}/update/accepted \
/// Topic (rejected): $aws/things/{thingName}/jobs/{jobId}/update/rejected
#[derive(Debug, PartialEq, Deserialize)]
pub struct UpdateJobExecutionResponse<'a, J> {
    /// A JobExecutionState object.
    #[serde(rename = "executionState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_state: Option<JobExecutionState<'a>>,
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
    pub client_token: Option<&'a str>,
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
pub struct NextJobExecutionChanged<'a, J> {
    /// Contains data about a job execution.
    #[serde(rename = "execution")]
    #[serde(borrow)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<JobExecution<'a, J>>,
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
pub struct ErrorResponse<'a> {
    pub code: ErrorCode,
    /// An error message string.
    pub message: &'a str,
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
    /// The time, in seconds since the epoch, when the message was sent.
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
    /// A JobExecutionState object.
    #[serde(rename = "executionState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_state: Option<JobExecutionState<'a>>,
}

/// Skips the `statusDetails` entries a [`StatusDetails`] can't hold, rather than
/// failing the whole message: a multi-file job's `files` is escaped JSON, which
/// can't be borrowed, and a full map can't take more.
mod lenient_status_details {
    use core::fmt;

    use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};

    use super::StatusDetails;

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<StatusDetails<'de>>, D::Error> {
        Ok(Option::<Lenient>::deserialize(deserializer)?.map(|l| l.0))
    }

    struct Lenient<'a>(StatusDetails<'a>);

    impl<'de> Deserialize<'de> for Lenient<'de> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_map(LenientVisitor)
        }
    }

    struct LenientVisitor;

    impl<'de> Visitor<'de> for LenientVisitor {
        type Value = Lenient<'de>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a map of strings")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut details = StatusDetails::new();
            while let Some((key, value)) = map.next_entry::<Borrowed, Borrowed>()? {
                if let (Some(key), Some(value)) = (key.0, value.0) {
                    let _ = details.insert(key, value);
                }
            }
            Ok(Lenient(details))
        }
    }

    /// `None` for a string that had to be unescaped into a temporary.
    struct Borrowed<'a>(Option<&'a str>);

    impl<'de> Deserialize<'de> for Borrowed<'de> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_str(BorrowedVisitor)
        }
    }

    struct BorrowedVisitor;

    impl<'de> Visitor<'de> for BorrowedVisitor {
        type Value = Borrowed<'de>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a string")
        }

        fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E> {
            Ok(Borrowed(Some(v)))
        }

        fn visit_str<E>(self, _v: &str) -> Result<Self::Value, E> {
            Ok(Borrowed(None))
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use heapless::Vec;
    use serde_json_core::from_slice;

    /// Job document used while developing the module
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct TestJob<'a> {
        operation: &'a str,
        somerandomkey: &'a str,
    }

    /// All known job document that the device knows how to process.
    #[derive(Debug, PartialEq, Deserialize)]
    pub enum JobDetails<'a> {
        #[serde(rename = "test_job")]
        #[serde(borrow)]
        TestJob(TestJob<'a>),

        #[serde(other)]
        Unknown,
    }

    const MULTI_FILE_EXECUTION: &str = r#"{
        "timestamp": 1587471560,
        "execution": {
            "jobId": "mini",
            "status": "IN_PROGRESS",
            "statusDetails": {
                "self_test": "active",
                "progress": "3/3",
                "files": "[{\"id\":0,\"status\":\"succeeded\",\"progress\":\"2/2\"}]"
            },
            "queuedAt": 1587471559,
            "lastUpdatedAt": 1587471559,
            "versionNumber": 3
        }
    }"#;

    #[test]
    fn escaped_status_details_are_skipped_not_fatal() {
        let response: DescribeJobExecutionResponse<JobDetails> =
            serde_json::from_str(MULTI_FILE_EXECUTION).unwrap();
        let details = response.execution.unwrap().status_details.unwrap();

        assert_eq!(details.get("self_test"), Some(&"active"));
        assert_eq!(details.get("progress"), Some(&"3/3"));
        assert_eq!(details.get("files"), None);
    }

    #[test]
    fn escaped_status_details_parse_without_an_unescape_buffer() {
        let (response, _) =
            from_slice::<DescribeJobExecutionResponse<JobDetails>>(MULTI_FILE_EXECUTION.as_bytes())
                .unwrap();
        let details = response.execution.unwrap().status_details.unwrap();

        assert_eq!(details.get("self_test"), Some(&"active"));
    }

    #[test]
    fn status_details_beyond_capacity_are_skipped() {
        let entries: std::vec::Vec<_> = (0..10).map(|i| format!(r#""k{i}":"v""#)).collect();
        let payload = format!(
            r#"{{"status":"IN_PROGRESS","statusDetails":{{{}}},"versionNumber":1}}"#,
            entries.join(",")
        );
        let state: JobExecutionState = serde_json::from_str(&payload).unwrap();

        assert_eq!(state.status_details.unwrap().len(), 8);
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
                        operation: "test",
                        somerandomkey: "random_value"
                    })),
                    job_id: "mini",
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
                client_token: Some("0:client_name"),
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
                job_id: Some(String::try_from("test").unwrap()),
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
                client_token: Some("0:client_name"),
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
                        operation: "test",
                        somerandomkey: "random_value"
                    })),
                    job_id: "test",
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
                client_token: Some("0:client_name"),
            }
        );
    }
}
