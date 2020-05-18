//! ## Over-the-air (OTA) flashing of firmware
//!
//! AWS IoT OTA works by using AWS IoT Jobs to manage firmware transfer and
//! status reporting of OTA.
//!
//! The OTA Jobs API makes use of the following special MQTT Topics:
//! - $aws/things/{thing_name}/jobs/$next/get/accepted
//! - $aws/things/{thing_name}/jobs/notify-next
//! - $aws/things/{thing_name}/jobs/$next/get
//! - $aws/things/{thing_name}/jobs/{job_id}/update
//! - $aws/things/{thing_name}/streams/{stream_id}/data/cbor
//! - $aws/things/{thing_name}/streams/{stream_id}/get/cbor
//!
//! Most of the data structures for the Jobs API has been copied from Rusoto:
//! https://docs.rs/rusoto_iot_jobs_data/0.43.0/rusoto_iot_jobs_data/
//!
//! ### OTA Flow:
//! 1. Device subscribes to notification topics for AWS IoT jobs and listens for
//!    update messages.
//! 2. When an update is available, the OTA agent publishes requests to AWS IoT
//!    and receives updates using the HTTP or MQTT protocol, depending on the
//!    settings you chose.
//! 3. The OTA agent checks the digital signature of the downloaded files and,
//!    if the files are valid, installs the firmware update to the appropriate
//!    flash bank.
//!
//! The OTA depends on working, and correctly setup:
//! - Bootloader
//! - MQTT Client
//! - Code sign verification
//! - CBOR deserializer
//! - Firmware writer (see
//!   https://gist.github.com/korken89/947d6458f34ca3ea7293dc06c2251078 for C++
//!   example with WFBootloader)

use serde_cbor;

use alloc::string::ToString;

use super::cbor::{Bitmap, StreamRequest, StreamResponse};
use super::pal::{OtaEvent, OtaPal, OtaPalError};
use crate::consts::MaxStreamIdLen;
use crate::jobs::{vec_to_vec, FileDescription, IotJobsData, JobError, JobStatus, OtaJob};
use heapless::{consts, String, Vec};
use mqttrust::{Mqtt, MqttClientError, Publish, QoS, SubscribeTopic};

use embedded_hal::timer::CountDown;

// #[derive(Default)]
// pub struct Statistics {
//     packets_received: u32,
//     packets_dropped: u32,
//     packets_queued: u32,
//     packets_processed: u32,
//     publish_failures: u32,
// }

// impl Statistics {
//     pub fn new() -> Self {
//         Statistics::default()
//     }
// }

#[derive(Clone, PartialEq)]
pub enum AgentState {
    Ready,
    Active(OtaState),
}

pub enum ImageState {
    Unknown,
    Aborted,
    Rejected,
    Accepted,
    Testing,
}

enum OtaTopicType {
    CborData(String<MaxStreamIdLen>),
    CborGetRejected(String<MaxStreamIdLen>),
    Invalid,
}

impl OtaTopicType {
    /// Checks if a given topic path is a valid `Jobs` topic
    ///
    /// Example:
    /// ```
    /// use heapless::{String, Vec, consts};
    ///
    /// let topic_path: String<consts::U128> =
    ///     String::from("$aws/things/SomeThingName/streams/AFR_OTA-fc149b0e-cb4e-456a-8cfb-7f0998a2855a/data/cbor");
    ///
    /// assert!(OtaTopicType::check("SomeThingName", &topic_path).is_some());
    /// assert_eq!(
    ///     OtaTopicType::check("SomeThingName", &topic_path),
    ///     Some(OtaTopicType::CborData(String::from("AFR_OTA-fc149b0e-cb4e-456a-8cfb-7f0998a2855a")))
    /// );
    /// ```
    pub fn check(expected_thing_name: &str, topic_name: &str) -> Option<Self> {
        if !is_ota_message(topic_name) {
            return None;
        }

        // Use the first 7
        // ($aws/things/{thingName}/jobs/$next/get/accepted), leaving
        // tokens 8+ at index 7
        let topic_tokens = topic_name.splitn(8, '/').collect::<Vec<&str, consts::U8>>();

        if topic_tokens.get(2) != Some(&expected_thing_name) {
            return None;
        }

        Some(match topic_tokens.get(4) {
            Some(stream_id) => match topic_tokens.get(5) {
                Some(&"data") if topic_tokens.get(6) == Some(&"cbor") => {
                    OtaTopicType::CborData(String::from(*stream_id))
                }
                Some(&"get")
                    if topic_tokens.get(6) == Some(&"cbor")
                        && topic_tokens.get(7) == Some(&"rejected") =>
                {
                    OtaTopicType::CborGetRejected(String::from(*stream_id))
                }
                Some(_) | None => OtaTopicType::Invalid,
            },
            None => OtaTopicType::Invalid,
        })
    }
}

