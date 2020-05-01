use super::temp::{vec_to_vec, WriteAdapter};
use super::{
    DescribeJobExecutionRequest, DescribeJobExecutionResponse, ErrorResponse,
    GetPendingJobExecutionsRequest, IotJobsData, JobError, JobExecution, JobNotification,
    JobStatus, JobTopicType, NextJobExecutionChanged, StartNextPendingJobExecutionRequest,
    UpdateJobExecutionRequest, UpdateJobExecutionResponse,
};
use crate::consts::{MaxClientTokenLen, MaxTopicLen};
use heapless::{consts, String, Vec};

pub struct JobAgent {
    request_cnt: u32,
    active_job: Option<JobNotification>,
}

pub fn is_job_message(topic_name: &str) -> bool {
    let topic_tokens = topic_name.splitn(8, '/').collect::<Vec<&str, consts::U8>>();
    topic_tokens.get(0) == Some(&"$aws")
        && topic_tokens.get(1) == Some(&"things")
        && topic_tokens.get(3) == Some(&"jobs")
}

impl JobAgent {
    /// Create a new IoT Job Agent reacting to topics for `thing_name`
    pub fn new() -> Self {
        JobAgent {
            request_cnt: 0,
            active_job: None,
        }
    }

    /// Obtains a unique client token on the form `{requestNumber}:{thingName}`,
    /// and increments the request counter
    fn get_client_token(
        &mut self,
        thing_name: &str,
    ) -> Result<String<MaxClientTokenLen>, JobError> {
        let mut client_token = String::new();
        ufmt::uwrite!(
            WriteAdapter(&mut client_token),
            "{}:{}",
            self.request_cnt,
            thing_name
        )
        .map_err(|_| JobError::Formatting)?;
        self.request_cnt += 1;
        Ok(client_token)
    }

    fn update_job_execution_internal(
        &self,
        client: &impl mqttrust::Mqtt,
        job_id: &str,
        status: JobStatus,
        expected_version: i64,
        client_token: String<MaxClientTokenLen>,
        execution_number: Option<i64>,
        step_timeout_in_minutes: Option<i64>,
    ) -> Result<(), JobError> {
        let thing_name = client.client_id();

        let mut topic = String::new();
        ufmt::uwrite!(
            WriteAdapter(&mut topic),
            "$aws/things/{}/jobs/{}/update",
            thing_name,
            job_id
        )
        .map_err(|_| JobError::Formatting)?;

        // Always include job_document, and job_execution_state!
        client.publish::<MaxTopicLen, consts::U512>(
            topic,
            vec_to_vec(serde_json::to_vec(&UpdateJobExecutionRequest {
                execution_number,
                expected_version,
                include_job_document: Some(true),
                include_job_execution_state: Some(true),
                status,
                step_timeout_in_minutes,
                client_token,
            })?),
            mqttrust::QoS::AtLeastOnce,
        )?;

        Ok(())
    }

    fn handle_job_execution(
        &mut self,
        client: &impl mqttrust::Mqtt,
        execution: JobExecution,
    ) -> Result<Option<JobNotification>, JobError> {
        match execution.status {
            JobStatus::Queued if self.active_job.is_none() => {
                // There is a new queued job available, and we are not currently
                // processing a job. Update the status to InProgress, and set it
                // active in the accepted response
                // (`$aws/things/{thingName}/jobs/{jobId}/update/accepted`).
                let client_token = self.get_client_token(client.client_id())?;
                self.update_job_execution_internal(
                    client,
                    &execution.job_id,
                    JobStatus::InProgress,
                    execution.version_number,
                    client_token,
                    None,
                    None,
                )?;

                Ok(None)
            }
            JobStatus::InProgress if self.active_job.is_none() => {
                // If we dont have an active job, and the cloud reports job
                // should be active, it means something panicked, or we lost
                // track of the current job
                let client_token = self.get_client_token(client.client_id())?;
                self.update_job_execution_internal(
                    client,
                    &execution.job_id,
                    JobStatus::Failed,
                    execution.version_number,
                    client_token,
                    None,
                    None,
                )?;
                Ok(None)
            }
            JobStatus::InProgress if self.active_job.is_some() => {
                // If we have an active job, and the cloud reports job should be
                // active, it means there is an update for the currently
                // executing job, perhaps requested by the device
                // TODO:

                // Validate that the job update is indeed for the active_job
                // if execution.job_id == self.active_job {
                //     self.active_job = Some(JobNotification {

                //         ..self.active_job.clone().unwrap()
                //     });
                // }

                Ok(self.active_job.clone())
            }
            JobStatus::Canceled | JobStatus::Removed if self.active_job.is_some() => {
                // Current job is canceled! Abort if possible
                let job = self.active_job.clone().unwrap();
                self.active_job = None;

                Ok(Some(JobNotification {
                    status: JobStatus::Canceled,
                    ..job
                }))
            }
            _ => Ok(None),
        }
    }
}

