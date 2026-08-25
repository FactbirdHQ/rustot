use core::fmt::Debug;

use crate::transfer::{
    data_interface::{FileBlock, RawBlock},
    error::TransferError,
};

/// Classification of a failed [`HttpClient::get_range`] call, used by the
/// transfer engine to decide whether to retry the same block or fail fast.
pub struct HttpFault {
    /// `true` if retrying the same range request could plausibly succeed
    /// (transport reset, timeout, 5xx, 429); `false` for a definitively fatal
    /// failure (4xx other than 429, malformed range).
    pub transient: bool,
    /// The HTTP status code, if the failure carried one. Preserved so a fatal
    /// error surfaces its cause even when logging is compiled out.
    pub status: Option<u16>,
}

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

    /// Classify a `get_range` error as transient (worth retrying the same
    /// block) or fatal (fail fast), and extract its HTTP status if any.
    ///
    /// Defaults to transient, so a client that doesn't override this still gets
    /// bounded per-block retry (capped by `max_request_momentum`) rather than
    /// aborting on the first blip. Override to fail fast on genuinely fatal
    /// responses (e.g. 4xx other than 429) and to surface the status code.
    fn classify(&self, _err: &Self::Error) -> HttpFault {
        HttpFault {
            transient: true,
            status: None,
        }
    }
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

    fn classify(&self, err: &Self::Error) -> HttpFault {
        C::classify(self, err)
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
        pub fn new() -> Self {
            Self {
                client: reqwest::Client::new(),
            }
        }

        pub fn new_from_client(client: reqwest::Client) -> Self {
            Self { client }
        }
    }

    impl Default for ReqwestClient {
        fn default() -> Self {
            Self::new()
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

        fn classify(&self, err: &reqwest::Error) -> HttpFault {
            let status = err.status().map(|s| s.as_u16());
            let transient = match status {
                // A status was returned: only 5xx and 429 are worth retrying;
                // other 4xx (403 expired URL, 404, malformed range) are fatal.
                Some(code) => code >= 500 || code == 429,
                // No status = transport-level fault (connection reset, timeout,
                // pool hiccup, body read error) — retry the block.
                None => true,
            };
            HttpFault { transient, status }
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
        // Per-block "momentum" retry (mirrors the MQTT interface): a transient
        // failure retries up to `max_momentum` times, `request_wait` apart.
        request_wait: embassy_time::Duration,
        max_momentum: u8,
        momentum: u8,
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

            // Copied out before the split borrow below so the retry branch can
            // read them without holding a borrow of `self`.
            let request_wait = self.request_wait;
            let max_momentum = self.max_momentum;

            // Destructure for split borrows across the async client call
            let HttpTransfer {
                client,
                url,
                buf,
                momentum,
                ..
            } = self;

            let len = match client.get_range(url.as_str(), start, end, buf).await {
                Ok(len) => {
                    // Block fetched — reset the consecutive-failure counter.
                    *momentum = 0;
                    len
                }
                Err(e) => {
                    let fault = client.classify(&e);
                    error!(
                        "HTTP range request failed (block {}, momentum {}/{}): {:?} (status={:?}, transient={})",
                        block_id, *momentum, max_momentum, e, fault.status, fault.transient
                    );

                    // Fatal (e.g. 403 expired URL, 404): fail fast, carrying the
                    // status so the cause survives even with logging compiled out.
                    if !fault.transient {
                        return Err(TransferError::Http(fault.status));
                    }

                    // Transient: retry the same block (bitmap is untouched, so the
                    // orchestrator re-requests it) until the momentum budget is spent.
                    *momentum += 1;
                    if *momentum > max_momentum {
                        return Err(TransferError::MomentumAbort);
                    }
                    embassy_time::Timer::after(request_wait).await;
                    return Err(TransferError::Momentum);
                }
            };

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
                request_wait: config.request_wait,
                max_momentum: config.max_request_momentum,
                momentum: 0,
            })
        }
    }

    #[cfg(test)]
    mod retry_tests {
        use super::*;
        use core::cell::Cell;

        /// Fails its first `n` `get_range` calls with a transient fault, then succeeds.
        struct FlakyClient(Cell<u32>);

        impl HttpClient for FlakyClient {
            type Error = ();

            async fn get_range(
                &self,
                _url: &str,
                start: usize,
                end: usize,
                buf: &mut [u8],
            ) -> Result<usize, ()> {
                if self.0.get() > 0 {
                    self.0.set(self.0.get() - 1);
                    return Err(());
                }
                buf[..end - start].fill(0);
                Ok(end - start)
            }
            // classify() left to the trait default (transient), which is what
            // this test exercises.
        }

        // The point of the fix: transient faults on a block are retried (each a
        // retryable `Momentum`) and the transfer recovers on the succeeding
        // attempt instead of aborting, with the counter reset once the block lands.
        #[tokio::test]
        async fn transient_fault_is_retried_and_recovers() {
            let block_size = 16;
            let mut t = HttpTransfer {
                client: FlakyClient(Cell::new(2)),
                url: alloc::string::String::from("http://example/x"),
                file_id: 0,
                block_size,
                file_size: block_size, // one block is enough to drive next_block
                bitmap: Bitmap::new(block_size, block_size, 0),
                block_offset: 0,
                buf: vec![0u8; block_size],
                request_wait: embassy_time::Duration::from_ticks(0), // no real backoff in tests
                max_momentum: 3,
                momentum: 0,
            };

            assert!(matches!(t.next_block().await, Err(TransferError::Momentum)));
            assert!(matches!(t.next_block().await, Err(TransferError::Momentum)));
            assert!(matches!(t.next_block().await, Ok(Some(_))));
            assert_eq!(t.momentum, 0);
        }
    }
}

#[cfg(feature = "std")]
pub use transfer::{HttpInterface, HttpTransfer};