#[derive(Debug)]
pub enum OtaError<PalError> {
    Mqtt(MqttClientError),
    Jobs(JobError),
    BadData,
    Memory,
    Formatting,
    InvalidTopic,
    NoOtaActive,
    BlockOutOfRange,
    PalError(OtaPalError<PalError>),
    BadFileHandle,
    RequestRejected,
    Busy,
    MaxMomentumAbort(u8),
}

impl<PE> From<core::fmt::Error> for OtaError<PE> {
    fn from(_e: core::fmt::Error) -> Self {
        OtaError::Formatting
    }
}

impl<PE> From<OtaPalError<PE>> for OtaError<PE> {
    fn from(e: OtaPalError<PE>) -> Self {
        OtaError::PalError(e)
    }
}

impl<PE> From<MqttClientError> for OtaError<PE> {
    fn from(e: MqttClientError) -> Self {
        OtaError::Mqtt(e)
    }
}

impl<PE> From<JobError> for OtaError<PE> {
    fn from(e: JobError) -> Self {
        OtaError::Jobs(e)
    }
}

pub struct OtaConfig {
    pub block_size: usize,
    pub max_request_momentum: u8,
    pub request_wait_ms: u32,
    pub max_blocks_per_request: u32,
    pub percentage_change_between_status_update: u8,
}

impl Default for OtaConfig {
    fn default() -> Self {
        OtaConfig {
            block_size: 1024,
            max_request_momentum: 3,
            request_wait_ms: 4500,
            max_blocks_per_request: 128,
            percentage_change_between_status_update: 5,
        }
    }
}

impl OtaConfig {
    pub fn set_block_size(self, block_size: usize) -> Self {
        Self { block_size, ..self }
    }

    pub fn set_request_wait_ms(self, request_wait_ms: u32) -> Self {
        Self {
            request_wait_ms,
            ..self
        }
    }

