#[cfg(feature = "transfer_http")]
pub mod http;
#[cfg(feature = "transfer_mqtt")]
pub mod mqtt;

use serde::Deserialize;

use crate::transfer::config::Config;
use crate::transfer::status_details::StatusDetailsExt;

use super::{encoding::JobContext, error::TransferError};

use super::encoding::Bitmap;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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

impl FileBlock<'_> {
    /// Validate the block index and size. If it is NOT the last block, it MUST
    /// be equal to a full block size. If it IS the last block, it MUST be equal
    /// to the expected remainder. If the block ID is out of range, that's an
    /// error.
    pub fn validate(&self, block_size: usize, filesize: usize) -> bool {
        let total_blocks = filesize.div_ceil(block_size);
        let last_block_id = total_blocks - 1;

        (self.block_id < last_block_id && self.block_size == block_size)
            || (self.block_id == last_block_id
                && self.block_size == (filesize - last_block_id * block_size))
    }
}

/// Current download progress, passed to the transfer so it can make
/// protocol-specific decisions (which blocks to request/fetch).
pub struct BlockProgress {
    pub bitmap: Bitmap,
    pub block_offset: u32,
}

/// Protocol-specific raw block data that can be decoded into a [`FileBlock`].
///
/// For MQTT, decoding performs in-place CBOR deserialization (zero-copy).
/// For HTTP, decoding is trivial (the metadata was known at fetch time).
pub trait RawBlock {
    fn decode(&mut self) -> Result<FileBlock<'_>, TransferError>;
}

pub trait BlockTransfer {
    type RawBlock<'b>: RawBlock
    where
        Self: 'b;

    /// Receive the next block.
    ///
    /// Returns `Ok(Some(raw))` with protocol-specific raw block data.
    /// Returns `Ok(None)` if the transfer session was interrupted and needs
    /// to be re-established via [`DataInterface::begin_transfer`].
    ///
    /// For pull-based protocols (HTTP), this fetches the next needed block.
    /// For push-based protocols (MQTT), this waits for the next pushed block
    /// and handles momentum/timeout internally.
    async fn next_block(&mut self) -> Result<Option<Self::RawBlock<'_>>, TransferError>;

    /// Notify the transfer that a block was successfully written to flash.
    ///
    /// Passes the updated progress so the transfer can request more blocks
    /// when a batch is exhausted (MQTT) or advance its internal cursor (HTTP).
    async fn on_block_written(&mut self, progress: &BlockProgress) -> Result<(), TransferError>;

    /// Explicitly close the transfer session, releasing any subscriptions.
    ///
    /// If not called, cleanup is best-effort via `Drop`.
    async fn close(self) -> Result<(), TransferError>
    where
        Self: Sized,
    {
        Ok(())
    }
}

pub trait DataInterface {
    const PROTOCOL: Protocol;

    type Transfer<'t>: BlockTransfer
    where
        Self: 't;

    /// Establish a transfer session.
    ///
    /// For MQTT: subscribes to data stream + notify-next topics and publishes
    /// the initial block request.
    /// For HTTP: validates the pre-signed URL and prepares the range fetcher.
    async fn begin_transfer(
        &self,
        job: &JobContext<'_, impl StatusDetailsExt>,
        config: &Config,
        progress: &BlockProgress,
    ) -> Result<Self::Transfer<'_>, TransferError>;
}
