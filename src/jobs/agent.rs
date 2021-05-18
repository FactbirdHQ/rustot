use super::{
    DescribeJobExecutionRequest, DescribeJobExecutionResponse, ErrorResponse,
    GetPendingJobExecutionsRequest, IotJobsData, JobError, JobExecution, JobNotification,
    JobStatus, JobTopicType, NextJobExecutionChanged, StartNextPendingJobExecutionRequest,
    UpdateJobExecutionRequest, UpdateJobExecutionResponse,
};
use crate::consts::MAX_CLIENT_TOKEN_LEN;
use heapless::{String, Vec};

use serde_json_core::{from_slice, to_vec};

#[derive(Default)]
pub struct JobAgent {
    request_cnt: u32,
    max_retries: u8,
    active_job: Option<JobNotification>,
}

pub fn is_job_message(topic_name: &str) -> bool {
    let topic_tokens = topic_name.splitn(8, '/').collect::<Vec<&str, 8>>();
    topic_tokens.get(0) == Some(&"$aws")
        && topic_tokens.get(1) == Some(&"things")
        && topic_tokens.get(3) == Some(&"jobs")
}

impl JobAgent {
    /// Create a new IoT Job Agent reacting to topics for `thing_name`
    pub fn new(max_retries: u8) -> Self {
        JobAgent {
            request_cnt: 0,
            max_retries,
            active_job: None,
        }
    }

    /// Obtains a unique client token on the form `{requestNumber}:{thingName}`,
    /// and increments the request counter
    fn get_client_token(
        &mut self,
        thing_name: &str,
    ) -> Result<String<MAX_CLIENT_TOKEN_LEN>, JobError> {
        let mut client_token = String::new();
        ufmt::uwrite!(&mut client_token, "{}:{}", self.request_cnt, thing_name)
            .map_err(|_| JobError::Formatting)?;
        self.request_cnt += 1;
        Ok(client_token)
    }