    pub fn set_max_blocks_per_request(self, max_blocks_per_request: u32) -> Self {
        Self {
            max_blocks_per_request,
            ..self
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct OtaState {
    file: FileDescription,
    bitmap: Bitmap,
    stream_name: String<MaxStreamIdLen>,
    total_blocks_remaining: usize,
    request_block_remaining: u32,
    request_momentum: u8,
}

pub struct OtaAgent<P, T> {
    config: OtaConfig,
    ota_pal: P,
    request_timer: T,
    agent_state: AgentState,
    pub image_state: ImageState,
    next_update_percentage: u8,
}

pub fn is_ota_message(topic_name: &str) -> bool {
    let topic_tokens = topic_name.splitn(8, '/').collect::<Vec<&str, consts::U8>>();
    topic_tokens.get(0) == Some(&"$aws")
        && topic_tokens.get(1) == Some(&"things")
        && topic_tokens.get(3) == Some(&"streams")
}

impl<P, T> OtaAgent<P, T>
where
    P: OtaPal,
    T: CountDown,
    T::Time: From<u32>,
{
    pub fn new(ota_pal: P, request_timer: T, config: OtaConfig) -> Self {
        OtaAgent {
            config,
            ota_pal,
            request_timer,
            agent_state: AgentState::Ready,
            image_state: ImageState::Unknown,
            next_update_percentage: 0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.agent_state != AgentState::Ready
    }

    pub fn abort(&mut self) {
        self.agent_state = AgentState::Ready;
    }

    // Call this from timer timeout IRQ or poll it regularly
    pub fn request_timer_irq(&mut self, client: &impl Mqtt) {
        if self.request_timer.wait().is_ok() {
            if let AgentState::Active(ref mut state) = self.agent_state {
                if state.total_blocks_remaining > 0 {
                    state.request_block_remaining = self.config.max_blocks_per_request;
                    self.publish_get_stream_message(client).ok();
                }
            }
        }
    }

    pub fn handle_message(
        &mut self,
        client: &impl Mqtt,
        job_agent: &mut impl IotJobsData,
        publish: &Publish,
    ) -> Result<u8, OtaError<P::Error>> {
        match self.agent_state {
            AgentState::Active(ref mut state) => {
                match OtaTopicType::check(client.client_id(), &publish.topic_name) {
                    None | Some(OtaTopicType::Invalid) => Err(OtaError::InvalidTopic),
                    Some(OtaTopicType::CborData(_)) => {
                        // Reset or start the firmware request timer.
                        self.request_timer.start(self.config.request_wait_ms);

                        let StreamResponse {
                            block_id,
                            block_payload,
                            block_size,
                            file_id,
                        } = serde_cbor::de::from_slice(&publish.payload).map_err(|e| {
                            log::error!("{:?}", e);
                            OtaError::BadData
                        })?;

                        if state.file.fileid != file_id {
                            return Err(OtaError::BadFileHandle);
                        }

                        let total_blocks = ((state.file.filesize + self.config.block_size - 1)
                            / self.config.block_size)
                            - 1;

                        if (block_id < total_blocks && block_size == self.config.block_size)
                            || (block_id == total_blocks
                                && block_size
                                    == (state.file.filesize
                                        - total_blocks * self.config.block_size))
                        {
                            // log::info!("Received file block {}, size {}", block_id, block_size);

                            // We're actively receiving a file so update the
                            // job status as needed. First reset the
                            // momentum counter since we received a good
                            // block.
                            state.request_momentum = 0;

                            if !state.bitmap.to_inner().get(block_id) {
                                log::warn!(
                                    "Block {} is a DUPLICATE. {} blocks remaining.",
                                    block_id,
                                    state.total_blocks_remaining
                                );

                                // Just return same progress as before
                                return Ok((((total_blocks - state.total_blocks_remaining) * 100)
                                    / total_blocks)
                                    as u8);
                            } else {
                                self.ota_pal.write_block(
                                    &state.file,
                                    block_id * self.config.block_size,
                                    block_payload,
                                )?;
                                state.bitmap.to_inner_mut().set(block_id, false);
                                state.total_blocks_remaining -= 1;
                            }
                        } else {
                            log::error!(
                                "Error! Block {} out of expected range! Size {}",
                                block_id,
                                block_size
                            );
                            return Err(OtaError::BlockOutOfRange);
                        }

                        let blocks_remaining = state.total_blocks_remaining;
                        let progress =
                            (((total_blocks - blocks_remaining) * 100) / total_blocks) as u8;

                        if blocks_remaining == 0 {
                            log::info!("Received final expected block of file.");

                            let event = match self.ota_pal.close_file(&state.file) {
                                Ok(()) => {
                                    log::info!("File receive complete and signature is valid.");
                                    // Update job status to success with 100% progress
                                    job_agent.update_job_execution(client, JobStatus::Succeeded)?;
                                    OtaEvent::Activate
                                }
                                Err(_e) => {
                                    // Update job status to failed with reason `e`
                                    job_agent.update_job_execution(client, JobStatus::Failed)?;

                                    OtaEvent::Fail
                                }
                            };

                            // Cleanup, unsubscribe and cleanup file contexts
                            // self.close().ok();

                            self.ota_pal.complete_callback(event)?;
                        } else {
                            // log::info!("Remaining: {}", state.total_blocks_remaining);

                            if progress == self.next_update_percentage {
                                // TODO: Include progress here
                                job_agent.update_job_execution(client, JobStatus::InProgress)?;
                                self.next_update_percentage +=
                                    self.config.percentage_change_between_status_update;
                            }

                            if state.request_block_remaining > 1 {
                                state.request_block_remaining -= 1;
                            } else {
                                // Received number of data blocks requested so restart the request timer
                                state.request_block_remaining = self.config.max_blocks_per_request;
                                self.publish_get_stream_message(client)?;
                            }
                        }
                        Ok(progress)
                    }
                    Some(OtaTopicType::CborGetRejected(_)) => {
                        self.agent_state = AgentState::Ready;
                        Err(OtaError::RequestRejected)
                    }
                }
            }
            AgentState::Ready => Err(OtaError::NoOtaActive),
        }
    }

    pub fn process_ota_job(
        &mut self,
        client: &impl Mqtt,
        job: OtaJob,
    ) -> Result<(), OtaError<P::Error>> {
        if let AgentState::Active(OtaState {
            ref stream_name, ..
        }) = self.agent_state
        {
            // Already have an OTA in progress
            if stream_name.as_str() == job.streamname.as_str() {
                return Ok(());
            } else {
                // We dont handle parallel OTA jobs!
                return Err(OtaError::Busy);
            }
        }
        // Subscribe to `$aws/things/{thingName}/streams/{streamId}/data/cbor`

        log::debug!("Accepted a new JOB! {:?}", job);

        let mut topic_path = String::<consts::U256>::new();
        ufmt::uwrite!(
            &mut topic_path,
            "$aws/things/{}/streams/{}/data/cbor",
            client.client_id(),
            job.streamname.as_str(),
        )
        .map_err(|_| OtaError::Formatting)?;

        let mut topics = Vec::<SubscribeTopic, consts::U1>::new();
        topics
            .push(SubscribeTopic {
                topic_path: topic_path.to_string(),
                qos: QoS::AtMostOnce,
            })
            .map_err(|_| OtaError::Memory)?;

        client.subscribe(topics)?;

        // Publish to `$aws/things/{thingName}/streams/{streamId}/get/cbor` to
        // initiate the stream transfer
        let file = job.files.get(0).unwrap().clone();
        log::info!("{:?}", file);
        let bitmap = Bitmap::new(file.filesize, self.config.block_size);

        if let Err(e) = self.ota_pal.create_file_for_rx(&file) {
            // Close Ota Context
            self.agent_state = AgentState::Ready;
            Err(e.into())
        } else {
            let number_of_blocks = core::cmp::min(
                self.config.max_blocks_per_request,
                128 * 1024 / self.config.block_size as u32,
            );

            let total_blocks_remaining =
                (file.filesize + self.config.block_size - 1) / self.config.block_size;

            self.agent_state = AgentState::Active(OtaState {
                file,
                stream_name: job.streamname,
                bitmap,
                total_blocks_remaining,
                request_block_remaining: number_of_blocks,
                request_momentum: 0,
            });

            self.publish_get_stream_message(client)
        }
    }

    fn publish_get_stream_message(&mut self, client: &impl Mqtt) -> Result<(), OtaError<P::Error>> {
        if let AgentState::Active(ref mut state) = self.agent_state {
            if state.request_momentum >= self.config.max_request_momentum {
                return Err(OtaError::MaxMomentumAbort(self.config.max_request_momentum));
            }

            let payload = serde_cbor::ser::to_vec(&StreamRequest {
                // Arbitrary client token sent in the stream "GET" message
                client_token: "rdy",
                file_id: state.file.fileid,
                block_size: self.config.block_size,
                block_offset: 0,
                block_bitmap: &state.bitmap,
                number_of_blocks: state.request_block_remaining,
            })
            .map_err(|_| OtaError::BadData)?;

            let mut topic: String<consts::U256> = String::new();
            ufmt::uwrite!(
                &mut topic,
                "$aws/things/{}/streams/{}/get/cbor",
                client.client_id(),
                state.stream_name.as_str(),
            )
            .map_err(|_| OtaError::Formatting)?;

            // Each Get Stream Request increases the momentum until a response
            // is received to ANY request. Too much momentum is interpreted as a
            // failure to communicate and will cause us to abort the OTA.
            state.request_momentum += 1;

            client
                .publish(
                    topic,
                    vec_to_vec::<_, heapless::consts::U384>(payload),
                    QoS::AtMostOnce,
                )
                .ok();

            log::info!("Requesting blocks! Momentum: {}", state.request_momentum);
            // Start request timer
            self.request_timer.start(self.config.request_wait_ms);

            Ok(())
        } else {
            Err(OtaError::NoOtaActive)
        }
    }
}
