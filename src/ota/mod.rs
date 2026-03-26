pub mod config;
pub mod control_interface;
pub mod data_interface;
pub mod encoding;
pub mod error;
pub mod pal;
pub mod status_details;

use core::future::Future;

#[cfg(feature = "ota_mqtt_data")]
pub use data_interface::mqtt::{Encoding, Topic};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
pub use status_details::{OtaStatusDetails, StatusDetailsExt};

use crate::{jobs::data_types::JobStatus, ota::encoding::json::JobStatusReason};

use self::{
    control_interface::ControlInterface,
    data_interface::{BlockProgress, BlockTransfer, DataInterface, FileBlock, RawBlock},
    encoding::{Bitmap, OtaJobContext},
    pal::{ImageState, ImageStateReason},
};

#[derive(PartialEq)]
pub struct JobEventData<'a> {
    pub job_name: &'a str,
    pub ota_document: encoding::json::OtaJob<'a>,
    pub status_details: Option<crate::jobs::StatusDetails<'a>>,
}

enum IngestResult {
    Complete,
    Ingested,
    Duplicate,
}

pub struct Updater;

impl Updater {
    pub fn check_for_job<C: ControlInterface>(
        control: &C,
    ) -> impl Future<Output = Result<(), error::OtaError>> + '_ {
        control.request_job()
    }

    pub async fn perform_ota<C: ControlInterface, D: DataInterface, P: pal::OtaPal>(
        control: &C,
        data: &D,
        job: &OtaJobContext<'_, P::StatusDetails>,
        pal: &mut P,
        config: &config::Config,
    ) -> Result<(), error::OtaError> {
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

        // Create the JobUpdater
        let mut job_updater = JobUpdater::new(job, &progress_state, config, control);

        match job_updater.initialize::<D, _>(pal).await? {
            Some(()) => {}
            None => return Ok(()),
        };

        info!("Job document was accepted. Attempting to begin the update");

        // Spawn the status update future
        let status_update_fut = job_updater.handle_status_updates();

        // Spawn the data handling future
        let data_fut = async {
            // Create/Open the OTA file on the file system
            let block_writer = match pal.create_file_for_rx(job).await {
                Ok(block_writer) => block_writer,
                Err(e) => {
                    job_updater
                        .set_image_state_with_reason(
                            pal,
                            ImageState::Aborted(ImageStateReason::Pal(e)),
                        )
                        .await?;

                    pal.close_file(job).await?;
                    return Err(e.into());
                }
            };

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
                //
                // The block processing is split into two phases to satisfy
                // the borrow checker: the match on `transfer.next_block()`
                // borrows `transfer`, so `on_block_written` must be called
                // after the match scope ends.
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
                                                match pal.close_file(job).await {
                                                    Err(e) => {
                                                        return Err(e.into());
                                                    }
                                                    Ok(_) if job.file_type == Some(0) => {
                                                        job_updater.signal_update(
                                                            JobStatus::InProgress,
                                                            JobStatusReason::SigCheckPassed,
                                                        );
                                                        return Ok(());
                                                    }
                                                    Ok(_) => {
                                                        job_updater.signal_update(
                                                            JobStatus::Succeeded,
                                                            JobStatusReason::Accepted,
                                                        );
                                                        return Ok(());
                                                    }
                                                }
                                            }
                                            Ok(IngestResult::Ingested) => {
                                                // Block ingested — signal status if milestone
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
                                            Ok(IngestResult::Duplicate) => {
                                                // Don't notify transfer — bitmap unchanged,
                                                // batch counter should not decrement
                                                None
                                            }
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
                                warn!("[OTA] Data stream ended (clean session/disconnect). Re-establishing and resuming...");

                                let blocks_remaining = {
                                    let progress = progress_state.lock().await;
                                    progress.blocks_remaining
                                };

                                info!("[OTA] Resuming OTA: {} blocks remaining", blocks_remaining);

                                // Break inner loop to trigger re-establishment
                                break;
                            }

                            Err(e) if e.is_retryable() => {
                                // Momentum timeout or transient error — retry
                                continue;
                            }
                            Err(e) => {
                                error!("Block transfer error: {:?}", e);
                                return Err(e);
                            }
                        }
                    };

                    // Notify the transfer outside the match scope so the
                    // borrow from next_block() is released.
                    if let Some(bp) = notify_progress {
                        transfer.on_block_written(&bp).await?;
                    }
                } // End of inner block processing loop
            } // End of outer re-establish loop
        };

        // select: when data_fut completes, status_update_fut is dropped.
        // The final status update is sent directly below, not via the
        // signal/handler — the handler loop would block forever otherwise.
        let data_res = match embassy_futures::select::select(data_fut, status_update_fut).await {
            embassy_futures::select::Either::First(res) => res,
            embassy_futures::select::Either::Second(res) => {
                // status_update_fut returned first — this means a status publish
                // error occurred. Propagate it.
                return res;
            }
        };

        // Cleanup and update the job status accordingly
        match data_res {
            Ok(()) => {
                let event = if let Some(0) = job.file_type {
                    pal::OtaEvent::Activate
                } else {
                    pal::OtaEvent::UpdateComplete
                };

                // The status_update_fut was dropped when data_fut completed,
                // so the signalled update was never sent. Send the final status
                // directly and wait for the cloud to accept it before rebooting.
                let (status, reason) = if let Some(0) = job.file_type {
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
            Err(error::OtaError::MomentumAbort) => {
                warn!("[OTA] Momentum abort triggered");
                job_updater
                    .set_image_state_with_reason(
                        pal,
                        ImageState::Aborted(ImageStateReason::MomentumAbort),
                    )
                    .await?;

                Err(error::OtaError::MomentumAbort)
            }
            Err(error::OtaError::UserAbort) => {
                warn!("[OTA] Job cancelled (detected via notification)");
                job_updater
                    .set_image_state_with_reason(
                        pal,
                        ImageState::Aborted(ImageStateReason::UserAbort),
                    )
                    .await?;

                Err(error::OtaError::UserAbort)
            }
            Err(e) => {
                // Signal the error status, preserving the failure reason if available
                let reason = match &e {
                    error::OtaError::Pal(pal_err) => {
                        JobStatusReason::Aborted(Some(ImageStateReason::Pal(*pal_err)))
                    }
                    _ => JobStatusReason::Aborted(None),
                };
                job_updater
                    .update_job_status(JobStatus::Failed, reason)
                    .await?;

                pal.complete_callback(pal::OtaEvent::Fail).await?;
                info!("Application callback! OtaEvent::Fail");

                Err(e)
            }
        }
    }

    async fn ingest_data_block<E: StatusDetailsExt>(
        block: &FileBlock<'_>,
        block_writer: &mut impl pal::BlockWriter,
        config: &config::Config,
        progress: &mut ProgressState<E>,
    ) -> Result<IngestResult, error::OtaError> {
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
                .map_err(|e| {
                    error!("Block write failed: {:?}", e);
                    error::OtaError::WriteFailed
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

            Err(error::OtaError::BlockOutOfRange)
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
    pub status_details: OtaStatusDetails,
    pub extra_status: E,
}

pub struct JobUpdater<'a, C: ControlInterface, E: StatusDetailsExt = ()> {
    pub job: &'a OtaJobContext<'a, E>,
    pub progress_state: &'a Mutex<NoopRawMutex, ProgressState<E>>,
    pub config: &'a config::Config,
    pub control: &'a C,
    pub status_update_signal: Signal<NoopRawMutex, (JobStatus, JobStatusReason)>,
}

impl<'a, C: ControlInterface, E: StatusDetailsExt> JobUpdater<'a, C, E> {
    pub fn new(
        job: &'a OtaJobContext<'a, E>,
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

    async fn initialize<D: DataInterface, PAL: pal::OtaPal>(
        &mut self,
        pal: &mut PAL,
    ) -> Result<Option<()>, error::OtaError> {
        // If the job is in self test mode, don't start an OTA update but
        // instead do the following:
        //
        // If the firmware that performed the update was older than the
        // currently running firmware, set the image state to "Testing." This is
        // the success path.
        //
        // If it's the same or newer, reject the job since either the firmware
        // was not accepted during self test or an incorrect image was sent by
        // the OTA operator.
        let platform_self_test = pal
            .get_platform_image_state()
            .await
            .is_ok_and(|i| i == pal::PalImageState::PendingCommit);

        match (self.job.self_test(), platform_self_test) {
            (true, true) => {
                // Run self-test!
                self.set_image_state_with_reason(
                    pal,
                    ImageState::Testing(ImageStateReason::VersionCheck),
                )
                .await?;

                info!("Beginning self-test");

                let test_fut = pal.complete_callback(pal::OtaEvent::StartTest);

                match self.config.self_test_timeout {
                    Some(timeout) => embassy_time::with_timeout(timeout, test_fut)
                        .await
                        .map_err(|_| error::OtaError::Timeout)?,
                    None => test_fut.await,
                }?;

                let mut progress = self.progress_state.lock().await;
                self.control
                    .update_job_status(
                        self.job,
                        &mut progress,
                        JobStatus::Succeeded,
                        JobStatusReason::Accepted,
                    )
                    .await?;

                return Ok(None);
            }
            (false, false) => {}
            (false, true) => {
                // Received a job that is not in self-test but platform is, so
                // reboot the device to allow roll back to previous image.
                error!("Rejecting new image and rebooting: The platform is in the self-test state while the job is not.");
                pal.reset_device().await?;
                return Err(error::OtaError::ResetFailed);
            }
            (true, false) => {
                // The job is in self test but the platform image state is not so it
                // could be an attack on the platform image state. Reject the update
                // (this should also cause the image to be erased), aborting the job
                // and reset the device.
                error!("Rejecting new image and rebooting: the job is in the self-test state while the platform is not.");
                self.set_image_state_with_reason(
                    pal,
                    ImageState::Rejected(ImageStateReason::ImageStateMismatch),
                )
                .await?;

                pal.reset_device().await?;
                return Err(error::OtaError::ResetFailed);
            }
        }

        if !self.job.protocols.contains(&D::PROTOCOL) {
            error!("Unable to handle current OTA job with given data interface ({:?}). Supported protocols: {:?}. Aborting current update.", D::PROTOCOL, self.job.protocols);
            self.set_image_state_with_reason(
                pal,
                ImageState::Aborted(ImageStateReason::InvalidDataProtocol),
            )
            .await?;
            return Err(error::OtaError::InvalidInterface);
        }

        Ok(Some(()))
    }

    async fn handle_status_updates(&self) -> Result<(), error::OtaError> {
        loop {
            // Wait for a signal from the main loop
            let (status, reason) = self.status_update_signal.wait().await;

            // Update the job status based on the signal
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

    async fn set_image_state_with_reason<PAL: pal::OtaPal>(
        &self,
        pal: &mut PAL,
        image_state: ImageState,
    ) -> Result<(), error::OtaError> {
        // Call the platform specific code to set the image state
        let image_state = match pal.set_platform_image_state(image_state).await {
            Err(e) if !matches!(image_state, ImageState::Aborted(_)) => {
                ImageState::Rejected(ImageStateReason::Pal(e))
            }
            _ => image_state,
        };

        // Now update the image state and job status on server side
        let mut progress = self.progress_state.lock().await;

        match image_state {
            ImageState::Testing(_) => {
                self.control
                    .update_job_status(
                        self.job,
                        &mut progress,
                        JobStatus::InProgress,
                        JobStatusReason::SelfTestActive,
                    )
                    .await?;
            }
            ImageState::Accepted => {
                self.control
                    .update_job_status(
                        self.job,
                        &mut progress,
                        JobStatus::Succeeded,
                        JobStatusReason::Accepted,
                    )
                    .await?;
            }
            ImageState::Rejected(reason) => {
                self.control
                    .update_job_status(
                        self.job,
                        &mut progress,
                        JobStatus::Failed,
                        JobStatusReason::Rejected(Some(reason)),
                    )
                    .await?;
            }
            ImageState::Aborted(reason) => {
                self.control
                    .update_job_status(
                        self.job,
                        &mut progress,
                        JobStatus::Failed,
                        JobStatusReason::Aborted(Some(reason)),
                    )
                    .await?;
            }
            ImageState::Unknown => {
                self.control
                    .update_job_status(
                        self.job,
                        &mut progress,
                        JobStatus::Failed,
                        JobStatusReason::Aborted(None),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    // Function to signal the status update future
    pub fn signal_update(&self, status: JobStatus, reason: JobStatusReason) {
        self.status_update_signal.signal((status, reason));
    }

    // Function to update the job status
    pub async fn update_job_status(
        &mut self,
        status: JobStatus,
        reason: JobStatusReason,
    ) -> Result<(), error::OtaError> {
        let mut progress = self.progress_state.lock().await;

        self.control
            .update_job_status(self.job, &mut progress, status, reason)
            .await?;
        Ok(())
    }
}
