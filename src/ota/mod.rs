pub mod config;
pub mod control_interface;
pub mod data_interface;
pub mod encoding;
pub mod error;
pub mod pal;

use core::ops::DerefMut as _;

#[cfg(feature = "ota_mqtt_data")]
pub use data_interface::mqtt::{Encoding, Topic};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use embedded_storage_async::nor_flash::{NorFlash, NorFlashError as _};

use crate::{
    jobs::{data_types::JobStatus, StatusDetailsOwned},
    ota::{data_interface::BlockTransfer, encoding::json::JobStatusReason},
};

use self::{
    control_interface::ControlInterface,
    data_interface::DataInterface,
    encoding::{Bitmap, FileContext},
    pal::{ImageState, ImageStateReason},
};

#[derive(PartialEq)]
pub struct JobEventData<'a> {
    pub job_name: &'a str,
    pub ota_document: encoding::json::OtaJob<'a>,
    pub status_details: Option<crate::jobs::StatusDetails<'a>>,
}

pub struct Updater;

impl Updater {
    pub async fn check_for_job<'a, C: ControlInterface>(
        control: &C,
    ) -> Result<(), error::OtaError> {
        control.request_job().await?;
        Ok(())
    }

    pub async fn perform_ota<'a, 'b, C: ControlInterface, D: DataInterface>(
        control: &C,
        data: &D,
        file_ctx: FileContext,
        pal: &mut impl pal::OtaPal,
        config: &config::Config,
    ) -> Result<(), error::OtaError> {
        let progress_state = Mutex::new(ProgressState {
            total_blocks: (file_ctx.filesize + config.block_size - 1) / config.block_size,
            blocks_remaining: (file_ctx.filesize + config.block_size - 1) / config.block_size,
            block_offset: file_ctx.block_offset,
            request_block_remaining: file_ctx.bitmap.len() as u32,
            bitmap: file_ctx.bitmap.clone(),
            file_size: file_ctx.filesize,
            request_momentum: None,
            status_details: file_ctx.status_details.clone(),
        });

        // Create the JobUpdater
        let mut job_updater = JobUpdater::new(&file_ctx, &progress_state, config, control);

        match job_updater.initialize::<D, _>(pal).await? {
            Some(()) => {}
            None => return Ok(()),
        };

        info!("Job document was accepted. Attempting to begin the update");

        // Spawn the request momentum future
        let momentum_fut = Self::handle_momentum(data, config, &file_ctx, &progress_state);

        // Spawn the status update future
        let status_update_fut = job_updater.handle_status_updates();

        // Spawn the data handling future
        let data_fut = async {
            // Create/Open the OTA file on the file system
            let mut block_writer = match pal.create_file_for_rx(&file_ctx).await {
                Ok(block_writer) => block_writer,
                Err(e) => {
                    job_updater
                        .set_image_state_with_reason(
                            pal,
                            ImageState::Aborted(ImageStateReason::Pal(e)),
                        )
                        .await?;

                    pal.close_file(&file_ctx).await?;
                    return Err(e.into());
                }
            };

            info!("Initialized file handler! Requesting file blocks");

            // Prepare the storage layer on receiving a new file
            let mut subscription = data.init_file_transfer(&file_ctx).await?;

            {
                let mut progress = progress_state.lock().await;
                data.request_file_blocks(&file_ctx, &mut progress, config)
                    .await?;
            }

            info!("Awaiting file blocks!");

            loop {
                // Select over the futures
                match subscription.next_block().await {
                    Ok(Some(mut payload)) => {
                        // Decode the file block received
                        let mut progress = progress_state.lock().await;

                        match Self::ingest_data_block(
                            data,
                            &mut block_writer,
                            config,
                            &mut progress,
                            payload.deref_mut(),
                        )
                        .await
                        {
                            Ok(true) => {
                                // ... (Handle end of file) ...
                                match pal.close_file(&file_ctx).await {
                                    Err(e) => {
                                        // FIXME: This seems like duplicate status update, as it will also report during cleanup
                                        // job_updater.signal_update(
                                        //     JobStatus::Failed,
                                        //     JobStatusReason::Pal(0),
                                        // );

                                        return Err(e.into());
                                    }
                                    Ok(_) if file_ctx.file_type == Some(0) => {
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
                            Ok(false) => {
                                // ... (Handle successful block processing) ...
                                progress.request_momentum = Some(0);

                                // Update the job status to reflect the download progress
                                if progress.blocks_remaining
                                    % config.status_update_frequency as usize
                                    == 0
                                {
                                    job_updater.signal_update(
                                        JobStatus::InProgress,
                                        JobStatusReason::Receiving,
                                    );
                                }

                                if progress.request_block_remaining > 1 {
                                    progress.request_block_remaining -= 1;
                                } else {
                                    data.request_file_blocks(&file_ctx, &mut progress, config)
                                        .await?;
                                }
                            }
                            Err(e) if e.is_retryable() => {
                                // ... (Handle retryable errors) ...
                                error!("Failed block validation: {:?}! Retrying", e);
                            }
                            Err(e) => {
                                // ... (Handle fatal errors) ...
                                return Err(e);
                            }
                        }
                    }
                    Ok(None) => {
                        error!("Stream ended unexpectedly");
                        // Handle the case where next_block returns None,
                        // this might mean the stream has ended unexpectedly.
                        todo!();
                    }

                    // Handle status update future results
                    Err(e) => {
                        error!("Status update error: {:?}", e);
                        // Handle the error appropriately.
                        todo!();
                    }
                }
            }
        };

        let (data_res, _) = embassy_futures::join::join(
            data_fut,
            embassy_futures::select::select(status_update_fut, momentum_fut),
        )
        .await;

        // Cleanup and update the job status accordingly
        match data_res {
            Ok(()) => {
                let event = if let Some(0) = file_ctx.file_type {
                    pal::OtaEvent::Activate
                } else {
                    pal::OtaEvent::UpdateComplete
                };

                info!(
                    "OTA Download finished! Running complete callback: {:?}",
                    event
                );

                pal.complete_callback(event).await?;

                Ok(())
            }
            Err(error::OtaError::MomentumAbort) => {
                job_updater
                    .set_image_state_with_reason(
                        pal,
                        ImageState::Aborted(ImageStateReason::MomentumAbort),
                    )
                    .await?;

                Err(error::OtaError::MomentumAbort)
            }
            Err(e) => {
                // Signal the error status
                job_updater
                    .update_job_status(JobStatus::Failed, JobStatusReason::Pal(0))
                    .await?;

                pal.complete_callback(pal::OtaEvent::Fail).await?;
                info!("Application callback! OtaEvent::Fail");

                Err(e)
            }
        }
    }

    async fn ingest_data_block<'a, D: DataInterface>(
        data: &D,
        block_writer: &mut impl NorFlash,
        config: &config::Config,
        progress: &mut ProgressState,
        payload: &mut [u8],
    ) -> Result<bool, error::OtaError> {
        let block = data.decode_file_block(payload)?;

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

                // Just return same progress as before
                return Ok(false);
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
                .map_err(|e| error::OtaError::Write(e.kind()))?;

            let block_offset = progress.block_offset;
            progress
                .bitmap
                .set(block.block_id - block_offset as usize, false);

            progress.blocks_remaining -= 1;

            if progress.blocks_remaining == 0 {
                info!("Received final expected block of file.");

                // Return true to indicate end of file.
                Ok(true)
            } else {
                if progress.bitmap.is_empty() {
                    progress.block_offset += 31;
                    progress.bitmap = encoding::Bitmap::new(
                        progress.file_size,
                        config.block_size,
                        progress.block_offset,
                    );
                }

                Ok(false)
            }
        } else {
            error!(
                "Error! Block {:?} out of expected range! Size {:?}",
                block.block_id, block.block_size
            );

            Err(error::OtaError::BlockOutOfRange)
        }
    }

    async fn handle_momentum<D: DataInterface>(
        data: &D,
        config: &config::Config,
        file_ctx: &FileContext,
        progress_state: &Mutex<NoopRawMutex, ProgressState>,
    ) -> Result<(), error::OtaError> {
        loop {
            embassy_time::Timer::after(config.request_wait).await;

            let mut progress = progress_state.lock().await;

            if progress.blocks_remaining == 0 {
                // No more blocks to request
                break;
            }

            let Some(request_momentum) = &mut progress.request_momentum else {
                continue;
            };

            // Increment momentum
            *request_momentum += 1;

            if *request_momentum == 1 {
                continue;
            }

            if *request_momentum <= config.max_request_momentum {
                warn!("Momentum requesting more blocks!");

                // Request data blocks
                data.request_file_blocks(file_ctx, &mut progress, config)
                    .await?;
            } else {
                // Too much momentum, abort
                return Err(error::OtaError::MomentumAbort);
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ProgressState {
    pub total_blocks: usize,
    pub blocks_remaining: usize,
    pub file_size: usize,
    pub block_offset: u32,
    pub request_block_remaining: u32,
    pub request_momentum: Option<u8>,
    #[cfg_attr(feature = "defmt", defmt(Debug2Format))]
    pub bitmap: Bitmap,
    #[cfg_attr(feature = "defmt", defmt(Debug2Format))]
    pub status_details: StatusDetailsOwned,
}

pub struct JobUpdater<'a, C: ControlInterface> {
    pub file_ctx: &'a FileContext,
    pub progress_state: &'a Mutex<NoopRawMutex, ProgressState>,
    pub config: &'a config::Config,
    pub control: &'a C,
    pub status_update_signal: Signal<NoopRawMutex, (JobStatus, JobStatusReason)>,
}

impl<'a, C: ControlInterface> JobUpdater<'a, C> {
    pub fn new(
        file_ctx: &'a FileContext,
        progress_state: &'a Mutex<NoopRawMutex, ProgressState>,
        config: &'a config::Config,
        control: &'a C,
    ) -> Self {
        Self {
            file_ctx,
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
            .map_or(false, |i| i == pal::PalImageState::PendingCommit);

        match (self.file_ctx.self_test(), platform_self_test) {
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
                        self.file_ctx,
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

        if !self.file_ctx.protocols.contains(&D::PROTOCOL) {
            error!("Unable to handle current OTA job with given data interface ({:?}). Supported protocols: {:?}. Aborting current update.", D::PROTOCOL, self.file_ctx.protocols);
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
                .update_job_status(self.file_ctx, &mut progress, status, reason)
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
                // If the platform image state couldn't be set correctly, force
                // fail the update by setting the image state to "Rejected"
                // unless it's already in "Aborted".

                // Capture the failure reason if not already set (and we're not
                // already Aborted as checked above). Otherwise Keep the
                // original reject reason code since it is possible for the PAL
                // to fail to update the image state in some cases (e.g. a reset
                // already caused the bundle rollback and we failed to rollback
                // again).

                // Intentionally override reason since we failed within this
                // function
                ImageState::Rejected(ImageStateReason::Pal(e))
            }
            _ => image_state,
        };

        // Now update the image state and job status on server side
        let mut progress = self.progress_state.lock().await;

        match image_state {
            ImageState::Testing(_) => {
                // We discovered we're ready for test mode, put job status
                // in self_test active
                self.control
                    .update_job_status(
                        self.file_ctx,
                        &mut progress,
                        JobStatus::InProgress,
                        JobStatusReason::SelfTestActive,
                    )
                    .await?;
            }
            ImageState::Accepted => {
                // Now that we have accepted the firmware update, we can
                // complete the job
                self.control
                    .update_job_status(
                        self.file_ctx,
                        &mut progress,
                        JobStatus::Succeeded,
                        JobStatusReason::Accepted,
                    )
                    .await?;
            }
            ImageState::Rejected(_) => {
                // The firmware update was rejected, complete the job as
                // FAILED (Job service will not allow us to set REJECTED
                // after the job has been started already).

                self.control
                    .update_job_status(
                        self.file_ctx,
                        &mut progress,
                        JobStatus::Failed,
                        JobStatusReason::Rejected,
                    )
                    .await?;
            }
            _ => {
                // The firmware update was aborted, complete the job as
                // FAILED (Job service will not allow us to set REJECTED
                // after the job has been started already).

                self.control
                    .update_job_status(
                        self.file_ctx,
                        &mut progress,
                        JobStatus::Failed,
                        JobStatusReason::Aborted,
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
            .update_job_status(self.file_ctx, &mut progress, status, reason)
            .await?;
        Ok(())
    }
}
