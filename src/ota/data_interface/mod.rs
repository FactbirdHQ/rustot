// #[cfg(feature = "ota_http_data")]
// pub mod http;
#[cfg(feature = "ota_mqtt_data")]
pub mod mqtt;

use serde::Deserialize;

use crate::ota::config::Config;

use super::{encoding::FileContext, error::OtaError};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum Protocol {
    #[serde(rename = "MQTT")]
    Mqtt,
    #[serde(rename = "HTTP")]
    Http,
}

#[derive(Debug)]
pub struct FileBlock<'a> {
    pub client_token: Option<&'a str>,
    pub file_id: u8,
    pub block_size: usize,
    pub block_id: usize,
    pub block_payload: &'a [u8],
}

impl<'a> FileBlock<'a> {
    /// Validate the block index and size. If it is NOT the last block, it MUST
    /// be equal to a full block size. If it IS the last block, it MUST be equal
    /// to the expected remainder. If the block ID is out of range, that's an
    /// error.
    pub fn validate(&self, block_size: usize, filesize: usize) -> bool {
        let total_blocks = (filesize + block_size - 1) / block_size;
        let last_block_id = total_blocks - 1;

        (self.block_id < last_block_id && self.block_size == block_size)
            || (self.block_id == last_block_id
                && self.block_size == (filesize - last_block_id * block_size))
    }
}

pub trait DataInterface {
    const PROTOCOL: Protocol;

    async fn init_file_transfer(&self, file_ctx: &mut FileContext) -> Result<(), OtaError>;
    async fn request_file_block(
        &self,
        file_ctx: &mut FileContext,
        config: &Config,
    ) -> Result<(), OtaError>;
    async fn decode_file_block<'a>(
        &self,
        file_ctx: &mut FileContext,
        payload: &'a mut [u8],
    ) -> Result<FileBlock<'a>, OtaError>;
}