impl IotJobsData for JobAgent {
    fn describe_job_execution(
        &mut self,
        client: &impl mqttrust::Mqtt,
        job_id: &str,
        execution_number: Option<i64>,
        include_job_document: Option<bool>,
    ) -> Result<(), JobError> {
        let thing_name = client.client_id();

        let mut topic = String::new();
        ufmt::uwrite!(
            WriteAdapter(&mut topic),
            "$aws/things/{}/jobs/{}/get",
            thing_name,
            job_id,
        )
        .map_err(|_| JobError::Formatting)?;

        let p = serde_json::to_vec(&DescribeJobExecutionRequest {
            execution_number,
            include_job_document,
            client_token: self.get_client_token(thing_name)?,
        })?;

        client.publish::<MaxTopicLen, consts::U128>(
            topic,
            vec_to_vec(p),
            mqttrust::QoS::AtLeastOnce,
        )?;

        Ok(())
    }

    fn get_pending_job_executions(&mut self, client: &impl mqttrust::Mqtt) -> Result<(), JobError> {
        let thing_name = client.client_id();

        let mut topic = String::new();
        ufmt::uwrite!(
            WriteAdapter(&mut topic),
            "$aws/things/{}/jobs/get",
            thing_name,
        )
        .map_err(|_| JobError::Formatting)?;

        client.publish::<MaxTopicLen, consts::U128>(
            topic,
            vec_to_vec(serde_json::to_vec(&GetPendingJobExecutionsRequest {
                client_token: self.get_client_token(thing_name)?,
            })?),
            mqttrust::QoS::AtLeastOnce,
        )?;

        Ok(())
    }

    fn start_next_pending_job_execution(
        &mut self,
        client: &impl mqttrust::Mqtt,
        step_timeout_in_minutes: Option<i64>,
    ) -> Result<(), JobError> {
        let thing_name = client.client_id();

        let mut topic = String::new();
        ufmt::uwrite!(
            WriteAdapter(&mut topic),
            "$aws/things/{}/jobs/start-next",
            thing_name,
        )
        .map_err(|_| JobError::Formatting)?;

        client.publish::<MaxTopicLen, consts::U128>(
            topic,
            vec_to_vec(serde_json::to_vec(&StartNextPendingJobExecutionRequest {
                step_timeout_in_minutes,
                client_token: self.get_client_token(thing_name)?,
            })?),
            mqttrust::QoS::AtLeastOnce,
        )?;

        Ok(())
    }

    fn update_job_execution(
        &mut self,
        client: &impl mqttrust::Mqtt,
        status: JobStatus,
    ) -> Result<(), JobError> {
        let client_token = self.get_client_token(client.client_id())?;

        if let Some(ref job) = &self.active_job {
            self.update_job_execution_internal(
                client,
                job.job_id.as_str(),
                status,
                job.version_number,
                client_token,
                None,
                None,
            )
        } else {
            Err(JobError::NoActiveJob)
        }
    }

    fn subscribe_to_jobs(&mut self, client: &impl mqttrust::Mqtt) -> Result<(), JobError> {
        let thing_name = client.client_id();
        let mut topics: Vec<mqttrust::SubscribeTopic, consts::U3> = Vec::new();

        topics
            .push(mqttrust::SubscribeTopic {
                topic_path: alloc::format!(
                    "$aws/things/{thing_name}/jobs/+/get/+",
                    thing_name = thing_name
                ),
                qos: mqttrust::QoS::AtLeastOnce,
            })
            .map_err(|_| JobError::Memory)?;

        topics
            .push(mqttrust::SubscribeTopic {
                topic_path: alloc::format!(
                    "$aws/things/{thing_name}/jobs/+/update/+",
                    thing_name = thing_name
                ),
                qos: mqttrust::QoS::AtLeastOnce,
            })
            .map_err(|_| JobError::Memory)?;

        topics
            .push(mqttrust::SubscribeTopic {
                topic_path: alloc::format!(
                    "$aws/things/{thing_name}/jobs/notify-next",
                    thing_name = thing_name
                ),
                qos: mqttrust::QoS::AtLeastOnce,
            })
            .map_err(|_| JobError::Memory)?;

        client.subscribe(topics)?;
        Ok(())
    }

    fn unsubscribe_from_jobs(&mut self, client: &impl mqttrust::Mqtt) -> Result<(), JobError> {
        let thing_name = client.client_id();

        let mut topics: Vec<alloc::string::String, consts::U2> = Vec::new();

        topics
            .push(alloc::format!(
                "$aws/things/{thing_name}/jobs/+/get/+",
                thing_name = thing_name
            ))
            .map_err(|_| JobError::Memory)?;

        topics
            .push(alloc::format!(
                "$aws/things/{thing_name}/jobs/+/update/+",
                thing_name = thing_name
            ))
            .map_err(|_| JobError::Memory)?;

        topics
            .push(alloc::format!(
                "$aws/things/{thing_name}/jobs/notify-next",
                thing_name = thing_name
            ))
            .map_err(|_| JobError::Memory)?;

        client.unsubscribe(topics)?;
        Ok(())
    }

