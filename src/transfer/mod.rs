//! File transfer orchestrator for AWS IoT Jobs.
//!
//! This module provides a file transfer engine that downloads files described
//! by AWS IoT Job documents. It supports MQTT (via IoT Streams) and HTTP
//! (via pre-signed S3 URLs) as data transport protocols.
//!
//! ## Generic file transfers
//!
//! Use [`Transfer::perform`] with any [`TransferPal`](pal::TransferPal)
//! implementation to download arbitrary files. The transfer engine handles
//! block-level progress tracking, bitmap-based retries, and status reporting.
//!
//! ## OTA firmware updates
//!
//! Use [`Transfer::perform_ota`] with an [`OtaPal`](pal::OtaPal) implementation
//! (which extends `TransferPal`) for firmware update workflows. This adds
//! self-test initialization, image state management, and device
//! reset/activation callbacks on top of the generic transfer.

pub mod config;
pub mod control_interface;
pub mod data_interface;
pub mod encoding;
pub mod error;
pub mod pal;
pub mod status_details;

use core::future::Future;

#[cfg(feature = "transfer_mqtt")]
pub use data_interface::mqtt::{Encoding, Topic};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
pub use status_details::{StatusDetails, StatusDetailsExt};

use crate::{jobs::data_types::JobStatus, transfer::encoding::json::JobStatusReason};

use self::{
    control_interface::ControlInterface,
    data_interface::{BlockProgress, BlockTransfer, DataInterface, FileBlock, RawBlock},
    encoding::{Bitmap, FileType, JobContext},
    pal::{ImageState, ImageStateReason},
};

enum IngestResult {
    Complete,
    Ingested,
    Duplicate,
}

pub struct Transfer;

