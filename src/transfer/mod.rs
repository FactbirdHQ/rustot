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
//! ## OTA updates
//!
//! Use [`Transfer::perform_ota`] with an [`OtaPal`](pal::OtaPal) implementation
//! (which extends `TransferPal`) for update workflows that need activation.
//! This adds self-test initialization, image state management, and device
//! reset/activation callbacks on top of the generic transfer.
//!
//! ## Multi-file jobs
//!
//! A job may contain up to `N` files (see [`JobContext`]). Files are
//! downloaded in document order and the job is all-or-nothing: activation
//! happens once after every file is closed, and any failure aborts the
//! whole job.

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
pub use status_details::{FileState, FileStatus, StatusDetails, StatusDetailsExt};

use crate::{jobs::data_types::JobStatus, transfer::encoding::json::JobStatusReason};

use self::{
    control_interface::ControlInterface,
    data_interface::{BlockProgress, BlockTransfer, DataInterface, FileBlock, RawBlock},
    encoding::{Bitmap, FileContext, JobContext},
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
    /// Downloads every file in the job context, in order, handling
    /// block-level progress tracking, bitmap-based retries, and status
    /// reporting. On error, calls `pal.abort()` once for the whole job and
    /// reports failure to the cloud.
    ///
    /// If no file [requires activation](pal::TransferPal::requires_activation)
    /// the job is marked `SUCCEEDED`. Otherwise it is left `IN_PROGRESS` with
    /// self-test `ready`, and the caller is responsible for activating the
    /// files and finishing the job (e.g. via
    /// [`JobAgent::succeed_job`](crate::jobs::stream::JobAgent::succeed_job)).
    ///
    /// For OTA updates with self-test and image state management, use
    /// [`perform_ota`](Self::perform_ota) instead.
    pub async fn perform<
        C: ControlInterface,
        D: DataInterface,
        P: pal::TransferPal,
        const N: usize,
    >(
        control: &C,
        data: &D,
        job: &JobContext<'_, P::StatusDetails, N>,
        pal: &mut P,
        config: &config::Config,
    ) -> Result<(), error::TransferError> {
        info!(
            "[Transfer] Starting transfer for job={} stream={} files={} size={}",
            job.job_name,
            job.stream_name.unwrap_or("N/A"),
            job.files.len(),
            job.total_size()
        );

        if !job.protocols.contains(&D::PROTOCOL) {
            error!(
                "Unable to handle current job with given data interface ({:?}). Supported protocols: {:?}.",
                D::PROTOCOL,
                job.protocols
            );
            return Err(error::TransferError::InvalidInterface);
        }

        let progress_state = Mutex::new(ProgressState::new(
            job,
            config.block_size,
            pal.status_details(),
        ));

        let job_updater = JobUpdater::new(job, &progress_state, config, control);

        let result = Self::download_files(&job_updater, &progress_state, pal, data, config).await;

        match result {
            Ok(()) => {
                let (status, reason) = if Self::requires_activation(job, pal) {
                    (JobStatus::InProgress, JobStatusReason::SigCheckPassed)
                } else {
                    (JobStatus::Succeeded, JobStatusReason::Accepted)
                };
                job_updater.update_job_status(status, reason).await?;
                Ok(())
            }
            Err(e) => {
                let _ = pal.abort(job.job_name).await;
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

    /// OTA update — requires [`OtaPal`](pal::OtaPal).
    ///
    /// Wraps [`perform`](Self::perform)'s download logic with OTA lifecycle
    /// management: self-test initialization, image state tracking, and
    /// device activation callbacks. Activation covers the job as a whole and
    /// only happens once every file has been downloaded and closed.
    pub async fn perform_ota<
        C: ControlInterface,
        D: DataInterface,
        P: pal::OtaPal,
        const N: usize,
    >(
        control: &C,
        data: &D,
        job: &JobContext<'_, P::StatusDetails, N>,
        pal: &mut P,
        config: &config::Config,
    ) -> Result<(), error::TransferError> {
        info!(
            "[OTA] Starting perform_ota for job={} stream={} files={} size={}",
            job.job_name,
            job.stream_name.unwrap_or("N/A"),
            job.files.len(),
            job.total_size()
        );

        let progress_state = Mutex::new(ProgressState::new(
            job,
            config.block_size,
            pal.status_details(),
        ));

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

        let result = Self::download_files(&job_updater, &progress_state, pal, data, config).await;

        match result {
            Ok(()) => {
                let (event, status, reason) = if Self::requires_activation(job, pal) {
                    (
                        pal::OtaEvent::Activate,
                        JobStatus::InProgress,
                        JobStatusReason::SigCheckPassed,
                    )
                } else {
                    (
                        pal::OtaEvent::UpdateComplete,
                        JobStatus::Succeeded,
                        JobStatusReason::Accepted,
                    )
                };
                job_updater.update_job_status(status, reason).await?;

                info!(
                    "OTA Download finished! Running complete callback: {:?}",
                    event
                );

                match pal.complete_callback(event).await {
                    Err(e) if event == pal::OtaEvent::Activate => {
                        Self::reject_failed_activation(pal, &job_updater, e).await
                    }
                    result => Ok(result?),
                }
            }
            Err(error::TransferError::MomentumAbort) => {
                warn!("[OTA] Momentum abort triggered");
                let _ = pal.abort(job.job_name).await;
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
                let _ = pal.abort(job.job_name).await;
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
                let _ = pal.abort(job.job_name).await;
                job_updater
                    .update_job_status(JobStatus::Failed, reason)
                    .await?;

                pal.complete_callback(pal::OtaEvent::Fail).await?;
                info!("Application callback! OtaEvent::Fail");
                Err(e)
            }
        }
    }

    /// Left unreported, a failed activation would leave the job IN_PROGRESS
    /// (`self_test: ready`) until the next delivery.
    async fn reject_failed_activation<C: ControlInterface, P: pal::OtaPal, const N: usize>(
        pal: &mut P,
        job_updater: &JobUpdater<'_, C, P::StatusDetails, N>,
        e: pal::PalError,
    ) -> Result<(), error::TransferError> {
        error!("Activation failed ({:?}), rejecting the update", e);
        Self::set_image_state_with_reason(
            pal,
            job_updater,
            ImageState::Rejected(ImageStateReason::Pal(e)),
        )
        .await?;
        pal.complete_callback(pal::OtaEvent::Fail).await?;
        Err(error::TransferError::Pal(e))
    }

    fn requires_activation<P: pal::TransferPal, const N: usize>(
        job: &JobContext<'_, P::StatusDetails, N>,
        pal: &P,
    ) -> bool {
        job.files.iter().any(|file| pal.requires_activation(file))
    }

    /// Download every file in the job, in document order. Stops at the first
    /// failure — the caller handles cleanup.
    async fn download_files<
        C: ControlInterface,
        D: DataInterface,
        P: pal::TransferPal,
        const N: usize,
    >(
        job_updater: &JobUpdater<'_, C, P::StatusDetails, N>,
        progress_state: &Mutex<NoopRawMutex, ProgressState<P::StatusDetails, N>>,
        pal: &mut P,
        data: &D,
        config: &config::Config,
    ) -> Result<(), error::TransferError> {
        let files = &job_updater.job.files;
        if files.is_empty() {
            return Err(error::TransferError::InvalidFile);
        }

        for (idx, file) in files.iter().enumerate() {
            info!(
                "[Transfer] Downloading file {}/{}: id={} path={} size={}",
                idx + 1,
                files.len(),
                file.fileid,
                file.filepath,
                file.filesize
            );

            progress_state
                .lock()
                .await
                .begin_file(idx, file.filesize, config.block_size);

            let result =
                Self::perform_inner(job_updater, progress_state, pal, data, file, config).await;

            let mut progress = progress_state.lock().await;
            match result {
                Ok(()) => progress.files[idx].state = FileState::Succeeded,
                Err(e) => {
                    progress.files[idx].state = FileState::Failed(file_failure_reason(&e));
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Raw download loop for a single file. No cleanup, no abort, no status
    /// reporting on error.
    ///
    /// Returns `Ok(())` when all blocks of `file` have been received and the
    /// file has been closed. Returns `Err` on any failure — the caller
    /// handles cleanup.
    async fn perform_inner<
        C: ControlInterface,
        D: DataInterface,
        P: pal::TransferPal,
        const N: usize,
    >(
        job_updater: &JobUpdater<'_, C, P::StatusDetails, N>,
        progress_state: &Mutex<NoopRawMutex, ProgressState<P::StatusDetails, N>>,
        pal: &mut P,
        data: &D,
        file: &FileContext<'_>,
        config: &config::Config,
    ) -> Result<(), error::TransferError> {
        let job = job_updater.job;

        // Spawn the status update future
        let status_update_fut = job_updater.handle_status_updates();

        // Spawn the data handling future
        let data_fut = async {
            // Create/Open the file on the file system
            let block_writer = pal.create_file_for_rx(file).await.map_err(|e| {
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

                let mut transfer = data
                    .begin_transfer(job, file, config, &block_progress)
                    .await?;

                info!("Awaiting file blocks!");

                // Inner loop to process blocks. Yields `true` once the file is
                // complete, `false` if the session must be re-established.
                let completed = loop {
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
                                            Ok(IngestResult::Complete) => break true,
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
                                break false;
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
                };

                // Release the session before closing the file or re-establishing,
                // so the next file (or session) starts with a clean subscription.
                let _ = transfer.close().await;

                if completed {
                    pal.close_file(file).await?;
                    return Ok(());
                }

                let blocks_remaining = progress_state.lock().await.file_blocks_remaining;
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
    async fn ota_initialize<C: ControlInterface, P: pal::OtaPal, const N: usize>(
        pal: &mut P,
        job_updater: &JobUpdater<'_, C, P::StatusDetails, N>,
    ) -> Result<Option<()>, error::TransferError> {
        let platform_self_test = pal
            .get_platform_image_state()
            .await
            .is_ok_and(|i| i == pal::PalImageState::PendingCommit);

        let job_self_test = job_updater.job.self_test();
        if job_self_test {
            // The job only reaches self-test after every file was downloaded,
            // but progress is not restored across the reboot that got us here.
            job_updater.progress_state.lock().await.mark_complete();
        }

        match (job_self_test, platform_self_test) {
            (true, true) => {
                let Err(reason) = Self::run_self_test(pal, job_updater).await? else {
                    job_updater
                        .update_job_status(JobStatus::Succeeded, JobStatusReason::Accepted)
                        .await?;
                    return Ok(None);
                };

                // Left unreported, the job would stay IN_PROGRESS and block
                // every later job. Rejecting before the reset matches the
                // mismatch branch below: the reset is what rolls back.
                error!("Self-test failed ({:?}), rejecting the update", reason);
                Self::set_image_state_with_reason(pal, job_updater, ImageState::Rejected(reason))
                    .await?;
                pal.complete_callback(pal::OtaEvent::SelfTestFailed).await?;
                pal.reset_device().await?;
                Err(error::TransferError::ResetFailed)
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

    /// Enter self-test and run it. The outer error is a failure to report
    /// status; the inner one is why the update must be rejected.
    async fn run_self_test<C: ControlInterface, P: pal::OtaPal, const N: usize>(
        pal: &mut P,
        job_updater: &JobUpdater<'_, C, P::StatusDetails, N>,
    ) -> Result<Result<(), ImageStateReason>, error::TransferError> {
        // Not via `set_image_state_with_reason`: on failure it reports the
        // job FAILED itself, and the caller must reject only once.
        if let Err(e) = pal
            .set_platform_image_state(ImageState::Testing(ImageStateReason::VersionCheck))
            .await
        {
            return Ok(Err(ImageStateReason::Pal(e)));
        }
        job_updater
            .update_job_status(JobStatus::InProgress, JobStatusReason::SelfTestActive)
            .await?;

        info!("Beginning self-test");

        let test_fut = pal.complete_callback(pal::OtaEvent::StartTest);
        Ok(match job_updater.config.self_test_timeout {
            Some(timeout) => match embassy_time::with_timeout(timeout, test_fut).await {
                Ok(result) => result.map_err(ImageStateReason::Pal),
                Err(_) => Err(ImageStateReason::SelfTestTimeout),
            },
            None => test_fut.await.map_err(ImageStateReason::Pal),
        })
    }

    /// Set the OTA image state and report it to the cloud.
    async fn set_image_state_with_reason<C: ControlInterface, P: pal::OtaPal, const N: usize>(
        pal: &mut P,
        job_updater: &JobUpdater<'_, C, P::StatusDetails, N>,
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

    async fn ingest_data_block<E: StatusDetailsExt, const N: usize>(
        block: &FileBlock<'_>,
        block_writer: &mut impl pal::BlockWriter,
        config: &config::Config,
        progress: &mut ProgressState<E, N>,
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
                block.block_id, progress.file_blocks_remaining
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
            progress.file_blocks_remaining -= 1;
            let current = progress.current_file;
            progress.files[current].received_blocks += 1;

            if progress.file_blocks_remaining == 0 {
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

/// Download progress for a job.
///
/// `total_blocks` and `blocks_remaining` span every file in the job and drive
/// the reported `progress`; `files` drives the per-file `files` status detail
/// of multi-file jobs. The remaining fields track the file currently being
/// downloaded and are reset at each file boundary.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ProgressState<E: StatusDetailsExt = (), const N: usize = 1> {
    pub total_blocks: usize,
    pub blocks_remaining: usize,
    pub files: heapless::Vec<FileStatus, N>,
    pub current_file: usize,
    pub file_size: usize,
    pub file_blocks_remaining: usize,
    pub block_offset: u32,
    #[cfg_attr(feature = "defmt", defmt(Debug2Format))]
    pub bitmap: Bitmap,
    #[cfg_attr(feature = "defmt", defmt(Debug2Format))]
    pub status_details: StatusDetails,
    pub extra_status: E,
}

impl<E: StatusDetailsExt, const N: usize> ProgressState<E, N> {
    fn new(job: &JobContext<'_, E, N>, block_size: usize, extra_status: E) -> Self {
        const { status_details::assert_files_fit::<N>() };

        let files: heapless::Vec<FileStatus, N> = job
            .files
            .iter()
            .map(|file| FileStatus {
                fileid: file.fileid,
                state: FileState::Pending,
                received_blocks: 0,
                total_blocks: file.filesize.div_ceil(block_size),
            })
            .collect();
        let total_blocks = files.iter().map(|file| file.total_blocks).sum();

        Self {
            total_blocks,
            blocks_remaining: total_blocks,
            files,
            current_file: 0,
            file_size: 0,
            file_blocks_remaining: 0,
            block_offset: 0,
            bitmap: Bitmap::new(0, block_size, 0),
            status_details: job.status.clone(),
            extra_status,
        }
    }

    fn begin_file(&mut self, idx: usize, file_size: usize, block_size: usize) {
        self.current_file = idx;
        self.files[idx].state = FileState::InProgress;
        self.file_size = file_size;
        self.file_blocks_remaining = file_size.div_ceil(block_size);
        self.block_offset = 0;
        self.bitmap = Bitmap::new(file_size, block_size, 0);
    }

    /// Mark every file as fully received.
    fn mark_complete(&mut self) {
        self.blocks_remaining = 0;
        for file in &mut self.files {
            file.state = FileState::Succeeded;
            file.received_blocks = file.total_blocks;
        }
    }

    /// Fill in the status details for an update with `status` and `reason`.
    fn apply_status(&mut self, status: JobStatus, reason: JobStatusReason) {
        self.status_details.set_self_test(reason.as_str());

        if let JobStatus::InProgress | JobStatus::Succeeded | JobStatus::Failed = status {
            self.status_details
                .set_progress(self.total_blocks - self.blocks_remaining, self.total_blocks);
        }

        if let Some(detail) = reason.detail() {
            self.status_details
                .set_failure(detail.as_reason_str(), detail.error_code());
        } else {
            self.status_details.clear_failure();
        }
    }
}

/// Per-file failure reason for a transfer error, if one can be reported.
fn file_failure_reason(e: &error::TransferError) -> Option<ImageStateReason> {
    match e {
        error::TransferError::Pal(pal_err) => Some(ImageStateReason::Pal(*pal_err)),
        error::TransferError::MomentumAbort => Some(ImageStateReason::MomentumAbort),
        error::TransferError::UserAbort => Some(ImageStateReason::UserAbort),
        _ => None,
    }
}

pub struct JobUpdater<'a, C: ControlInterface, E: StatusDetailsExt = (), const N: usize = 1> {
    pub job: &'a JobContext<'a, E, N>,
    pub config: &'a config::Config,
    pub control: &'a C,
    progress_state: &'a Mutex<NoopRawMutex, ProgressState<E, N>>,
    status_update_signal: Signal<NoopRawMutex, (JobStatus, JobStatusReason)>,
}

impl<'a, C: ControlInterface, E: StatusDetailsExt, const N: usize> JobUpdater<'a, C, E, N> {
    pub fn new(
        job: &'a JobContext<'a, E, N>,
        progress_state: &'a Mutex<NoopRawMutex, ProgressState<E, N>>,
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
            progress.apply_status(status, reason);
            self.control
                .update_job_status(self.job, &progress, status, reason)
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
        progress.apply_status(status, reason);

        self.control
            .update_job_status(self.job, &progress, status, reason)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;
    use std::{
        collections::BTreeMap,
        string::{String, ToString},
        vec::Vec,
    };

    use super::*;
    use crate::transfer::{
        data_interface::Protocol,
        encoding::{FileType, json::Signature},
        error::TransferError,
        pal::{BlockWriter, ImageState, OtaEvent, OtaPal, PalError, PalImageState, TransferPal},
    };

    const BLOCK_SIZE: usize = 4;

    /// Records each update as its status plus the serialized `statusDetails`.
    #[derive(Default)]
    struct MockControl {
        updates: RefCell<Vec<(JobStatus, String)>>,
    }

    impl ControlInterface for MockControl {
        async fn request_job(&self) -> Result<(), TransferError> {
            Ok(())
        }

        async fn update_job_status<E: StatusDetailsExt, const N: usize>(
            &self,
            _job: &JobContext<'_, E, N>,
            progress: &ProgressState<E, N>,
            status: JobStatus,
            _reason: JobStatusReason,
        ) -> Result<(), TransferError> {
            let details = progress
                .status_details
                .with_extra(&progress.extra_status)
                .with_files(&progress.files);
            let json = serde_json_core::to_string::<_, 2048>(&details).unwrap();
            self.updates.borrow_mut().push((status, json.to_string()));
            Ok(())
        }
    }

    /// Serves each file's content sequentially, keyed by `fileid`.
    struct MockData {
        files: BTreeMap<u8, Vec<u8>>,
        log: RefCell<Vec<String>>,
    }

    struct MockTransfer<'t> {
        data: &'t [u8],
        file_id: u8,
        next_block: usize,
        log: &'t RefCell<Vec<String>>,
    }

    struct MockRaw<'b> {
        file_id: u8,
        block_id: usize,
        payload: &'b [u8],
    }

    impl RawBlock for MockRaw<'_> {
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

    impl BlockTransfer for MockTransfer<'_> {
        type RawBlock<'b>
            = MockRaw<'b>
        where
            Self: 'b;

        async fn next_block(&mut self) -> Result<Option<Self::RawBlock<'_>>, TransferError> {
            // Yield like a real transport, so the engine's status-update
            // future gets polled between blocks.
            embassy_futures::yield_now().await;
            let start = self.next_block * BLOCK_SIZE;
            let end = (start + BLOCK_SIZE).min(self.data.len());
            let raw = MockRaw {
                file_id: self.file_id,
                block_id: self.next_block,
                payload: &self.data[start..end],
            };
            self.next_block += 1;
            Ok(Some(raw))
        }

        async fn on_block_written(
            &mut self,
            _progress: &BlockProgress,
        ) -> Result<(), TransferError> {
            Ok(())
        }

        async fn close(self) -> Result<(), TransferError> {
            self.log
                .borrow_mut()
                .push(format!("close:{}", self.file_id));
            Ok(())
        }
    }

    impl DataInterface for MockData {
        const PROTOCOL: Protocol = Protocol::Mqtt;

        type Transfer<'t>
            = MockTransfer<'t>
        where
            Self: 't;

        async fn begin_transfer<const N: usize>(
            &self,
            _job: &JobContext<'_, impl StatusDetailsExt, N>,
            file: &FileContext<'_>,
            _config: &config::Config,
            _progress: &BlockProgress,
        ) -> Result<Self::Transfer<'_>, TransferError> {
            self.log.borrow_mut().push(format!("begin:{}", file.fileid));
            Ok(MockTransfer {
                data: &self.files[&file.fileid],
                file_id: file.fileid,
                next_block: 0,
                log: &self.log,
            })
        }
    }

    struct MemWriter(Vec<u8>);

    impl BlockWriter for MemWriter {
        type Error = ();

        async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
            let offset = offset as usize;
            if self.0.len() < offset + data.len() {
                self.0.resize(offset + data.len(), 0);
            }
            self.0[offset..offset + data.len()].copy_from_slice(data);
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockPal {
        writer: Option<MemWriter>,
        closed: BTreeMap<u8, Vec<u8>>,
        fail_close: Option<u8>,
        pending_commit: bool,
        fail_self_test: Option<PalError>,
        hang_self_test: bool,
        fail_testing_state: bool,
        fail_activate: Option<PalError>,
        events: Vec<String>,
    }

    impl TransferPal for MockPal {
        type BlockWriter = MemWriter;
        type StatusDetails = ();

        fn status_details(&self) -> Self::StatusDetails {}

        async fn create_file_for_rx(
            &mut self,
            file: &FileContext<'_>,
        ) -> Result<&mut Self::BlockWriter, PalError> {
            self.events.push(format!("create:{}", file.fileid));
            Ok(self.writer.insert(MemWriter(Vec::new())))
        }

        async fn close_file(&mut self, file: &FileContext<'_>) -> Result<(), PalError> {
            self.events.push(format!("close:{}", file.fileid));
            if self.fail_close == Some(file.fileid) {
                return Err(PalError::SignatureCheckFailed);
            }
            let writer = self.writer.take().ok_or(PalError::BadFileHandle)?;
            self.closed.insert(file.fileid, writer.0);
            Ok(())
        }

        async fn abort(&mut self, job_name: &str) -> Result<(), PalError> {
            self.events.push(format!("abort:{}", job_name));
            self.closed.clear();
            Ok(())
        }
    }

    impl OtaPal for MockPal {
        async fn get_platform_image_state(&mut self) -> Result<PalImageState, PalError> {
            Ok(if self.pending_commit {
                PalImageState::PendingCommit
            } else {
                PalImageState::Valid
            })
        }

        async fn set_platform_image_state(&mut self, state: ImageState) -> Result<(), PalError> {
            self.events.push(format!("image_state:{:?}", state));
            if self.fail_testing_state && matches!(state, ImageState::Testing(_)) {
                return Err(PalError::BadImageState);
            }
            Ok(())
        }

        async fn reset_device(&mut self) -> Result<(), PalError> {
            self.events.push("reset".into());
            Ok(())
        }

        async fn complete_callback(&mut self, event: OtaEvent) -> Result<(), PalError> {
            self.events.push(format!("callback:{:?}", event));
            if event == OtaEvent::Activate {
                if let Some(e) = self.fail_activate {
                    return Err(e);
                }
            }
            if event == OtaEvent::StartTest {
                if self.hang_self_test {
                    core::future::pending::<()>().await;
                }
                if let Some(e) = self.fail_self_test {
                    return Err(e);
                }
            }
            Ok(())
        }
    }

    fn file(fileid: u8, filesize: usize, file_type: Option<FileType>) -> FileContext<'static> {
        FileContext {
            filepath: "file",
            filesize,
            fileid,
            certfile: None,
            update_data_url: None,
            auth_scheme: None,
            signature: Some(Signature::Sha256Ecdsa("sig")),
            file_type,
        }
    }

    fn job<const N: usize>(files: &[FileContext<'static>]) -> JobContext<'static, (), N> {
        JobContext {
            job_name: "job",
            files: heapless::Vec::from_slice(files).unwrap(),
            protocols: heapless::Vec::from_slice(&[Protocol::Mqtt]).unwrap(),
            stream_name: Some("stream"),
            status: StatusDetails::new(),
            extra_status: (),
        }
    }

    fn data(files: &[(u8, usize)]) -> MockData {
        MockData {
            files: files
                .iter()
                .map(|&(id, len)| (id, (0..len).map(|i| (i as u8) ^ id).collect()))
                .collect(),
            log: RefCell::new(Vec::new()),
        }
    }

    fn config() -> config::Config {
        config::Config {
            block_size: BLOCK_SIZE,
            status_update_frequency: 1000,
            ..Default::default()
        }
    }

    #[test]
    fn multi_file_downloads_in_order_then_succeeds_once() {
        let job = job::<2>(&[
            file(3, 10, Some(FileType::Other(1))),
            file(7, 5, Some(FileType::Other(2))),
        ]);
        let data = data(&[(3, 10), (7, 5)]);
        let control = MockControl::default();
        let mut pal = MockPal::default();

        embassy_futures::block_on(Transfer::perform(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ))
        .unwrap();

        assert_eq!(pal.events, ["create:3", "close:3", "create:7", "close:7"]);
        assert_eq!(
            *data.log.borrow(),
            ["begin:3", "close:3", "begin:7", "close:7"]
        );
        assert_eq!(pal.closed[&3], data.files[&3]);
        assert_eq!(pal.closed[&7], data.files[&7]);

        // Single terminal update, with progress spanning both files (3 + 2 blocks).
        assert_eq!(
            *control.updates.borrow(),
            [(
                JobStatus::Succeeded,
                r#"{"self_test":"accepted","progress":"5/5","files":"[{\"id\":3,\"status\":\"succeeded\",\"progress\":\"3/3\"},{\"id\":7,\"status\":\"succeeded\",\"progress\":\"2/2\"}]"}"#.to_string()
            )]
        );
    }

    #[test]
    fn activation_happens_once_after_all_files() {
        let job = job::<2>(&[
            file(0, 8, Some(FileType::Firmware)),
            file(1, 3, Some(FileType::Other(1))),
        ]);
        let data = data(&[(0, 8), (1, 3)]);
        let control = MockControl::default();
        let mut pal = MockPal::default();

        embassy_futures::block_on(Transfer::perform_ota(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ))
        .unwrap();

        assert_eq!(
            pal.events,
            [
                "create:0",
                "close:0",
                "create:1",
                "close:1",
                "callback:Activate"
            ]
        );
        assert_eq!(
            *control.updates.borrow(),
            [(
                JobStatus::InProgress,
                r#"{"self_test":"ready","progress":"3/3","files":"[{\"id\":0,\"status\":\"succeeded\",\"progress\":\"2/2\"},{\"id\":1,\"status\":\"succeeded\",\"progress\":\"1/1\"}]"}"#.to_string()
            )]
        );
    }

    #[test]
    fn failed_activation_rejects_the_update() {
        let job = job::<2>(&[
            file(0, 8, Some(FileType::Firmware)),
            file(1, 3, Some(FileType::Other(1))),
        ]);
        let data = data(&[(0, 8), (1, 3)]);
        let control = MockControl::default();
        let mut pal = MockPal {
            fail_activate: Some(PalError::Other),
            ..Default::default()
        };

        let result = embassy_futures::block_on(Transfer::perform_ota(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ));

        assert_eq!(result, Err(TransferError::Pal(PalError::Other)));
        assert_eq!(
            pal.events,
            [
                "create:0",
                "close:0",
                "create:1",
                "close:1",
                "callback:Activate",
                "image_state:Rejected(Pal(Other))",
                "callback:Fail"
            ]
        );
        let files = r#""files":"[{\"id\":0,\"status\":\"succeeded\",\"progress\":\"2/2\"},{\"id\":1,\"status\":\"succeeded\",\"progress\":\"1/1\"}]""#;
        assert_eq!(
            *control.updates.borrow(),
            [
                (
                    JobStatus::InProgress,
                    format!(r#"{{"self_test":"ready","progress":"3/3",{files}}}"#)
                ),
                (
                    JobStatus::Failed,
                    format!(
                        r#"{{"self_test":"rejected","progress":"3/3","reason":"other","error_code":1099,{files}}}"#
                    )
                ),
            ]
        );
    }

    #[test]
    fn failure_in_later_file_aborts_whole_job() {
        let job = job::<2>(&[
            file(0, 8, Some(FileType::Firmware)),
            file(1, 3, Some(FileType::Other(1))),
        ]);
        let data = data(&[(0, 8), (1, 3)]);
        let control = MockControl::default();
        let mut pal = MockPal {
            fail_close: Some(1),
            ..Default::default()
        };

        let result = embassy_futures::block_on(Transfer::perform_ota(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ));

        assert_eq!(
            result,
            Err(TransferError::Pal(PalError::SignatureCheckFailed))
        );
        assert_eq!(
            pal.events,
            [
                "create:0",
                "close:0",
                "create:1",
                "close:1",
                "abort:job",
                "callback:Fail"
            ]
        );
        assert!(pal.closed.is_empty());
        assert_eq!(
            *control.updates.borrow(),
            [(
                JobStatus::Failed,
                r#"{"self_test":"aborted","progress":"3/3","reason":"sig_check_failed","error_code":1001,"files":"[{\"id\":0,\"status\":\"succeeded\",\"progress\":\"2/2\"},{\"id\":1,\"status\":\"failed\",\"progress\":\"1/1\",\"reason\":\"sig_check_failed\",\"error_code\":1001}]"}"#.to_string()
            )]
        );
    }

    #[test]
    fn requires_activation_is_decided_by_the_pal() {
        struct ModelPal(MockPal);

        impl TransferPal for ModelPal {
            type BlockWriter = MemWriter;
            type StatusDetails = ();

            fn status_details(&self) -> Self::StatusDetails {}

            fn requires_activation(&self, file: &FileContext<'_>) -> bool {
                file.file_type == Some(FileType::Other(42))
            }

            async fn create_file_for_rx(
                &mut self,
                file: &FileContext<'_>,
            ) -> Result<&mut Self::BlockWriter, PalError> {
                self.0.create_file_for_rx(file).await
            }

            async fn close_file(&mut self, file: &FileContext<'_>) -> Result<(), PalError> {
                self.0.close_file(file).await
            }

            async fn abort(&mut self, job_name: &str) -> Result<(), PalError> {
                self.0.abort(job_name).await
            }
        }

        let job = job::<1>(&[file(0, 4, Some(FileType::Other(42)))]);
        let data = data(&[(0, 4)]);
        let control = MockControl::default();
        let mut pal = ModelPal(MockPal::default());

        embassy_futures::block_on(Transfer::perform(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ))
        .unwrap();

        assert_eq!(
            control.updates.borrow()[0].0,
            JobStatus::InProgress,
            "a file the PAL marks as requiring activation must leave the job IN_PROGRESS"
        );
    }

    #[test]
    fn empty_job_is_rejected() {
        let job = job::<1>(&[]);
        let data = data(&[]);
        let control = MockControl::default();
        let mut pal = MockPal::default();

        let result = embassy_futures::block_on(Transfer::perform(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ));

        assert_eq!(result, Err(TransferError::InvalidFile));
        assert_eq!(pal.events, ["abort:job"]);
    }

    #[test]
    fn single_file_job_reports_no_files_entry() {
        let job = job::<1>(&[file(0, 6, Some(FileType::Other(1)))]);
        let data = data(&[(0, 6)]);
        let control = MockControl::default();
        let mut pal = MockPal::default();

        embassy_futures::block_on(Transfer::perform(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ))
        .unwrap();

        assert_eq!(
            *control.updates.borrow(),
            [(
                JobStatus::Succeeded,
                r#"{"self_test":"accepted","progress":"2/2"}"#.to_string()
            )]
        );
    }

    /// Progress must reflect the completed download on `SUCCEEDED`, not the
    /// last periodic update.
    #[test]
    fn succeeded_reports_final_progress() {
        let job = job::<1>(&[file(0, 40, Some(FileType::Other(1)))]);
        let data = data(&[(0, 40)]);
        let control = MockControl::default();
        let mut pal = MockPal::default();
        let config = config::Config {
            block_size: BLOCK_SIZE,
            status_update_frequency: 4,
            ..Default::default()
        };

        embassy_futures::block_on(Transfer::perform(&control, &data, &job, &mut pal, &config))
            .unwrap();

        let updates = control.updates.borrow();
        let (status, last) = updates.last().unwrap();
        assert_eq!(*status, JobStatus::Succeeded);
        assert_eq!(last, r#"{"self_test":"accepted","progress":"10/10"}"#);
        // Periodic updates were sent while streaming, so the final one matters.
        assert!(updates.len() > 1);
    }

    /// After the activation reboot progress is not restored, but every
    /// update in the self-test must still report a completed download.
    #[test]
    fn self_test_reports_completed_download() {
        let mut job = job::<2>(&[
            file(0, 8, Some(FileType::Firmware)),
            file(1, 3, Some(FileType::Other(1))),
        ]);
        job.status.set_self_test("ready");
        let data = data(&[]);
        let control = MockControl::default();
        let mut pal = MockPal {
            pending_commit: true,
            ..Default::default()
        };

        embassy_futures::block_on(Transfer::perform_ota(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ))
        .unwrap();

        let files = r#""files":"[{\"id\":0,\"status\":\"succeeded\",\"progress\":\"2/2\"},{\"id\":1,\"status\":\"succeeded\",\"progress\":\"1/1\"}]""#;
        assert_eq!(
            *control.updates.borrow(),
            [
                (
                    JobStatus::InProgress,
                    format!(r#"{{"self_test":"active","progress":"3/3",{files}}}"#)
                ),
                (
                    JobStatus::Succeeded,
                    format!(r#"{{"self_test":"accepted","progress":"3/3",{files}}}"#)
                ),
            ]
        );
        assert_eq!(
            pal.events,
            ["image_state:Testing(VersionCheck)", "callback:StartTest"]
        );
    }

    fn self_test_job() -> JobContext<'static, (), 2> {
        let mut job = job::<2>(&[
            file(0, 8, Some(FileType::Firmware)),
            file(1, 3, Some(FileType::Other(1))),
        ]);
        job.status.set_self_test("ready");
        job
    }

    #[test]
    fn failed_self_test_rejects_and_resets() {
        let job = self_test_job();
        let data = data(&[]);
        let control = MockControl::default();
        let mut pal = MockPal {
            pending_commit: true,
            fail_self_test: Some(PalError::CommitFailed),
            ..Default::default()
        };

        let result = embassy_futures::block_on(Transfer::perform_ota(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ));

        assert_eq!(result, Err(TransferError::ResetFailed));
        // The downloads themselves succeeded; only the job is rejected.
        let files = r#""files":"[{\"id\":0,\"status\":\"succeeded\",\"progress\":\"2/2\"},{\"id\":1,\"status\":\"succeeded\",\"progress\":\"1/1\"}]""#;
        assert_eq!(
            *control.updates.borrow(),
            [
                (
                    JobStatus::InProgress,
                    format!(r#"{{"self_test":"active","progress":"3/3",{files}}}"#)
                ),
                (
                    JobStatus::Failed,
                    format!(
                        r#"{{"self_test":"rejected","progress":"3/3","reason":"commit_failed","error_code":1008,{files}}}"#
                    )
                ),
            ]
        );
        assert_eq!(
            pal.events,
            [
                "image_state:Testing(VersionCheck)",
                "callback:StartTest",
                "image_state:Rejected(Pal(CommitFailed))",
                "callback:SelfTestFailed",
                "reset"
            ]
        );
    }

    #[test]
    fn self_test_timeout_rejects_and_resets() {
        let job = self_test_job();
        let data = data(&[]);
        let control = MockControl::default();
        let mut pal = MockPal {
            pending_commit: true,
            hang_self_test: true,
            ..Default::default()
        };
        let config = config::Config {
            self_test_timeout: Some(embassy_time::Duration::from_millis(10)),
            ..config()
        };

        let result = embassy_futures::block_on(Transfer::perform_ota(
            &control, &data, &job, &mut pal, &config,
        ));

        assert_eq!(result, Err(TransferError::ResetFailed));
        // The downloads themselves succeeded; only the job is rejected.
        let files = r#""files":"[{\"id\":0,\"status\":\"succeeded\",\"progress\":\"2/2\"},{\"id\":1,\"status\":\"succeeded\",\"progress\":\"1/1\"}]""#;
        assert_eq!(
            *control.updates.borrow(),
            [
                (
                    JobStatus::InProgress,
                    format!(r#"{{"self_test":"active","progress":"3/3",{files}}}"#)
                ),
                (
                    JobStatus::Failed,
                    format!(
                        r#"{{"self_test":"rejected","progress":"3/3","reason":"self_test_timeout","error_code":2008,{files}}}"#
                    )
                ),
            ]
        );
        assert_eq!(
            pal.events,
            [
                "image_state:Testing(VersionCheck)",
                "callback:StartTest",
                "image_state:Rejected(SelfTestTimeout)",
                "callback:SelfTestFailed",
                "reset"
            ]
        );
    }

    /// A self-test that cannot start must reject the update once, not report
    /// FAILED and then run the test and report SUCCEEDED.
    #[test]
    fn self_test_that_cannot_start_rejects_once() {
        let job = self_test_job();
        let data = data(&[]);
        let control = MockControl::default();
        let mut pal = MockPal {
            pending_commit: true,
            fail_testing_state: true,
            ..Default::default()
        };

        let result = embassy_futures::block_on(Transfer::perform_ota(
            &control,
            &data,
            &job,
            &mut pal,
            &config(),
        ));

        assert_eq!(result, Err(TransferError::ResetFailed));
        let files = r#""files":"[{\"id\":0,\"status\":\"succeeded\",\"progress\":\"2/2\"},{\"id\":1,\"status\":\"succeeded\",\"progress\":\"1/1\"}]""#;
        assert_eq!(
            *control.updates.borrow(),
            [(
                JobStatus::Failed,
                format!(
                    r#"{{"self_test":"rejected","progress":"3/3","reason":"bad_image_state","error_code":1007,{files}}}"#
                )
            )]
        );
        assert_eq!(
            pal.events,
            [
                "image_state:Testing(VersionCheck)",
                "image_state:Rejected(Pal(BadImageState))",
                "callback:SelfTestFailed",
                "reset"
            ]
        );
    }

    #[test]
    fn incoming_files_key_is_not_passed_to_extension() {
        use crate::transfer::encoding::afr_ota::OtaJob;

        #[derive(Default)]
        struct Ext(Vec<String>);

        impl StatusDetailsExt for Ext {
            const MAX_EXTRA_JSON_SIZE: usize = 0;

            fn serialize_into_map<S: serde::ser::SerializeMap>(
                &self,
                _map: &mut S,
            ) -> Result<(), S::Error> {
                Ok(())
            }

            fn accept_entry(&mut self, key: &str, _value: &str) -> bool {
                self.0.push(key.into());
                true
            }
        }

        let (doc, _) = serde_json_core::from_str::<OtaJob<2>>(
            r#"{"protocols":["MQTT"],"files":[{"filepath":"a","filesize":1,"fileid":0},{"filepath":"b","filesize":1,"fileid":1}]}"#,
        )
        .unwrap();
        let mut details = crate::jobs::StatusDetails::new();
        details.insert("files", "[]").unwrap();
        details.insert("custom", "x").unwrap();

        let ctx = JobContext::<Ext, 2>::new(&doc, "job", Some(&details)).unwrap();
        assert_eq!(ctx.extra_status.0, ["custom"]);
    }
}
