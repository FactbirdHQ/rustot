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

use core::ops::{Deref, DerefMut};

use serde_cbor;

use super::cbor::{Bitmap, GetStreamRequest, GetStreamResponse};
use super::pal::{OtaEvent, OtaPal, OtaPalError};
use crate::consts::MAX_STREAM_ID_LEN;
use crate::jobs::{FileDescription, IotJobsData, JobError, JobStatus, OtaJob};
use heapless::{String, Vec};
use mqttrust::{Mqtt, MqttError, PublishNotification, QoS, SubscribeTopic};

use embedded_hal::timer::CountDown;

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
    CborData(String<MAX_STREAM_ID_LEN>),
    CborGetRejected(String<MAX_STREAM_ID_LEN>),
    Invalid,
}

impl OtaTopicType {
    /// Checks if a given topic path is a valid `Jobs` topic
    ///
    /// Example:
    /// ```
    /// use heapless::{String, Vec};
    ///
    /// let topic_path: String<128> =
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
        let topic_tokens = topic_name.splitn(8, '/').collect::<Vec<&str, 8>>();

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
    Mqtt,
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

impl<PE> From<MqttError> for OtaError<PE> {
    fn from(_e: MqttError) -> Self {
        OtaError::Mqtt
    }
}

impl<PE> From<JobError> for OtaError<PE> {
    fn from(e: JobError) -> Self {
        OtaError::Jobs(e)
    }
}

pub struct OtaConfig {
    block_size: usize,
    max_request_momentum: u8,
    request_wait_ms: u32,
    max_blocks_per_request: u32,
    status_update_frequency: u32,
}

impl Default for OtaConfig {
    fn default() -> Self {
        OtaConfig {
            block_size: 256,
            max_request_momentum: 3,
            request_wait_ms: 4500,
            max_blocks_per_request: 128,
            status_update_frequency: 24,
        }
    }
}

impl OtaConfig {
    pub fn set_block_size(self, block_size: usize) -> Self {
        Self { block_size, ..self }
    }

    pub fn set_max_request_momentum(self, max_request_momentum: u8) -> Self {
        Self {
            max_request_momentum,
            ..self
        }
    }

    pub fn set_request_wait_ms(self, request_wait_ms: u32) -> Self {
        Self {
            request_wait_ms,
            ..self
        }
    }

    pub fn set_max_blocks_per_request(self, max_blocks_per_request: u32) -> Self {
        assert!(max_blocks_per_request < 32);

        Self {
            max_blocks_per_request,
            ..self
        }
    }

    pub fn set_status_update_frequency(self, status_update_frequency: u32) -> Self {
        Self {
            status_update_frequency,
            ..self
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct OtaState {
    file: FileDescription,
    block_offset: u32,
    bitmap: Bitmap,
    stream_name: String<MAX_STREAM_ID_LEN>,
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
}

pub fn is_ota_message(topic_name: &str) -> bool {
    let topic_tokens = topic_name.splitn(8, '/').collect::<Vec<&str, 8>>();
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
        }
    }

    pub fn is_active(&self) -> bool {
        self.agent_state != AgentState::Ready
    }

    pub fn close<M: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl Mqtt<M>,
    ) -> Result<(), OtaError<P::Error>> {
        match self.agent_state {
            AgentState::Ready => Ok(()),
            AgentState::Active(ref state) => {
                // Unsubscribe from stream topic
                let mut topic_path = String::new();
                ufmt::uwrite!(
                    &mut topic_path,
                    "$aws/things/{}/streams/{}/data/cbor",
                    client.client_id(),
                    state.stream_name.as_str(),
                )
                .map_err(|_| OtaError::Formatting)?;

                client
                    .unsubscribe(Vec::from_slice(&[topic_path]).map_err(|_| OtaError::Memory)?)
                    .map_err(|_| OtaError::Mqtt)?;

                self.ota_pal.abort(&state.file)?;

                // Reset agent state
                self.agent_state = AgentState::Ready;
                Ok(())
            }
        }
    }

    // Call this from timer timeout IRQ or poll it regularly
    pub fn request_timer_irq<M: mqttrust::PublishPayload>(&mut self, client: &mut impl Mqtt<M>) {
        if self.request_timer.try_wait().is_ok() {
            if let AgentState::Active(ref mut state) = self.agent_state {
                if state.total_blocks_remaining > 0 {
                    state.request_block_remaining = self.config.max_blocks_per_request;
                    self.publish_get_stream_message(client).ok();
                }
            }
        }
    }

