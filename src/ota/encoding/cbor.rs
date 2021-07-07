use serde::{Deserialize, Serialize};

use crate::ota::data_interface::FileBlock;

use super::Bitmap;

#[derive(Serialize)]
pub struct DescribeStreamRequest<'a> {
    #[serde(rename = "c", skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

#[derive(Serialize)]
pub struct DescribeStreamResponse<'a> {
    #[serde(rename = "c", skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
    #[serde(rename = "s")]
    pub stream_version: u8,
    #[serde(rename = "d")]
    pub description: &'a str,
    #[serde(rename = "r")]
    pub files: &'a [StreamFile],
}

#[derive(Serialize)]
pub struct StreamFile {
    #[serde(rename = "f")]
    pub file_id: u8,
    #[serde(rename = "z")]
    pub file_size: usize,
}

#[derive(Serialize)]
pub struct GetStreamRequest<'a> {
    #[serde(rename = "c", skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
    #[serde(rename = "s", skip_serializing_if = "Option::is_none")]
    pub stream_version: Option<u8>,
    #[serde(rename = "f")]
    pub file_id: u8,
    #[serde(rename = "l")]
    pub block_size: usize,
    #[serde(rename = "o", skip_serializing_if = "Option::is_none")]
    pub block_offset: Option<u32>,
    #[serde(rename = "b", skip_serializing_if = "Option::is_none")]
    pub block_bitmap: Option<&'a Bitmap>,
    #[serde(rename = "n", skip_serializing_if = "Option::is_none")]
    pub number_of_blocks: Option<u32>,
}

#[derive(Debug, PartialEq, Deserialize)]
pub struct GetStreamResponse<'a> {
    #[serde(rename = "c", skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
    #[serde(rename = "f")]
    pub file_id: u8,
    #[serde(rename = "l")]
    pub block_size: usize,
    #[serde(rename = "i")]
    pub block_id: usize,
    #[serde(rename = "p")]
    pub block_payload: &'a [u8],
}

#[derive(Deserialize)]
pub struct StreamError<'a> {
    #[serde(rename = "o")]
    pub error_code: &'a str,
    #[serde(rename = "m")]
    pub error_message: &'a str,
    #[serde(rename = "c", skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

pub fn to_slice<T>(value: &T, slice: &mut [u8]) -> Result<usize, ()>
where
    T: serde::ser::Serialize,
{
    let mut serializer = serde_cbor::ser::Serializer::new(serde_cbor::ser::SliceWrite::new(slice));
    value.serialize(&mut serializer).map_err(|_| ())?;
    Ok(serializer.into_inner().bytes_written())
}

impl<'a> From<GetStreamResponse<'a>> for FileBlock<'a> {
    fn from(v: GetStreamResponse<'a>) -> Self {
        Self {
            client_token: v.client_token,
            file_id: v.file_id,
            block_size: v.block_size,
            block_id: v.block_id,
            block_payload: v.block_payload,
        }
    }
}

#[cfg(test)]
mod test {
    use core::ops::{Deref, DerefMut};

    use super::*;

    #[test]
    fn serialize_bitmap() {
        let mut bitmap = Bitmap::new(1000000, 256, 20);

        assert_eq!(
            &bitmap.deref().into_value().to_le_bytes(),
            &[0xFF, 0xFF, 0xFF, 0x7F]
        );
        let buf: &mut [u8] = &mut [0u8; 1024];
        let len = to_slice(&bitmap, buf).unwrap();
        assert_eq!(&buf[..len], &[0x44, 0xFF, 0xFF, 0xFF, 0x7F]);

        // Example from AWS IoT Developer guide
        *bitmap.deref_mut() = bitmaps::Bitmap::new();
        bitmap.deref_mut().set(0, true);
        bitmap.deref_mut().set(1, true);
        bitmap.deref_mut().set(4, true);
        bitmap.deref_mut().set(23, true);

        assert_eq!(
            &bitmap.deref().into_value().to_le_bytes(),
            &[0x13, 0x00, 0x80, 0x00]
        );

        let buf: &mut [u8] = &mut [0u8; 1024];
        let len = to_slice(&bitmap, buf).unwrap();
        assert_eq!(&buf[..len], &[0x44, 0x13, 0x00, 0x80, 0x00]);
    }

