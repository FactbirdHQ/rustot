use core::fmt::{Display, Write};
use core::str::FromStr;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_mqtt::{MqttClient, Properties, Publish, RetainHandling, Subscribe, SubscribeTopic};

use crate::ota::error::OtaError;
use crate::{
    jobs::{MAX_STREAM_ID_LEN, MAX_THING_NAME_LEN},
    ota::{
        config::Config,
        data_interface::{DataInterface, FileBlock, Protocol},
        encoding::{cbor, FileContext},
    },
};

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
        Some(match (tt.get(0), tt.get(1), tt.get(2), tt.get(3)) {
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

impl<'a> OtaTopic<'a> {
    pub fn format<const L: usize>(&self, client_id: &str) -> Result<heapless::String<L>, OtaError> {
        let mut topic_path = heapless::String::new();
        match self {
            Self::Data(encoding, stream_name) => topic_path.write_fmt(format_args!(
                "$aws/things/{}/streams/{}/data/{}",
                client_id, stream_name, encoding
            )),
            Self::Description(encoding, stream_name) => topic_path.write_fmt(format_args!(
                "$aws/things/{}/streams/{}/description/{}",
                client_id, stream_name, encoding
            )),
            Self::Rejected(encoding, stream_name) => topic_path.write_fmt(format_args!(
                "$aws/things/{}/streams/{}/rejected/{}",
                client_id, stream_name, encoding
            )),
            Self::Get(encoding, stream_name) => topic_path.write_fmt(format_args!(
                "$aws/things/{}/streams/{}/get/{}",
                client_id, stream_name, encoding
            )),
        }
        .map_err(|_| OtaError::Overflow)?;

        Ok(topic_path)
    }
}

impl<'a, M: RawMutex, const SUBS: usize> DataInterface for MqttClient<'a, M, SUBS> {
    const PROTOCOL: Protocol = Protocol::Mqtt;

    /// Init file transfer by subscribing to the OTA data stream topic
    async fn init_file_transfer(&self, file_ctx: &mut FileContext) -> Result<(), OtaError> {
        let topic_path = OtaTopic::Data(Encoding::Cbor, file_ctx.stream_name.as_str())
            .format::<256>(self.client_id())?;

        let topic = SubscribeTopic {
            topic_path: topic_path.as_str(),
            maximum_qos: embedded_mqtt::QoS::AtLeastOnce,
            no_local: false,
            retain_as_published: false,
            retain_handling: RetainHandling::SendAtSubscribeTime,
        };

        debug!("Subscribing to: [{:?}]", &topic_path);

        // FIXME:
        self.subscribe::<1>(Subscribe::new(&[topic])).await?;

        Ok(())
    }

    /// Request file block by publishing to the get stream topic
    async fn request_file_block(
        &self,
        file_ctx: &mut FileContext,
        config: &Config,
    ) -> Result<(), OtaError> {
        // Reset number of blocks requested
        file_ctx.request_block_remaining = file_ctx.bitmap.len() as u32;

        // FIXME: Serialize directly into the publish payload through `DeferredPublish` API
        let buf = &mut [0u8; 32];
        let len = cbor::to_slice(
            &cbor::GetStreamRequest {
                // Arbitrary client token sent in the stream "GET" message
                client_token: None,
                stream_version: None,
                file_id: file_ctx.fileid,
                block_size: config.block_size,
                block_offset: Some(file_ctx.block_offset),
                block_bitmap: Some(&file_ctx.bitmap),
                number_of_blocks: None,
            },
            buf,
        )
        .map_err(|_| OtaError::Encoding)?;

        self.publish(Publish {
            dup: false,
            qos: embedded_mqtt::QoS::AtMostOnce,
            retain: false,
            pid: None,
            topic_name: OtaTopic::Get(Encoding::Cbor, file_ctx.stream_name.as_str())
                .format::<{ MAX_STREAM_ID_LEN + MAX_THING_NAME_LEN + 30 }>(self.client_id())?
                .as_str(),
            payload: &buf[..len],
            properties: Properties::Slice(&[]),
        })
        .await?;

        Ok(())
    }

    /// Decode a cbor encoded fileblock received from streaming service
    async fn decode_file_block<'c>(
        &self,
        _file_ctx: &mut FileContext,
        payload: &'c mut [u8],
    ) -> Result<FileBlock<'c>, OtaError> {
        Ok(
            serde_cbor::de::from_mut_slice::<cbor::GetStreamResponse>(payload)
                .map_err(|_| OtaError::Encoding)?
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use mqttrust::{encoding::v4::decode_slice, Packet, SubscribeTopic};

    use super::*;
    use crate::{ota::test::test_file_ctx, test::MockMqtt};

    #[test]
    fn protocol_fits() {
        assert_eq!(<&MockMqtt as DataInterface>::PROTOCOL, Protocol::Mqtt);
    }

    #[test]
    fn init_file_transfer_subscribes() {
        let mqtt = &MockMqtt::new();

        let mut file_ctx = test_file_ctx(&Config::default());

        mqtt.init_file_transfer(&mut file_ctx).unwrap();

        assert_eq!(mqtt.tx.borrow_mut().len(), 1);

        let bytes = mqtt.tx.borrow_mut().pop_front().unwrap();

        let packet = decode_slice(bytes.as_slice()).unwrap();
        let topics = match packet {
            Some(Packet::Subscribe(ref s)) => s.topics().collect::<Vec<_>>(),
            _ => panic!(),
        };
        assert_eq!(
            topics,
            vec![SubscribeTopic {
                topic_path: "$aws/things/test_client/streams/test_stream/data/cbor",
                maximum_qos: QoS::AtLeastOnce,
                no_local: false,
                retain_as_published: false,
                retain_handling: RetainHandling::SendAtSubscribeTime
            }]
        );
    }

    #[test]
    fn request_file_block_publish() {
        let mqtt = &MockMqtt::new();

        let config = Config::default();
        let mut file_ctx = test_file_ctx(&config);

        mqtt.request_file_block(&mut file_ctx, &config).unwrap();

        assert_eq!(mqtt.tx.borrow_mut().len(), 1);

        let bytes = mqtt.tx.borrow_mut().pop_front().unwrap();

        let publish = match decode_slice(bytes.as_slice()).unwrap() {
            Some(Packet::Publish(s)) => s,
            _ => panic!(),
        };

        assert_eq!(
            publish,
            mqttrust::encoding::v4::publish::Publish {
                dup: false,
                qos: QoS::AtMostOnce,
                retain: false,
                topic_name: "$aws/things/test_client/streams/test_stream/get/cbor",
                payload: &[
                    164, 97, 102, 0, 97, 108, 25, 1, 0, 97, 111, 0, 97, 98, 68, 255, 255, 255, 127
                ],
                pid: None
            }
        );
    }

    #[test]
    fn decode_file_block_cbor() {
        let mqtt = &MockMqtt::new();

        let mut file_ctx = test_file_ctx(&Config::default());

        let payload = &mut [
            191, 97, 102, 0, 97, 105, 0, 97, 108, 25, 4, 0, 97, 112, 89, 4, 0, 141, 62, 28, 246,
            80, 193, 2, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 255,
        ];

        let file_blk = mqtt.decode_file_block(&mut file_ctx, payload).unwrap();

        assert_eq!(mqtt.tx.borrow_mut().len(), 0);
        assert_eq!(file_blk.file_id, 0);
        assert_eq!(file_blk.block_id, 0);
        assert_eq!(
            file_blk.block_payload,
            &[
                141, 62, 28, 246, 80, 193, 2, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(file_blk.block_size, 1024);
        assert_eq!(file_blk.client_token, None);
    }

    #[test]
    fn cleanup_unsubscribe() {
        let mqtt = &MockMqtt::new();

        let config = Config::default();

        let mut file_ctx = test_file_ctx(&config);

        mqtt.cleanup(&mut file_ctx, &config).unwrap();

        assert_eq!(mqtt.tx.borrow_mut().len(), 1);
        let bytes = mqtt.tx.borrow_mut().pop_front().unwrap();

        let packet = decode_slice(bytes.as_slice()).unwrap();
        let topics = match packet {
            Some(Packet::Unsubscribe(ref s)) => s.topics().collect::<Vec<_>>(),
            _ => panic!(),
        };

        assert_eq!(
            topics,
            vec!["$aws/things/test_client/streams/test_stream/data/cbor"]
        );
    }

    #[test]
    fn cleanup_no_unsubscribe() {
        let mqtt = &MockMqtt::new();

        let mut config = Config::default();
        config.unsubscribe_on_shutdown = false;

        let mut file_ctx = test_file_ctx(&config);

        mqtt.cleanup(&mut file_ctx, &config).unwrap();

        assert_eq!(mqtt.tx.borrow_mut().len(), 0);
    }
}
