use core::fmt::{Display, Write};
use core::str::FromStr;

use embassy_time::Duration;

use crate::mqtt::{Mqtt, MqttClient, MqttMessage, MqttSubscription, PublishOptions, QoS};

use crate::jobs::JobTopic;
use crate::ota::error::OtaError;
use crate::ota::status_details::StatusDetailsExt;
use crate::{
    jobs::{MAX_STREAM_ID_LEN, MAX_THING_NAME_LEN},
    ota::{
        config::Config,
        data_interface::{BlockProgress, DataInterface, FileBlock, Protocol, RawBlock},
        encoding::{cbor, Bitmap, OtaJobContext},
    },
};

use super::BlockTransfer;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encoding {
    Cbor,
    Json,
}

impl Display for Encoding {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Encoding::Cbor => write!(f, "cbor"),
            Encoding::Json => write!(f, "json"),
        }
    }
}

impl FromStr for Encoding {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cbor" => Ok(Self::Cbor),
            "json" => Ok(Self::Json),
            _ => Err(()),
        }
    }
}

#[derive(Debug)]
pub enum Topic<'a> {
    Data(Encoding, &'a str),
    Description(Encoding, &'a str),
    Rejected(Encoding, &'a str),
}

impl<'a> Topic<'a> {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &'a str) -> Option<Self> {
        let tt = s.splitn(8, '/').collect::<heapless::Vec<&str, 8>>();
        Some(match (tt.first(), tt.get(1), tt.get(2), tt.get(3)) {
            (Some(&"$aws"), Some(&"things"), _, Some(&"streams")) => {
                // This is a stream topic! Figure out which
                match (tt.get(4), tt.get(5), tt.get(6), tt.get(7)) {
                    (Some(stream_name), Some(&"data"), Some(encoding), None) => {
                        Topic::Data(Encoding::from_str(encoding).ok()?, stream_name)
                    }
                    (Some(stream_name), Some(&"description"), Some(encoding), None) => {
                        Topic::Description(Encoding::from_str(encoding).ok()?, stream_name)
                    }
                    (Some(stream_name), Some(&"rejected"), Some(encoding), None) => {
                        Topic::Rejected(Encoding::from_str(encoding).ok()?, stream_name)
                    }
                    _ => return None,
                }
            }
            _ => return None,
        })
    }
}

impl<'a> From<&Topic<'a>> for OtaTopic<'a> {
    fn from(t: &Topic<'a>) -> Self {
        match t {
            Topic::Data(encoding, job_id) => Self::Data(*encoding, job_id),
            Topic::Description(encoding, job_id) => Self::Description(*encoding, job_id),
            Topic::Rejected(encoding, job_id) => Self::Rejected(*encoding, job_id),
        }
    }
}

enum OtaTopic<'a> {
    Data(Encoding, &'a str),
    Description(Encoding, &'a str),
    Rejected(Encoding, &'a str),

    Get(Encoding, &'a str),
}

impl OtaTopic<'_> {
    fn format_inner(&self, client_id: &str, w: &mut dyn Write) -> Result<(), core::fmt::Error> {
        match self {
            Self::Data(encoding, stream_name) => w.write_fmt(format_args!(
                "$aws/things/{}/streams/{}/data/{}",
                client_id, stream_name, encoding
            )),
            Self::Description(encoding, stream_name) => w.write_fmt(format_args!(
                "$aws/things/{}/streams/{}/description/{}",
                client_id, stream_name, encoding
            )),
            Self::Rejected(encoding, stream_name) => w.write_fmt(format_args!(
                "$aws/things/{}/streams/{}/rejected/{}",
                client_id, stream_name, encoding
            )),
            Self::Get(encoding, stream_name) => w.write_fmt(format_args!(
                "$aws/things/{}/streams/{}/get/{}",
                client_id, stream_name, encoding
            )),
        }
    }

    pub fn format<const L: usize>(&self, client_id: &str) -> Result<heapless::String<L>, OtaError> {
        let mut topic_path = heapless::String::new();
        self.format_inner(client_id, &mut topic_path)
            .map_err(|_| OtaError::Overflow)?;
        Ok(topic_path)
    }
}

/// Raw block wrapping an MQTT message. Decoding performs in-place CBOR
/// deserialization — zero-copy, the [`FileBlock`] payload borrows directly
/// from the message buffer.
pub struct MqttRawBlock<M> {
    message: M,
}

impl<M: MqttMessage> RawBlock for MqttRawBlock<M> {
    fn decode(&mut self) -> Result<FileBlock<'_>, OtaError> {
        Ok(
            minicbor_serde::from_slice::<cbor::GetStreamResponse>(self.message.payload_mut())
                .map_err(|_| OtaError::Encoding)?
                .into(),
        )
    }
}

pub struct MqttTransfer<'t, S, C: MqttClient> {
    sub: S,
    client: &'t C,
    job_name: heapless::String<64>,
    stream_name: heapless::String<64>,
    file_id: u8,
    block_size: usize,
    // Batch tracking
    batch_remaining: u32,
    max_blocks_per_request: u32,
    // Momentum tracking
    request_wait: Duration,
    max_momentum: u8,
    momentum: u8,
    // Block request state (updated via on_block_written)
    bitmap: Bitmap,
    block_offset: u32,
}

