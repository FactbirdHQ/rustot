use core::fmt::{Display, Write};
use core::ops::DerefMut;
use core::str::FromStr;

use embassy_sync::blocking_mutex::raw::RawMutex;
use mqttrust::{
    DeferredPayload, EncodingError, MqttClient, Publish, Subscribe, SubscribeTopic, Subscription,
};

use crate::ota::error::OtaError;
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

impl<M: RawMutex> BlockTransfer for Subscription<'_, '_, M, 1> {
    async fn next_block(&mut self) -> Result<Option<impl DerefMut<Target = [u8]>>, OtaError> {
        let next = self.next_message().await;
        if next.is_none() {
            warn!("[OTA] Data stream ended (subscription closed due to clean session/disconnect)");
        }
        Ok(next)
    }
}

impl<'a, M: RawMutex> DataInterface for MqttClient<'a, M> {
    const PROTOCOL: Protocol = Protocol::Mqtt;

    type ActiveTransfer<'t>
        = Subscription<'a, 't, M, 1>
    where
        Self: 't;

    /// Init file transfer by subscribing to the OTA data stream topic
    async fn init_file_transfer(
        &self,
        file_ctx: &FileContext,
    ) -> Result<Self::ActiveTransfer<'_>, OtaError> {
        let topic_path = OtaTopic::Data(Encoding::Cbor, file_ctx.stream_name.as_str())
            .format::<256>(self.client_id())?;

        let topics = [SubscribeTopic::builder()
            .topic_path(topic_path.as_str())
            .build()];

        debug!("Subscribing to: [{:?}]", &topic_path);

        let sub = self
            .subscribe::<1>(Subscribe::builder().topics(&topics).build())
            .await?;

        info!(
            "[OTA] Subscribed to data stream {}",
            file_ctx.stream_name.as_str()
        );

        Ok(sub)
    }

    /// Request file block by publishing to the get stream topic
    async fn request_file_blocks(
        &self,
        file_ctx: &FileContext,
        progress_state: &mut ProgressState,
        config: &Config,
    ) -> Result<(), OtaError> {
        progress_state.request_block_remaining = progress_state.bitmap.len() as u32;

        let payload = DeferredPayload::new(
            |buf| {
                cbor::to_slice(
                    &cbor::GetStreamRequest {
                        // Arbitrary client token sent in the stream "GET" message
                        client_token: None,
                        stream_version: None,
                        file_id: file_ctx.fileid,
                        block_size: config.block_size,
                        block_offset: Some(progress_state.block_offset),
                        block_bitmap: Some(&progress_state.bitmap),
                        number_of_blocks: Some(progress_state.request_block_remaining),
                    },
                    buf,
                )
                .map_err(|_| EncodingError::BufferSize)
            },
            32,
        );

        debug!(
            "Requesting more file blocks. Remaining: {}",
            progress_state.request_block_remaining
        );
        info!(
            "[OTA] Requesting blocks stream={} offset={} bitmap_len={} blocks_remaining={}",
            file_ctx.stream_name.as_str(),
            progress_state.block_offset,
            progress_state.bitmap.len(),
            progress_state.blocks_remaining
        );
        self.publish(
            Publish::builder()
                .topic_name(
                    OtaTopic::Get(Encoding::Cbor, file_ctx.stream_name.as_str())
                        .format::<{ MAX_STREAM_ID_LEN + MAX_THING_NAME_LEN + 30 }>(
                            self.client_id(),
                        )?
                        .as_str(),
                )
                // .qos(mqttrust::QoS::AtMostOnce)
                .payload(payload)
                .build(),
        )
        .await?;

        Ok(())
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