impl Transfer {
    pub fn check_for_job<C: ControlInterface>(
        control: &C,
    ) -> impl Future<Output = Result<(), error::TransferError>> + '_ {
        control.request_job()
    }

    /// Generic file transfer — requires only [`TransferPal`](pal::TransferPal).
    ///
    /// Downloads the file described by the job context, handling block-level
    /// progress tracking, bitmap-based retries, and status reporting.
    /// On error, calls `pal.abort()` and reports failure to the cloud.
    ///
    /// For OTA firmware updates with self-test and image state management,
    /// use [`perform_ota`](Self::perform_ota) instead.
    pub async fn perform<C: ControlInterface, D: DataInterface, P: pal::TransferPal>(
        control: &C,
        data: &D,
        job: &JobContext<'_, P::StatusDetails>,
        pal: &mut P,
        config: &config::Config,
    ) -> Result<(), error::TransferError> {
        info!(
            "[Transfer] Starting transfer for job={} stream={} size={}",
            job.job_name,
            job.stream_name.unwrap_or("N/A"),
            job.filesize
        );

        if !job.protocols.contains(&D::PROTOCOL) {
            error!(
                "Unable to handle current job with given data interface ({:?}). Supported protocols: {:?}.",
                D::PROTOCOL,
                job.protocols
            );
            return Err(error::TransferError::InvalidInterface);
        }

        let block_offset = 0;
        let bitmap = Bitmap::new(job.filesize, config.block_size, block_offset);

        let progress_state = Mutex::new(ProgressState {
            total_blocks: job.filesize.div_ceil(config.block_size),
            blocks_remaining: job.filesize.div_ceil(config.block_size),
            block_offset,
            bitmap,
            file_size: job.filesize,
            status_details: job.status.clone(),
            extra_status: pal.status_details(),
        });

        let job_updater = JobUpdater::new(job, &progress_state, config, control);

        let result = Self::perform_inner(&job_updater, &progress_state, pal, data, config).await;

        match result {
            Ok(()) => {
                let (status, reason) = if job.file_type == Some(FileType::Firmware) {
                    (JobStatus::InProgress, JobStatusReason::SigCheckPassed)
                } else {
                    (JobStatus::Succeeded, JobStatusReason::Accepted)
                };
                job_updater.update_job_status(status, reason).await?;
                Ok(())
            }
            Err(e) => {
                let _ = pal.abort(job).await;
                let reason = match &e {
                    error::TransferError::Pal(pal_err) => {
                        JobStatusReason::Aborted(Some(ImageStateReason::Pal(*pal_err)))
                    }
                    _ => JobStatusReason::Aborted(None),
                };
                let _ = job_updater
                    .update_job_status(JobStatus::Failed, reason)
                    .await;
                Err(e)
            }
        }
    }

    /// OTA firmware update — requires [`OtaPal`](pal::OtaPal).
    ///
    /// Wraps [`perform`](Self::perform)'s download logic with OTA lifecycle
    /// management: self-test initialization, image state tracking, and
    /// device activation callbacks.
    pub async fn perform_ota<C: ControlInterface, D: DataInterface, P: pal::OtaPal>(
        control: &C,
        data: &D,
        job: &JobContext<'_, P::StatusDetails>,
        pal: &mut P,
        config: &config::Config,
    ) -> Result<(), error::TransferError> {
        info!(
            "[OTA] Starting perform_ota for job={} stream={} size={}",
            job.job_name,
            job.stream_name.unwrap_or("N/A"),
            job.filesize
        );

        let block_offset = 0;
        let bitmap = Bitmap::new(job.filesize, config.block_size, block_offset);

        let progress_state = Mutex::new(ProgressState {
            total_blocks: job.filesize.div_ceil(config.block_size),
            blocks_remaining: job.filesize.div_ceil(config.block_size),
            block_offset,
            bitmap,
            file_size: job.filesize,
            status_details: job.status.clone(),
            extra_status: pal.status_details(),
        });

        let job_updater = JobUpdater::new(job, &progress_state, config, control);

        // Self-test initialization (OTA-specific)
        match Self::ota_initialize(pal, &job_updater).await? {
            Some(()) => {}
            None => return Ok(()),
        };

        // Protocol check with image state reporting
        if !job.protocols.contains(&D::PROTOCOL) {
            error!(
                "Unable to handle current OTA job with given data interface ({:?}). Supported protocols: {:?}. Aborting current update.",
                D::PROTOCOL,
                job.protocols
            );
            Self::set_image_state_with_reason(
                pal,
                &job_updater,
                ImageState::Aborted(ImageStateReason::InvalidDataProtocol),
            )
            .await?;
            return Err(error::TransferError::InvalidInterface);
        }

        info!("Job document was accepted. Attempting to begin the update");

        let result = Self::perform_inner(&job_updater, &progress_state, pal, data, config).await;

        match result {
            Ok(()) => {
                let event = if job.file_type == Some(FileType::Firmware) {
                    pal::OtaEvent::Activate
                } else {
                    pal::OtaEvent::UpdateComplete
                };

                let (status, reason) = if job.file_type == Some(FileType::Firmware) {
                    (JobStatus::InProgress, JobStatusReason::SigCheckPassed)
                } else {
                    (JobStatus::Succeeded, JobStatusReason::Accepted)
                };
                job_updater.update_job_status(status, reason).await?;

                info!(
                    "OTA Download finished! Running complete callback: {:?}",
                    event
                );

                pal.complete_callback(event).await?;
                Ok(())
            }
            Err(error::TransferError::MomentumAbort) => {
                warn!("[OTA] Momentum abort triggered");
                let _ = pal.abort(job).await;
                Self::set_image_state_with_reason(
                    pal,
                    &job_updater,
                    ImageState::Aborted(ImageStateReason::MomentumAbort),
                )
                .await?;
                Err(error::TransferError::MomentumAbort)
            }
            Err(error::TransferError::UserAbort) => {
                warn!("[OTA] Job cancelled (detected via notification)");
                let _ = pal.abort(job).await;
                Self::set_image_state_with_reason(
                    pal,
                    &job_updater,
                    ImageState::Aborted(ImageStateReason::UserAbort),
                )
                .await?;
                Err(error::TransferError::UserAbort)
            }
            Err(e) => {
                let reason = match &e {
                    error::TransferError::Pal(pal_err) => {
                        JobStatusReason::Aborted(Some(ImageStateReason::Pal(*pal_err)))
                    }
                    _ => JobStatusReason::Aborted(None),
                };
                let _ = pal.abort(job).await;
                job_updater
                    .update_job_status(JobStatus::Failed, reason)
                    .await?;

                pal.complete_callback(pal::OtaEvent::Fail).await?;
                info!("Application callback! OtaEvent::Fail");
                Err(e)
            }
        }
    }

    /// Raw download loop. No cleanup, no abort, no status reporting on error.
    ///
    /// Returns `Ok(())` when all blocks have been received and the file has
    /// been closed. Returns `Err` on any failure — the caller handles cleanup.
    async fn perform_inner<C: ControlInterface, D: DataInterface, P: pal::TransferPal>(
        job_updater: &JobUpdater<'_, C, P::StatusDetails>,
        progress_state: &Mutex<NoopRawMutex, ProgressState<P::StatusDetails>>,
        pal: &mut P,
        data: &D,
        config: &config::Config,
    ) -> Result<(), error::TransferError> {
        let job = job_updater.job;

        // Spawn the status update future
        let status_update_fut = job_updater.handle_status_updates();

        // Spawn the data handling future
        let data_fut = async {
            // Create/Open the file on the file system
            let block_writer = pal.create_file_for_rx(job).await.map_err(|e| {
                error!("Failed to create file for rx: {:?}", e);
                error::TransferError::from(e)
            })?;

            info!("Initialized file handler! Requesting file blocks");

            // Outer loop to handle re-establishment on clean session
            loop {
                let block_progress = {
                    let progress = progress_state.lock().await;
                    BlockProgress {
                        bitmap: progress.bitmap.clone(),
                        block_offset: progress.block_offset,
                    }
                };

                let mut transfer = data.begin_transfer(job, config, &block_progress).await?;

                info!("Awaiting file blocks!");

                // Inner loop to process blocks.
                loop {
                    let notify_progress = {
                        let result = transfer.next_block().await;
                        match result {
                            Ok(Some(mut raw)) => {
                                let mut progress = progress_state.lock().await;

                                match raw.decode() {
                                    Ok(block) => {
                                        match Self::ingest_data_block(
                                            &block,
                                            block_writer,
                                            config,
                                            &mut progress,
                                        )
                                        .await
                                        {
                                            Ok(IngestResult::Complete) => {
                                                // All blocks received — close file
                                                drop(progress);
                                                pal.close_file(job).await?;
                                                return Ok(());
                                            }
                                            Ok(IngestResult::Ingested) => {
                                                if progress.blocks_remaining
                                                    % config.status_update_frequency as usize
                                                    == 0
                                                {
                                                    job_updater.signal_update(
                                                        JobStatus::InProgress,
                                                        JobStatusReason::Receiving,
                                                    );
                                                }
                                                Some(BlockProgress {
                                                    bitmap: progress.bitmap.clone(),
                                                    block_offset: progress.block_offset,
                                                })
                                            }
                                            Ok(IngestResult::Duplicate) => None,
                                            Err(e) if e.is_retryable() => {
                                                error!(
                                                    "Failed block validation: {:?}! Retrying",
                                                    e
                                                );
                                                None
                                            }
                                            Err(e) => return Err(e),
                                        }
                                    }
                                    Err(e) if e.is_retryable() => {
                                        error!("Failed block decode: {:?}! Retrying", e);
                                        None
                                    }
                                    Err(e) => return Err(e),
                                }
                            }
                            Ok(None) => {
                                warn!(
                                    "Data stream ended (clean session/disconnect). Re-establishing and resuming..."
                                );
                                // Break inner loop to trigger re-establishment
                                break;
                            }

                            Err(e) if e.is_retryable() => {
                                continue;
                            }
                            Err(e) => {
                                error!("Block transfer error: {:?}", e);
                                return Err(e);
                            }
                        }
                    };

                    if let Some(bp) = notify_progress {
                        transfer.on_block_written(&bp).await?;
                    }
                }

                // Stream disconnected — close the transfer before re-establishing
                let _ = transfer.close().await;

                let blocks_remaining = {
                    let progress = progress_state.lock().await;
                    progress.blocks_remaining
                };
                info!("Resuming transfer: {} blocks remaining", blocks_remaining);
            }
        };

        // select: when data_fut completes, status_update_fut is dropped.
        match embassy_futures::select::select(data_fut, status_update_fut).await {
            embassy_futures::select::Either::First(res) => res,
            embassy_futures::select::Either::Second(res) => {
                // status_update_fut returned first — this means a status publish
                // error occurred. Propagate it.
                res
            }
        }
    }

    /// OTA self-test initialization. Returns `None` if the self-test was
    /// handled (job is done), `Some(())` if the download should proceed.
    async fn ota_initialize<C: ControlInterface, P: pal::OtaPal>(
        pal: &mut P,
        job_updater: &JobUpdater<'_, C, P::StatusDetails>,
    ) -> Result<Option<()>, error::TransferError> {
        let platform_self_test = pal
            .get_platform_image_state()
            .await
            .is_ok_and(|i| i == pal::PalImageState::PendingCommit);

        match (job_updater.job.self_test(), platform_self_test) {
            (true, true) => {
                // Run self-test!
                Self::set_image_state_with_reason(
                    pal,
                    job_updater,
                    ImageState::Testing(ImageStateReason::VersionCheck),
                )
                .await?;

                info!("Beginning self-test");

                let test_fut = pal.complete_callback(pal::OtaEvent::StartTest);

                match job_updater.config.self_test_timeout {
                    Some(timeout) => embassy_time::with_timeout(timeout, test_fut)
                        .await
                        .map_err(|_| error::TransferError::Timeout)?,
                    None => test_fut.await,
                }?;

                job_updater
                    .update_job_status(JobStatus::Succeeded, JobStatusReason::Accepted)
                    .await?;

                Ok(None)
            }
            (false, false) => Ok(Some(())),
            (false, true) => {
                error!(
                    "Rejecting new image and rebooting: The platform is in the self-test state while the job is not."
                );
                pal.reset_device().await?;
                Err(error::TransferError::ResetFailed)
            }
            (true, false) => {
                error!(
                    "Rejecting new image and rebooting: the job is in the self-test state while the platform is not."
                );
                Self::set_image_state_with_reason(
                    pal,
                    job_updater,
                    ImageState::Rejected(ImageStateReason::ImageStateMismatch),
                )
                .await?;

                pal.reset_device().await?;
                Err(error::TransferError::ResetFailed)
            }
        }
    }

    /// Set the OTA image state and report it to the cloud.
    async fn set_image_state_with_reason<C: ControlInterface, P: pal::OtaPal>(
        pal: &mut P,
        job_updater: &JobUpdater<'_, C, P::StatusDetails>,
        image_state: ImageState,
    ) -> Result<(), error::TransferError> {
        let image_state = match pal.set_platform_image_state(image_state).await {
            Err(e) if !matches!(image_state, ImageState::Aborted(_)) => {
                ImageState::Rejected(ImageStateReason::Pal(e))
            }
            _ => image_state,
        };

        match image_state {
            ImageState::Testing(_) => {
                job_updater
                    .update_job_status(JobStatus::InProgress, JobStatusReason::SelfTestActive)
                    .await?;
            }
            ImageState::Accepted => {
                job_updater
                    .update_job_status(JobStatus::Succeeded, JobStatusReason::Accepted)
                    .await?;
            }
            ImageState::Rejected(reason) => {
                job_updater
                    .update_job_status(JobStatus::Failed, JobStatusReason::Rejected(Some(reason)))
                    .await?;
            }
            ImageState::Aborted(reason) => {
                job_updater
                    .update_job_status(JobStatus::Failed, JobStatusReason::Aborted(Some(reason)))
                    .await?;
            }
            ImageState::Unknown => {
                job_updater
                    .update_job_status(JobStatus::Failed, JobStatusReason::Aborted(None))
                    .await?;
            }
        }
        Ok(())
    }

    async fn ingest_data_block<E: StatusDetailsExt>(
        block: &FileBlock<'_>,
        block_writer: &mut impl pal::BlockWriter,
        config: &config::Config,
        progress: &mut ProgressState<E>,
    ) -> Result<IngestResult, error::TransferError> {
        if block.validate(config.block_size, progress.file_size) {
            if block.block_id < progress.block_offset as usize
                || !progress
                    .bitmap
                    .get(block.block_id - progress.block_offset as usize)
            {
                info!(
                    "Block {:?} is a DUPLICATE. {:?} blocks remaining.",
                    block.block_id, progress.blocks_remaining
                );

                return Ok(IngestResult::Duplicate);
            }

            info!(
                "Received block {}. {:?} blocks remaining.",
                block.block_id, progress.blocks_remaining
            );

            block_writer
                .write(
                    (block.block_id * config.block_size) as u32,
                    block.block_payload,
                )
                .await
                .map_err(|_| {
                    error!("Block write failed");
                    error::TransferError::WriteFailed
                })?;

            let block_offset = progress.block_offset;
            progress
                .bitmap
                .set(block.block_id - block_offset as usize, false);

            progress.blocks_remaining -= 1;

            if progress.blocks_remaining == 0 {
                info!("Received final expected block of file.");
                Ok(IngestResult::Complete)
            } else {
                if progress.bitmap.is_empty() {
                    progress.block_offset += 31;
                    progress.bitmap = encoding::Bitmap::new(
                        progress.file_size,
                        config.block_size,
                        progress.block_offset,
                    );
                }

                Ok(IngestResult::Ingested)
            }
        } else {
            error!(
                "Error! Block {:?} out of expected range! Size {:?}",
                block.block_id, block.block_size
            );

            Err(error::TransferError::BlockOutOfRange)
        }
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ProgressState<E: StatusDetailsExt = ()> {
    pub total_blocks: usize,
    pub blocks_remaining: usize,
    pub file_size: usize,
    pub block_offset: u32,
    #[cfg_attr(feature = "defmt", defmt(Debug2Format))]
    pub bitmap: Bitmap,
    #[cfg_attr(feature = "defmt", defmt(Debug2Format))]
    pub status_details: StatusDetails,
    pub extra_status: E,
}

pub struct JobUpdater<'a, C: ControlInterface, E: StatusDetailsExt = ()> {
    pub job: &'a JobContext<'a, E>,
    pub config: &'a config::Config,
    pub control: &'a C,
    progress_state: &'a Mutex<NoopRawMutex, ProgressState<E>>,
    status_update_signal: Signal<NoopRawMutex, (JobStatus, JobStatusReason)>,
}