impl<'t, S, C: MqttClient> MqttTransfer<'t, S, C> {
    /// Publish a CBOR-encoded GetStreamRequest to request file blocks.
    async fn publish_block_request(&mut self) -> Result<(), OtaError> {
        let blocks_available = self.bitmap.len() as u32;
        let blocks_to_request = blocks_available.min(self.max_blocks_per_request);
        self.batch_remaining = blocks_to_request;

        let topic = OtaTopic::Get(Encoding::Cbor, &self.stream_name).format::<{
            MAX_STREAM_ID_LEN + MAX_THING_NAME_LEN + 30
        }>(
            self.client.client_id()
        )?;

        let mut buf = [0u8; 256];
        let len = cbor::to_slice(
            &cbor::GetStreamRequest {
                client_token: None,
                stream_version: None,
                file_id: self.file_id,
                block_size: self.block_size,
                block_offset: Some(self.block_offset),
                block_bitmap: Some(&self.bitmap),
                number_of_blocks: Some(blocks_to_request),
            },
            &mut buf,
        )
        .map_err(|_| OtaError::Encoding)?;

        debug!(
            "Requesting {} file blocks (of {} remaining)",
            blocks_to_request, blocks_available
        );

        self.client
            .publish_with_options(
                topic.as_str(),
                &buf[..len],
                PublishOptions::new().qos(QoS::AtMostOnce),
            )
            .await
            .map_err(|_| OtaError::Mqtt)
    }
}

impl<'t, S: MqttSubscription, C: MqttClient> BlockTransfer for MqttTransfer<'t, S, C> {
    type RawBlock<'b>
        = MqttRawBlock<S::Message<'b>>
    where
        Self: 'b;

    async fn next_block(&mut self) -> Result<Option<Self::RawBlock<'_>>, OtaError> {
        // Destructure for split borrows: the message branch borrows `sub`,
        // while the timer branch borrows `client` and other fields. Without
        // destructuring the compiler treats all field access as &mut self.
        //
        // No internal loop — if the timer fires (momentum), we return
        // Err(Momentum) and let the orchestrator retry. This avoids a GAT
        // lifetime conflict where the returned RawBlock (borrowing from sub)
        // would overlap with the next iteration's borrow of sub.
        let MqttTransfer {
            sub,
            client,
            job_name,
            stream_name,
            file_id,
            block_size,
            batch_remaining,
            max_blocks_per_request,
            request_wait,
            max_momentum,
            momentum,
            bitmap,
            block_offset,
        } = self;

