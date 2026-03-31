use core::fmt::Debug;

use crate::transfer::{
    data_interface::{FileBlock, RawBlock},
    error::TransferError,
};

/// Minimal HTTP client trait for OTA Range-based downloads.
///
/// No_std compatible — the caller provides the buffer. Implementations
/// for `reqwest` (std) and `reqless` (no_std) can be provided via feature flags.
pub trait HttpClient {
    type Error: Debug;

    /// Fetch bytes from `url` in the byte range `[start, end)`.
    ///
    /// Writes the response body into `buf` and returns the number of bytes
    /// written. The caller guarantees `buf.len() >= (end - start)`.
    async fn get_range(
        &self,
        url: &str,
        start: usize,
        end: usize,
        buf: &mut [u8],
    ) -> Result<usize, Self::Error>;
}

impl<C: HttpClient> HttpClient for &C {
    type Error = C::Error;

    async fn get_range(
        &self,
        url: &str,
        start: usize,
        end: usize,
        buf: &mut [u8],
    ) -> Result<usize, Self::Error> {
        C::get_range(self, url, start, end, buf).await
    }
}

/// Raw block from an HTTP Range response. Borrows from the transfer's
/// internal buffer — no extra allocation per block.
pub struct HttpRawBlock<'a> {
    payload: &'a [u8],
    file_id: u8,
    block_id: usize,
}

impl RawBlock for HttpRawBlock<'_> {
    fn decode(&mut self) -> Result<FileBlock<'_>, TransferError> {
        Ok(FileBlock {
            client_token: None,
            file_id: self.file_id,
            block_size: self.payload.len(),
            block_id: self.block_id,
            block_payload: self.payload,
        })
    }
}

// --- reqwest implementation (requires std) ---

#[cfg(feature = "transfer_http_reqwest")]
mod reqwest_impl {
    use super::*;

    pub struct ReqwestClient {
        client: reqwest::Client,
    }

    impl ReqwestClient {
        pub fn new(client: reqwest::Client) -> Self {
            Self { client }
        }
    }

    impl HttpClient for ReqwestClient {
        type Error = reqwest::Error;

        async fn get_range(
            &self,
            url: &str,
            start: usize,
            end: usize,
            buf: &mut [u8],
        ) -> Result<usize, Self::Error> {
            let response = self
                .client
                .get(url)
                .header("Range", format!("bytes={}-{}", start, end - 1))
                .send()
                .await?
                .error_for_status()?;

            let bytes = response.bytes().await?;
            let len = bytes.len();
            buf[..len].copy_from_slice(&bytes);
            Ok(len)
        }
    }
}

#[cfg(feature = "transfer_http_reqwest")]
pub use reqwest_impl::ReqwestClient;

// --- Concrete transfer implementation (requires std for Vec) ---

#[cfg(feature = "std")]
mod transfer {
    extern crate alloc;

    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::transfer::{
        config::Config,
        data_interface::{BlockProgress, DataInterface, Protocol},
        encoding::{Bitmap, JobContext},
        status_details::StatusDetailsExt,
    };

    use super::super::BlockTransfer;

    pub struct HttpInterface<C> {
        client: C,
    }

    impl<C: HttpClient> HttpInterface<C> {
        pub fn new(client: C) -> Self {
            Self { client }
        }
    }

    pub struct HttpTransfer<C> {
        client: C,
        url: alloc::string::String,
        file_id: u8,
        block_size: usize,
        file_size: usize,
        bitmap: Bitmap,
        block_offset: u32,
        buf: Vec<u8>,
    }

    impl<C: HttpClient> BlockTransfer for HttpTransfer<C> {
        type RawBlock<'b>
            = HttpRawBlock<'b>
        where
            Self: 'b;

        async fn next_block(&mut self) -> Result<Option<Self::RawBlock<'_>>, TransferError> {
            // Find the next block we need from the bitmap
            let local_id = match self.bitmap.first_index() {
                Some(id) => id,
                None => return Ok(None),
            };
            let block_id = self.block_offset as usize + local_id;

            let start = block_id * self.block_size;
            let end = (start + self.block_size).min(self.file_size);

            // Destructure for split borrows across the async client call
            let HttpTransfer {
                client, url, buf, ..
            } = self;

            let len = client
                .get_range(url.as_str(), start, end, buf)
                .await
                .map_err(|e| {
                    error!("HTTP range request failed: {:?}", e);
                    TransferError::Http
                })?;

            Ok(Some(HttpRawBlock {
                payload: &self.buf[..len],
                file_id: self.file_id,
                block_id,
            }))
        }

        async fn on_block_written(
            &mut self,
            progress: &BlockProgress,
        ) -> Result<(), TransferError> {
            self.bitmap = progress.bitmap.clone();
            self.block_offset = progress.block_offset;
            Ok(())
        }
    }

    impl<C: HttpClient> DataInterface for HttpInterface<C> {
        const PROTOCOL: Protocol = Protocol::Http;

        type Transfer<'t>
            = HttpTransfer<&'t C>
        where
            Self: 't;

        async fn begin_transfer(
            &self,
            job: &JobContext<'_, impl StatusDetailsExt>,
            config: &Config,
            progress: &BlockProgress,
        ) -> Result<Self::Transfer<'_>, TransferError> {
            let url = job.update_data_url.ok_or(TransferError::InvalidFile)?;

            info!(
                "[OTA-HTTP] Beginning transfer: url_len={} block_size={} file_size={}",
                url.len(),
                config.block_size,
                job.filesize
            );

            Ok(HttpTransfer {
                client: &self.client,
                url: alloc::string::String::from(url),
                file_id: job.fileid,
                block_size: config.block_size,
                file_size: job.filesize,
                bitmap: progress.bitmap.clone(),
                block_offset: progress.block_offset,
                buf: vec![0u8; config.block_size],
            })
        }
    }
}

#[cfg(feature = "std")]
pub use transfer::{HttpInterface, HttpTransfer};