    pub fn handle_message<M: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl Mqtt<M>,
        job_agent: &mut impl IotJobsData,
        publish: &mut PublishNotification,
    ) -> Result<(), OtaError<P::Error>> {
        match self.agent_state {
            AgentState::Active(ref mut state) => {
                match OtaTopicType::check(client.client_id(), &publish.topic_name) {
                    None | Some(OtaTopicType::Invalid) => Err(OtaError::InvalidTopic),
                    Some(OtaTopicType::CborData(_)) => {
                        // Reset or start the firmware request timer.
                        self.request_timer
                            .try_start(self.config.request_wait_ms)
                            .ok();

                        let GetStreamResponse {
                            block_id,
                            block_payload,
                            block_size,
                            file_id,
                            ..
                        } = serde_cbor::de::from_mut_slice(&mut publish.payload).map_err(|_e| {
                            defmt::error!("CBOR decoding error");
                            OtaError::BadData
                        })?;

                        if state.file.fileid != file_id {
                            return Err(OtaError::BadFileHandle);
                        }

                        let total_blocks = (state.file.filesize + self.config.block_size - 1)
                            / self.config.block_size;
                        let last_block_id = total_blocks - 1;

                        let received_blocks =
                            (total_blocks - state.total_blocks_remaining + 1) as u32;

                        if (block_id < last_block_id && block_size == self.config.block_size)
                            || (block_id == last_block_id
                                && block_size
                                    == (state.file.filesize
                                        - last_block_id * self.config.block_size))
                        {
                            // We're actively receiving a file so update the
                            // job status as needed. First reset the
                            // momentum counter since we received a good
                            // block.
                            state.request_momentum = 0;

                            if !state
                                .bitmap
                                .deref()
                                .get(block_id - state.block_offset as usize)
                            {
                                defmt::warn!(
                                    "Block {:?} is a DUPLICATE. {:?} blocks remaining.",
                                    block_id,
                                    state.total_blocks_remaining
                                );

                                // Just return same progress as before
                                return Ok(());
                            } else {
                                self.ota_pal.write_block(
                                    &state.file,
                                    block_id * self.config.block_size,
                                    block_payload,
                                )?;
                                state
                                    .bitmap
                                    .deref_mut()
                                    .set(block_id - state.block_offset as usize, false);
                                state.total_blocks_remaining -= 1;
                            }
                        } else {
                            defmt::error!(
                                "Error! Block {:?} out of expected range! Size {:?}",
                                block_id,
                                block_size
                            );
                            return Err(OtaError::BlockOutOfRange);
                        }

                        if state.total_blocks_remaining == 0 {
                            defmt::info!("Received final expected block of file.");

                            match self.ota_pal.close_file(&state.file) {
                                Ok(()) => {
                                    let mut progress = String::new();
                                    ufmt::uwrite!(
                                        &mut progress,
                                        "{}/{}",
                                        received_blocks,
                                        total_blocks
                                    )
                                    .map_err(|_| JobError::Formatting)?;

                                    defmt::debug!(
                                        "File receive complete and signature is valid. {=str}",
                                        progress.as_str()
                                    );

                                    let mut map = heapless::IndexMap::new();
                                    map.insert(String::from("progress"), progress).ok();

                                    // Update job status to success with 100% progress
                                    // TODO: Update progress, but keep status as InProgress, until the firmware has been successfully booted.
                                    // Also consider OTA self test flow as part of status updates.
                                    job_agent.update_job_execution(
                                        client,
                                        JobStatus::Succeeded,
                                        Some(map),
                                    )?;
                                }
                                Err(_e) => {
                                    // Update job status to failed with reason `e`
                                    job_agent.update_job_execution(
                                        client,
                                        JobStatus::Failed,
                                        None,
                                    )?;
                                }
                            };
                        } else {
                            defmt::trace!(
                                "Received file block {:?}, size {:?}, remaining: {:?}",
                                block_id,
                                block_size,
                                total_blocks as u32 - received_blocks
                            );
                            if (received_blocks - 1) % self.config.status_update_frequency as u32
                                == 0
                            {
                                let mut progress = String::new();
                                ufmt::uwrite!(
                                    &mut progress,
                                    "{}/{}",
                                    received_blocks,
                                    total_blocks
                                )
                                .map_err(|_| JobError::Formatting)?;

                                defmt::info!("OTA Progress: {=str}", progress.as_str());

                                let mut map = heapless::IndexMap::new();
                                map.insert(String::from("progress"), progress).ok();

                                job_agent.update_job_execution(
                                    client,
                                    JobStatus::InProgress,
                                    Some(map),
                                )?;
                            }

                            if state.request_block_remaining > 1 {
                                state.request_block_remaining -= 1;
                            } else {
                                // Received number of data blocks requested so restart the request timer, and start a new bitmap
                                state.block_offset += self.config.max_blocks_per_request;
                                state.bitmap = Bitmap::new(
                                    state.file.filesize,
                                    self.config.block_size,
                                    state.block_offset,
                                );
                                state.request_block_remaining = core::cmp::min(
                                    self.config.max_blocks_per_request,
                                    state.total_blocks_remaining as u32,
                                );
                                self.publish_get_stream_message(client)?;
                            }
                        }
                        Ok(())
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

    pub fn finalize_ota_job<M: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl Mqtt<M>,
        status: JobStatus,
    ) -> Result<(), OtaError<P::Error>> {
        let event = match status {
            JobStatus::Succeeded => OtaEvent::Activate,
            _ => OtaEvent::Fail,
        };
        self.close(client)?;
        // TODO: Somehow allow time for mqtt messages to get sent!
        Ok(self.ota_pal.complete_callback(event)?)
    }

    pub fn process_ota_job<M: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl Mqtt<M>,
        job: &OtaJob,
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

        defmt::debug!("Accepted a new JOB! {=str}", job.streamname.as_str());

        let mut topic_path = String::new();
        ufmt::uwrite!(
            &mut topic_path,
            "$aws/things/{}/streams/{}/data/cbor",
            client.client_id(),
            job.streamname.as_str(),
        )
        .map_err(|_| OtaError::Formatting)?;

        let mut topics = Vec::new();
        topics
            .push(SubscribeTopic {
                topic_path,
                qos: QoS::AtMostOnce,
            })
            .map_err(|_| OtaError::Memory)?;

        client.subscribe(topics).map_err(|_| OtaError::Mqtt)?;

        // Publish to `$aws/things/{thingName}/streams/{streamId}/get/cbor` to
        // initiate the stream transfer
        let file = job.files.get(0).unwrap().clone();
        let block_offset = 0;
        let bitmap = Bitmap::new(file.filesize, self.config.block_size, block_offset);

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
                stream_name: job.streamname.clone(),
                block_offset,
                bitmap,
                total_blocks_remaining,
                request_block_remaining: number_of_blocks,
                request_momentum: 0,
            });

            self.publish_get_stream_message(client)
        }
    }