        match embassy_futures::select::select(
            sub.next_message(),
            embassy_time::Timer::after(*request_wait),
        )
        .await
        {
            // Message received
            embassy_futures::select::Either::First(Some(msg)) => {
                if msg.topic_name().contains("/streams/") {
                    *momentum = 0;
                    return Ok(Some(MqttRawBlock { message: msg }));
                }

                // Non-stream message on the merged subscription (notify-next).
                // Only treat it as cancellation if our job name is absent from
                // the payload — that means execution is null (job gone) or a
                // different job replaced ours.
                if msg
                    .payload()
                    .windows(job_name.len())
                    .any(|w| w == job_name.as_bytes())
                {
                    debug!("Ignoring notify-next status update for current job");
                    Ok(None)
                } else {
                    Err(OtaError::UserAbort)
                }
            }

            // Subscription ended
            embassy_futures::select::Either::First(None) => Ok(None),

            // Timer expired — handle momentum and signal the orchestrator to retry
            embassy_futures::select::Either::Second(()) => {
                *momentum += 1;

                if *momentum <= 1 {
                    // Grace period — just retry
                    return Err(OtaError::Momentum);
                }

                if *momentum <= *max_momentum {
                    warn!("Momentum requesting more blocks!");

                    // Inline block request publish (can't call &mut self method
                    // because `sub` is destructured separately)
                    let blocks_available = bitmap.len() as u32;
                    let blocks_to_request = blocks_available.min(*max_blocks_per_request);
                    *batch_remaining = blocks_to_request;

                    let topic = OtaTopic::Get(Encoding::Cbor, stream_name).format::<{
                        MAX_STREAM_ID_LEN + MAX_THING_NAME_LEN + 30
                    }>(
                        client.client_id()
                    )?;

                    let mut buf = [0u8; 256];
                    let len = cbor::to_slice(
                        &cbor::GetStreamRequest {
                            client_token: None,
                            stream_version: None,
                            file_id: *file_id,
                            block_size: *block_size,
                            block_offset: Some(*block_offset),
                            block_bitmap: Some(bitmap),
                            number_of_blocks: Some(blocks_to_request),
                        },
                        &mut buf,
                    )
                    .map_err(|_| OtaError::Encoding)?;

                    debug!(
                        "Requesting {} file blocks (of {} remaining)",
                        blocks_to_request, blocks_available
                    );

                    client
                        .publish_with_options(
                            topic.as_str(),
                            &buf[..len],
                            PublishOptions::new().qos(QoS::AtMostOnce),
                        )
                        .await
                        .map_err(|_| OtaError::Mqtt)?;

                    Err(OtaError::Momentum)
                } else {
                    Err(OtaError::MomentumAbort)
                }
            }
        }
    }

    async fn on_block_written(&mut self, progress: &BlockProgress) -> Result<(), OtaError> {
        // Update internal state from orchestrator's progress
        self.bitmap = progress.bitmap.clone();
        self.block_offset = progress.block_offset;

        self.batch_remaining = self.batch_remaining.saturating_sub(1);
        if self.batch_remaining == 0 {
            self.publish_block_request().await?;
        }
        Ok(())
    }
}

impl<C: MqttClient> DataInterface for Mqtt<&'_ C> {
    const PROTOCOL: Protocol = Protocol::Mqtt;

    type Transfer<'t>
        = MqttTransfer<'t, C::Subscription<'t, 2>, C>
    where
        Self: 't;

    /// Begin a file transfer by subscribing to the OTA data stream topic
    /// and the jobs notify-next topic (to detect cancellation), then publishing
    /// the initial block request.
    async fn begin_transfer(
        &self,
        job: &OtaJobContext<'_, impl StatusDetailsExt>,
        config: &Config,
        progress: &BlockProgress,
    ) -> Result<Self::Transfer<'_>, OtaError> {
        let stream_name = job.stream_name.ok_or(OtaError::InvalidFile)?;

        let data_topic =
            OtaTopic::Data(Encoding::Cbor, stream_name).format::<256>(self.0.client_id())?;

        let notify_topic: heapless::String<256> = JobTopic::NotifyNext
            .format::<256>(self.0.client_id())
            .map_err(|_| OtaError::Overflow)?;

        debug!(
            "Subscribing to: [{:?}] and [{:?}]",
            &data_topic, &notify_topic
        );

        let sub = self
            .0
            .subscribe(&[
                (data_topic.as_str(), QoS::AtMostOnce),
                (notify_topic.as_str(), QoS::AtMostOnce),
            ])
            .await
            .map_err(|_| OtaError::Mqtt)?;

        let mut transfer = MqttTransfer {
            sub,
            client: self.0,
            job_name: heapless::String::try_from(job.job_name).map_err(|_| OtaError::Overflow)?,
            stream_name: heapless::String::try_from(stream_name).map_err(|_| OtaError::Overflow)?,
            file_id: job.fileid,
            block_size: config.block_size,
            batch_remaining: 0,
            max_blocks_per_request: config.max_blocks_per_request,
            request_wait: config.request_wait,
            max_momentum: config.max_request_momentum,
            momentum: 0,
            bitmap: progress.bitmap.clone(),
            block_offset: progress.block_offset,
        };

        // Publish initial block request
        transfer.publish_block_request().await?;

        Ok(transfer)
    }
}