    fn update_job_execution_internal<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        execution_number: Option<i64>,
        step_timeout_in_minutes: Option<i64>,
    ) -> Result<(), JobError> {
        let thing_name = client.client_id();
        let client_token = self.get_client_token(thing_name)?;

        if let Some(ref mut active_job) = self.active_job {
            let mut topic = String::new();
            ufmt::uwrite!(
                &mut topic,
                "$aws/things/{}/jobs/{}/update",
                thing_name,
                active_job.job_id.as_str()
            )
            .map_err(|_| JobError::Formatting)?;

            // Always include job_document, and job_execution_state!
            client
                .publish(
                    topic,
                    P::from_bytes(&to_vec::<_, 512>(&UpdateJobExecutionRequest {
                        execution_number,
                        expected_version: active_job.version_number,
                        include_job_document: Some(true),
                        include_job_execution_state: Some(true),
                        status: active_job.status,
                        status_details: active_job.status_details.as_ref(),
                        step_timeout_in_minutes,
                        client_token,
                    })?),
                    mqttrust::QoS::AtMostOnce,
                )
                .map_err(|_| JobError::Mqtt)?;

            active_job.version_number += 1;

            Ok(())
        } else {
            Err(JobError::NoActiveJob)
        }
    }

    fn handle_job_execution<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        execution: JobExecution,
        status_details: Option<
            heapless::FnvIndexMap<String<8>, String<10>, 4>,
        >,
    ) -> Result<Option<&JobNotification>, JobError> {
        match execution.status {
            JobStatus::Queued if self.active_job.is_none() && execution.job_document.is_some() => {
                // There is a new queued job available, and we are not currently
                // processing a job. Update the status to InProgress, and set it
                // active in the accepted response
                // (`$aws/things/{thingName}/jobs/{jobId}/update/accepted`).
                self.active_job = Some(JobNotification {
                    job_id: execution.job_id,
                    version_number: execution.version_number,
                    status: JobStatus::InProgress,
                    details: execution.job_document.unwrap(),
                    status_details,
                });
                defmt::debug!("Accepting new job!");

                self.update_job_execution_internal(client, None, None)?;

                Ok(None)
            }
            JobStatus::InProgress
                if self.active_job.is_none() && execution.job_document.is_some() =>
            {
                // If we dont have an active job, and the cloud reports job
                // should be active, it means something panicked, or we lost
                // track of the current job.

                // Start over current job, instead of failing it
                let mut map = execution
                    .status_details
                    .unwrap_or_else(|| heapless::IndexMap::new());

                let attempt = map
                    .get(&String::from("attempt"))
                    .and_then(|a| a.as_str().parse::<u8>().ok())
                    .unwrap_or_else(|| 1);

                let status = if attempt < self.max_retries {
                    defmt::debug!("Retrying existing job! Attempt: {}", attempt);

                    let mut attempt_str = String::new();
                    if ufmt::uwrite!(&mut attempt_str, "{}", attempt + 1).is_ok() {
                        map.insert(String::from("attempt"), attempt_str).ok();
                    }

                    map.insert(String::from("progress"), String::from("0/0"))
                        .ok();

                    for (k, v) in map.iter() {
                        defmt::debug!("{}: {}", k.as_str(), v.as_str());
                    }

                    JobStatus::InProgress
                } else {
                    defmt::error!(
                        "Retried job {} times without success! Failing it...",
                        self.max_retries
                    );
                    JobStatus::Failed
                };

                self.active_job = Some(JobNotification {
                    job_id: execution.job_id,
                    version_number: execution.version_number,
                    status: status.clone(),
                    details: execution.job_document.unwrap(),
                    status_details: Some(map),
                });

                self.update_job_execution_internal(client, None, None)?;

                if status == JobStatus::Failed {
                    self.active_job = None;
                    Ok(None)
                } else {
                    Ok(self.active_job.as_ref())
                }
            }
            JobStatus::InProgress => {
                // If we have an active job, and the cloud reports job should be
                // active, it means there is an update for the currently
                // executing job, perhaps requested by the device

                // Validate that the job update is indeed for the active_job
                match self.active_job {
                    Some(ref active_job) if execution.job_id != active_job.job_id => {
                        // TODO: We got a job notification for a new job
                    }
                    Some(_) => {
                        // We got a job notification for a the currently active job
                    }
                    None => {
                        defmt::error!("Active job is none! Should never happen!");
                    }
                }
                Ok(self.active_job.as_ref())
            }
            JobStatus::Canceled | JobStatus::Removed | JobStatus::Failed
                if self.active_job.is_some() =>
            {
                // TODO:
                // Current job is canceled! Abort if possible
                // let job = self.active_job.unwrap();
                // self.active_job = None;

                // Ok(Some(&JobNotification {
                //     status: JobStatus::Canceled,
                //     ..job
                // }))'
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

impl IotJobsData for JobAgent {
    fn describe_job_execution<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        job_id: &str,
        execution_number: Option<i64>,
        include_job_document: Option<bool>,
    ) -> Result<(), JobError> {
        let thing_name = client.client_id();

        let mut topic = String::new();
        ufmt::uwrite!(&mut topic, "$aws/things/{}/jobs/{}/get", thing_name, job_id)
            .map_err(|_| JobError::Formatting)?;

        // TODO: This should be possible to optimize, wrt. clones/copies and allocations
        let p = to_vec::<_, 128>(&DescribeJobExecutionRequest {
            execution_number,
            include_job_document,
            client_token: self.get_client_token(thing_name)?,
        })?;

        client
            .publish(topic, P::from_bytes(&p), mqttrust::QoS::AtLeastOnce)
            .map_err(|_| JobError::Mqtt)?;

        Ok(())
    }

    fn get_pending_job_executions<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
    ) -> Result<(), JobError> {
        let thing_name = client.client_id();
        let client_token = self.get_client_token(thing_name)?;

        let mut topic = String::new();
        ufmt::uwrite!(&mut topic, "$aws/things/{}/jobs/get", thing_name)
            .map_err(|_| JobError::Formatting)?;

        client
            .publish(
                topic,
                P::from_bytes(&to_vec::<_, 48>(
                    &GetPendingJobExecutionsRequest { client_token },
                )?),
                mqttrust::QoS::AtLeastOnce,
            )
            .map_err(|_| JobError::Mqtt)?;

        Ok(())
    }

    fn start_next_pending_job_execution<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        step_timeout_in_minutes: Option<i64>,
    ) -> Result<(), JobError> {
        let thing_name = client.client_id();
        let client_token = self.get_client_token(thing_name)?;

        let mut topic = String::new();
        ufmt::uwrite!(&mut topic, "$aws/things/{}/jobs/start-next", thing_name)
            .map_err(|_| JobError::Formatting)?;

        client
            .publish(
                topic,
                P::from_bytes(&to_vec::<_, 48>(
                    &StartNextPendingJobExecutionRequest {
                        step_timeout_in_minutes,
                        client_token,
                    },
                )?),
                mqttrust::QoS::AtLeastOnce,
            )
            .map_err(|_| JobError::Mqtt)?;

        Ok(())
    }

    fn update_job_execution<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        status: JobStatus,
        status_details: Option<
            heapless::FnvIndexMap<String<8>, String<10>, 4>,
        >,
    ) -> Result<(), JobError> {
        if let Some(ref mut active_job) = self.active_job {
            active_job.status = status;

            // Merge status_details
            match active_job.status_details {
                Some(ref mut active_details) => {
                    if let Some(new_details) = status_details {
                        new_details.iter().for_each(|(key, value)| {
                            active_details.insert(key.clone(), value.clone()).ok();
                        });
                    }
                }
                None => active_job.status_details = status_details,
            };
        }
        self.update_job_execution_internal(client, None, None)
    }

    fn subscribe_to_jobs<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
    ) -> Result<(), JobError> {
        let thing_name = client.client_id();
        let mut topics = Vec::new();

        let mut topic = String::new();
        ufmt::uwrite!(&mut topic, "$aws/things/{}/jobs/+/get/+", thing_name)
            .map_err(|_| JobError::Formatting)?;

        topics
            .push(mqttrust::SubscribeTopic {
                topic_path: topic,
                qos: mqttrust::QoS::AtLeastOnce,
            })
            .map_err(|_| JobError::Memory)?;

        let mut topic = String::new();
        ufmt::uwrite!(&mut topic, "$aws/things/{}/jobs/+/update/+", thing_name)
            .map_err(|_| JobError::Formatting)?;

        topics
            .push(mqttrust::SubscribeTopic {
                topic_path: topic,
                qos: mqttrust::QoS::AtLeastOnce,
            })
            .map_err(|_| JobError::Memory)?;

        let mut topic = String::new();
        ufmt::uwrite!(&mut topic, "$aws/things/{}/jobs/notify-next", thing_name)
            .map_err(|_| JobError::Formatting)?;

        topics
            .push(mqttrust::SubscribeTopic {
                topic_path: topic,
                qos: mqttrust::QoS::AtLeastOnce,
            })
            .map_err(|_| JobError::Memory)?;

        client.subscribe(topics).map_err(|_| JobError::Mqtt)?;
        Ok(())
    }

    fn unsubscribe_from_jobs<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
    ) -> Result<(), JobError> {
        let thing_name = client.client_id();

        let mut topics = Vec::new();

        let mut topic = String::new();
        ufmt::uwrite!(&mut topic, "$aws/things/{}/jobs/+/get/+", thing_name)
            .map_err(|_| JobError::Formatting)?;

        topics.push(topic).map_err(|_| JobError::Memory)?;

        let mut topic = String::new();
        ufmt::uwrite!(&mut topic, "$aws/things/{}/jobs/+/update/+", thing_name)
            .map_err(|_| JobError::Formatting)?;

        topics.push(topic).map_err(|_| JobError::Memory)?;

        let mut topic = String::new();
        ufmt::uwrite!(&mut topic, "$aws/things/{}/jobs/notify-next", thing_name)
            .map_err(|_| JobError::Formatting)?;

        topics.push(topic).map_err(|_| JobError::Memory)?;

        client.unsubscribe(topics).map_err(|_| JobError::Mqtt)?;
        Ok(())
    }

    fn handle_message<P: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl mqttrust::Mqtt<P>,
        publish: &mqttrust::PublishNotification,
    ) -> Result<Option<&JobNotification>, JobError> {
        match JobTopicType::check(
            client.client_id(),
            &publish
                .topic_name
                // Use the first 7
                // ($aws/things/{thingName}/jobs/$next/get/accepted), leaving
                // tokens 8+ at index 7
                .splitn(8, '/')
                .collect::<Vec<&str, 8>>(),
        ) {
            None => {
                defmt::debug!("Not a job message!");
                Ok(None)
            }
            Some(JobTopicType::NotifyNext) => {
                // Message published to
                // `$aws/things/{thingName}/jobs/notify-next`

                let (response, _) = from_slice::<NextJobExecutionChanged>(&publish.payload)?;
                defmt::debug!(
                    "notify-next message! active_job: {:?}",
                    self.active_job.is_some()
                );
                if let Some(execution) = response.execution {
                    // Job updated from the cloud!
                    self.handle_job_execution(client, execution, None)
                } else {
                    // Queue is empty! `jobs done`
                    Ok(None)
                }
            }
            Some(JobTopicType::Notify) => {
                // Message published to `$aws/things/{thingName}/jobs/notify`
                defmt::error!("notify message!, currently unhandled! Use notify-next instead");

                Ok(None)
            }
            Some(JobTopicType::GetAccepted(job_id)) => {
                // Message published to
                // `$aws/things/{thingName}/jobs/{jobId}/get/accepted`

                defmt::debug!("{=str}/get/accepted message!", job_id.as_str());
                if let Ok((response, _)) =
                    from_slice::<DescribeJobExecutionResponse>(&publish.payload)
                {
                    if let Some(execution) = response.execution {
                        self.handle_job_execution(client, execution, None)
                    } else {
                        Ok(None)
                    }
                } else {
                    defmt::error!("Unknown job document!");

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
                defmt::debug!("{=str}/update/accepted message!", job_id.as_str());

                match from_slice::<UpdateJobExecutionResponse>(&publish.payload) {
                    Ok((
                        UpdateJobExecutionResponse {
                            execution_state,
                            job_document,
                            ..
                        },
                        _,
                    )) if execution_state.is_some() && job_document.is_some() => {
                        let state = execution_state.unwrap();

                        let version_number = if let Some(ref active) = self.active_job {
                            if state.version_number > active.version_number {
                                state.version_number
                            } else {
                                active.version_number
                            }
                        } else {
                            state.version_number
                        };

                        match state.status {
                            JobStatus::Canceled | JobStatus::Removed | JobStatus::Failed => {
                                self.active_job = None;
                            }
                            _ => {
                                self.active_job = Some(JobNotification {
                                    job_id,
                                    version_number,
                                    status: state.status,
                                    details: job_document.unwrap(),
                                    status_details: state.status_details,
                                });
                            }
                        }
                        Ok(self.active_job.as_ref())
                    }
                    Ok(_) => {
                        // job_execution_state or job_document is missing, should never happen!
                        defmt::error!(
                            "job_execution_state or job_document is missing, should never happen!"
                        );
                        Ok(None)
                    }
                    Err(_) => Err(JobError::InvalidTopic),
                }
            }
            Some(JobTopicType::GetRejected(job_id)) => {
                // Message published to
                // `$aws/things/{thingName}/jobs/{jobId}/get/rejected`
                defmt::debug!("{=str}/get/rejected message!", job_id.as_str());
                let (error, _) = from_slice::<ErrorResponse>(&publish.payload)?;
                // defmt::debug!("{:?}", error);
                Err(JobError::Rejected(error))
            }
            Some(JobTopicType::UpdateRejected(job_id)) => {
                // Message published to
                // `$aws/things/{thingName}/jobs/{jobId}/update/rejected`
                defmt::debug!("{=str}/update/rejected message!", job_id.as_str());
                let (error, _) = from_slice::<ErrorResponse>(&publish.payload)?;
                // defmt::debug!("{:?}", error);
                Err(JobError::Rejected(error))
            }
            Some(JobTopicType::Invalid) => Err(JobError::InvalidTopic),
        }
    }
}