    fn handle_message(
        &mut self,
        client: &impl mqttrust::Mqtt,
        publish: &mqttrust::Publish,
    ) -> Result<Option<JobNotification>, JobError> {
        match JobTopicType::check(
            client.client_id(),
            &publish
                .topic_name
                // Use the first 7
                // ($aws/things/{thingName}/jobs/$next/get/accepted), leaving
                // tokens 8+ at index 7
                .splitn(8, '/')
                .collect::<Vec<&str, consts::U8>>(),
        ) {
            None => {
                log::debug!("Not a job message!");
                Ok(None)
            }
            Some(JobTopicType::NotifyNext) => {
                // Message published to
                // `$aws/things/{thingName}/jobs/notify-next`

                let response: NextJobExecutionChanged = serde_json::from_slice(&publish.payload)?;
                log::debug!("notify-next message! {:?}", response);
                if let Some(execution) = response.execution {
                    // Job updated from the cloud!
                    self.handle_job_execution(client, execution)
                } else {
                    // Queue is empty! `jobs done`
                    Ok(None)
                }
            }
            Some(JobTopicType::Notify) => {
                // Message published to `$aws/things/{thingName}/jobs/notify`
                log::error!("notify message!, currently unhandled! Use notify-next instead");

                Ok(None)
            }
            Some(JobTopicType::GetAccepted(job_id)) => {
                // Message published to
                // `$aws/things/{thingName}/jobs/{jobId}/get/accepted`

                log::debug!("{}/get/accepted message!", job_id);
                if let Ok(response) =
                    serde_json::from_slice::<DescribeJobExecutionResponse>(&publish.payload)
                {
                    if let Some(execution) = response.execution {
                        self.handle_job_execution(client, execution)
                    } else {
                        Ok(None)
                    }
                } else {
                    log::error!(
                        "Unknown job document! {:?}",
                        alloc::string::String::from_utf8_lossy(&publish.payload)
                    );

                    // TODO: See progress for serde(other) can be tracked at:
                    // https://github.com/serde-rs/serde/issues/912
                    //
                    // Update to rejected with a reason of unknown job document!
                    // self.update_job_execution(client, &execution.job_id,
                    // JobStatus::Rejected, execution.version_number, None, None
                    // )?;

                    Ok(None)
                }
            }
            Some(JobTopicType::UpdateAccepted(job_id)) => {
                // Message published to
                // `$aws/things/{thingName}/jobs/{jobId}/update/accepted`
                log::debug!("{}/update/accepted message!", job_id);

                match serde_json::from_slice::<UpdateJobExecutionResponse>(&publish.payload) {
                    Ok(UpdateJobExecutionResponse {
                        execution_state,
                        job_document,
                        timestamp: _,
                        client_token: _,
                    }) if execution_state.is_some() && job_document.is_some() => {
                        let state = execution_state.unwrap();

                        match state.status {
                            JobStatus::Queued | JobStatus::InProgress => {
                                self.active_job = Some(JobNotification {
                                    job_id,
                                    version_number: state.version_number,
                                    status: state.status,
                                    details: job_document.unwrap(),
                                });

                            }
                            JobStatus::Failed | JobStatus::Rejected | JobStatus::Succeeded => {
                                self.active_job = None;
                            }
                            JobStatus::Canceled | JobStatus::Removed => {
                                self.active_job = None;
                            }
                        }
                        Ok(self.active_job.clone())
                    }
                    Ok(_) => {
                        // job_execution_state or job_document is missing, should never happen!
                        Ok(None)
                    }
                    Err(_) => Err(JobError::InvalidTopic),
                }
            }
            Some(JobTopicType::GetRejected(job_id)) => {
                // Message published to
                // `$aws/things/{thingName}/jobs/{jobId}/get/rejected`
                log::debug!("{}/get/rejected message!", job_id);
                let error: ErrorResponse = serde_json::from_slice(&publish.payload)?;
                log::debug!("{:?}", error);
                Err(JobError::Rejected(error))
            }
            Some(JobTopicType::UpdateRejected(job_id)) => {
                // Message published to
                // `$aws/things/{thingName}/jobs/{jobId}/update/rejected`
                log::debug!("{}/update/rejected message!", job_id);
                let error: ErrorResponse = serde_json::from_slice(&publish.payload)?;
                log::debug!("{:?}", error);
                Err(JobError::Rejected(error))
            }
            Some(JobTopicType::Invalid) => Err(JobError::InvalidTopic),
        }
    }
}
