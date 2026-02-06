use core::fmt::{Display, Write};
use core::ops::{Deref, DerefMut};
use core::str::FromStr;

use crate::mqtt::{Mqtt, MqttClient, MqttMessage, MqttSubscription, QoS};

use crate::jobs::JobTopic;
use crate::ota::error::OtaError;
use crate::ota::status_details::StatusDetailsExt;
use crate::ota::ProgressState;
use crate::{
    jobs::{MAX_STREAM_ID_LEN, MAX_THING_NAME_LEN},
    ota::{
        config::Config,
        data_interface::{DataInterface, FileBlock, Protocol},
        encoding::{cbor, FileContext},
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

struct MessagePayload<M>(M);

impl<M: MqttMessage> Deref for MessagePayload<M> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.0.payload()
    }
}

impl<M: MqttMessage> DerefMut for MessagePayload<M> {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.0.payload_mut()
    }
}

pub struct MqttTransfer<S>(S);

impl<S: MqttSubscription> BlockTransfer for MqttTransfer<S> {
    async fn next_block(&mut self) -> Result<Option<impl DerefMut<Target = [u8]>>, OtaError> {
        match self.0.next_message().await {
            Some(msg) => {
                if !msg.topic_name().contains("/streams/") {
                    return Err(OtaError::UserAbort);
                }
                Ok(Some(MessagePayload(msg)))
            }
            None => Ok(None),
        }
    }
}

impl<C: MqttClient> DataInterface for Mqtt<&'_ C> {
    const PROTOCOL: Protocol = Protocol::Mqtt;

    type ActiveTransfer<'t>
        = MqttTransfer<C::Subscription<'t, 2>>
    where
        Self: 't;

    /// Init file transfer by subscribing to the OTA data stream topic
    /// and the jobs notify-next topic (to detect cancellation).
    async fn init_file_transfer(
        &self,
        file_ctx: &FileContext,
    ) -> Result<Self::ActiveTransfer<'_>, OtaError> {
        let data_topic = OtaTopic::Data(Encoding::Cbor, file_ctx.stream_name.as_str())
            .format::<256>(self.0.client_id())?;

        let notify_topic: heapless::String<256> = JobTopic::NotifyNext
            .format::<256>(self.0.client_id())
            .map_err(|_| OtaError::Overflow)?;

        debug!(
            "Subscribing to: [{:?}] and [{:?}]",
            &data_topic, &notify_topic
        );

        self.0
            .subscribe(&[
                (data_topic.as_str(), QoS::AtMostOnce),
                (notify_topic.as_str(), QoS::AtMostOnce),
            ])
            .await
            .map(MqttTransfer)
            .map_err(|_| OtaError::Mqtt)
    }

    /// Request file block by publishing to the get stream topic
    async fn request_file_blocks<E: StatusDetailsExt>(
        &self,
        file_ctx: &FileContext,
        progress_state: &mut ProgressState<E>,
        config: &Config,
    ) -> Result<(), OtaError> {
        let blocks_available = progress_state.bitmap.len() as u32;
        let blocks_to_request = blocks_available.min(config.max_blocks_per_request);
        progress_state.request_block_remaining = blocks_to_request;

        let topic = OtaTopic::Get(Encoding::Cbor, file_ctx.stream_name.as_str()).format::<{
            MAX_STREAM_ID_LEN + MAX_THING_NAME_LEN + 30
        }>(
            self.0.client_id(),
        )?;

        let mut buf = [0u8; 256];
        let len = cbor::to_slice(
            &cbor::GetStreamRequest {
                client_token: None,
                stream_version: None,
                file_id: file_ctx.fileid,
                block_size: config.block_size,
                block_offset: Some(progress_state.block_offset),
                block_bitmap: Some(&progress_state.bitmap),
                number_of_blocks: Some(blocks_to_request),
            },
            &mut buf,
        )
        .map_err(|_| OtaError::Encoding)?;

        debug!(
            "Requesting {} file blocks (of {} remaining)",
            blocks_to_request, blocks_available
        );

        self.0
            .publish(topic.as_str(), &buf[..len])
            .await
            .map_err(|_| OtaError::Mqtt)
    }

    /// Decode a cbor encoded fileblock received from streaming service
    fn decode_file_block<'c>(&self, payload: &'c mut [u8]) -> Result<FileBlock<'c>, OtaError> {
        Ok(
            minicbor_serde::from_slice::<cbor::GetStreamResponse>(payload)
                .map_err(|_| OtaError::Encoding)?
                .into(),
        )
    }
}