    #[test]
    fn deserialize_stream_response() {
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

        let response: GetStreamResponse = serde_cbor::de::from_mut_slice(payload).unwrap();

        assert_eq!(
            response,
            GetStreamResponse {
                file_id: 0,
                block_id: 0,
                block_size: 1024,
                client_token: None,
                block_payload: &[
                    141, 62, 28, 246, 80, 193, 2, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                ]
            }
        );
    }
    #[test]
    fn serialize_stream_request() {
        let file_size = 181584;
        const BLOCK_SIZE: usize = 2048;

        // Construct a bitmap to get the first 32 blocks (offset 0)
        let mut block_offset = 0;
        let buf: &mut [u8] = &mut [0u8; 32];

        // Check the first request
        {
            let bitmap = Bitmap::new(file_size, BLOCK_SIZE, block_offset);

            let req = GetStreamRequest {
                client_token: Some("rdy"),
                stream_version: None,
                file_id: 0,
                block_size: BLOCK_SIZE,
                block_offset: Some(block_offset),
                number_of_blocks: None,
                block_bitmap: Some(&bitmap),
            };

            let len = to_slice(&req, buf).unwrap();

            let expectation = [
                165, 97, 99, 99, 114, 100, 121, 97, 102, 0, 97, 108, 25, 8, 0, 97, 111, 0, 97, 98,
                68, 255, 255, 255, 127,
            ];
            assert_eq!(len, expectation.len(), "Arrays don't have the same length");
            assert_eq!(&buf[..len], &expectation);
        }

        block_offset = 88;

        // Check the last request (All requests in between will have same bitmap as first request, with different block_offset)
        {
            let bitmap = Bitmap::new(file_size, BLOCK_SIZE, block_offset as u32);

            let req = GetStreamRequest {
                client_token: Some("rdy"),
                stream_version: None,
                file_id: 0,
                block_size: BLOCK_SIZE,
                block_offset: Some(block_offset as u32),
                number_of_blocks: None,
                block_bitmap: Some(&bitmap),
            };

            let len = to_slice(&req, buf).unwrap();

            assert_eq!(
                &buf[..len],
                &[
                    165, 97, 99, 99, 114, 100, 121, 97, 102, 0, 97, 108, 25, 8, 0, 97, 111, 24, 88,
                    97, 98, 68, 1, 0, 0, 0
                ]
            );
        }
    }

    #[test]
    fn serialize_large_stream_request() {
        let file_size = 181584;
        const BLOCK_SIZE: usize = 512;

        // Construct a bitmap to get the first 32 blocks (offset 0)
        let mut block_offset = 0;
        let buf: &mut [u8] = &mut [0u8; 32];

        // Check the first request
        {
            let bitmap = Bitmap::new(file_size, BLOCK_SIZE, block_offset as u32);

            let req = GetStreamRequest {
                client_token: Some("rdy"),
                stream_version: None,
                file_id: 0,
                block_size: BLOCK_SIZE,
                block_offset: Some(block_offset as u32),
                number_of_blocks: None,
                block_bitmap: Some(&bitmap),
            };

            let len = to_slice(&req, buf).unwrap();

            assert_eq!(
                &buf[..len],
                &[
                    165, 97, 99, 99, 114, 100, 121, 97, 102, 0, 97, 108, 25, 2, 0, 97, 111, 0, 97,
                    98, 68, 255, 255, 255, 127
                ]
            );
        }

        block_offset = 352;

        // Check the last request (All requests in between will have same bitmap as first request, with different block_offset)
        {
            let bitmap = Bitmap::new(file_size, BLOCK_SIZE, block_offset as u32);

            let req = GetStreamRequest {
                client_token: Some("rdy"),
                stream_version: None,
                file_id: 0,
                block_size: BLOCK_SIZE,
                block_offset: Some(block_offset as u32),
                number_of_blocks: None,
                block_bitmap: Some(&bitmap),
            };

            let len = to_slice(&req, buf).unwrap();

            assert_eq!(
                &buf[..len],
                &[
                    165, 97, 99, 99, 114, 100, 121, 97, 102, 0, 97, 108, 25, 2, 0, 97, 111, 25, 1,
                    96, 97, 98, 68, 7, 0, 0, 0
                ]
            );
        }
    }
}