    fn publish_get_stream_message<M: mqttrust::PublishPayload>(
        &mut self,
        client: &mut impl Mqtt<M>,
    ) -> Result<(), OtaError<P::Error>> {
        if let AgentState::Active(ref mut state) = self.agent_state {
            if state.request_momentum >= self.config.max_request_momentum {
                return Err(OtaError::MaxMomentumAbort(self.config.max_request_momentum));
            }

            let buf: &mut [u8] = &mut [0u8; 32];
            let len = crate::ota::cbor::to_slice(
                &GetStreamRequest {
                    // Arbitrary client token sent in the stream "GET" message
                    client_token: None,
                    stream_version: None,
                    file_id: state.file.fileid,
                    block_size: self.config.block_size,
                    block_offset: Some(state.block_offset),
                    block_bitmap: Some(&state.bitmap),
                    number_of_blocks: None,
                },
                buf,
            )
            .map_err(|_| OtaError::BadData)?;

            let mut topic = String::new();
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
                .publish(topic, M::from_bytes(&buf[..len]), QoS::AtMostOnce)
                .ok();

            defmt::info!(
                "Requesting blocks! Momentum: {:?}, using {:?} bytes",
                state.request_momentum,
                len
            );
            // Start request timer
            self.request_timer
                .try_start(self.config.request_wait_ms)
                .ok();

            Ok(())
        } else {
            Err(OtaError::NoOtaActive)
        }
    }
}