impl<'a, C: ControlInterface, E: StatusDetailsExt> JobUpdater<'a, C, E> {
    pub fn new(
        job: &'a JobContext<'a, E>,
        progress_state: &'a Mutex<NoopRawMutex, ProgressState<E>>,
        config: &'a config::Config,
        control: &'a C,
    ) -> Self {
        Self {
            job,
            progress_state,
            config,
            control,
            status_update_signal: Signal::<NoopRawMutex, (JobStatus, JobStatusReason)>::new(),
        }
    }

    async fn handle_status_updates(&self) -> Result<(), error::TransferError> {
        loop {
            let (status, reason) = self.status_update_signal.wait().await;

            let mut progress = self.progress_state.lock().await;
            self.control
                .update_job_status(self.job, &mut progress, status, reason)
                .await?;

            match status {
                JobStatus::Queued | JobStatus::InProgress => {}
                _ => return Ok(()),
            }
        }
    }

    pub fn signal_update(&self, status: JobStatus, reason: JobStatusReason) {
        self.status_update_signal.signal((status, reason));
    }

    pub async fn update_job_status(
        &self,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), error::TransferError> {
        let mut progress = self.progress_state.lock().await;

        self.control
            .update_job_status(self.job, &mut progress, status, reason)
            .await?;
        Ok(())
    }
}
