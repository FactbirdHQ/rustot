use embedded_hal::timer;
use smlang::statemachine;

use super::config::Config;
use super::control_interface::ControlInterface;
use super::data_interface::{DataInterface, Protocol};
use super::encoding::json::JobStatusReason;
use super::encoding::json::OtaJob;
use super::encoding::FileContext;
use super::pal::OtaPal;
use super::pal::OtaPalError;

use crate::ota::encoding::Bitmap;
use crate::rustot_log;
use crate::{
    jobs::{data_types::JobStatus, StatusDetails},
    ota::pal::Version,
};

use super::{
    callback::JobEvent,
    pal::{ImageState, PalImageState},
};

#[derive(Clone, Copy)]
pub enum ImageStateReason<E: Copy> {
    ImageStateMismatch,
    SignatureCheckPassed,
    InvalidDataProtocol,
    UserAbort,
    VersionCheck,
    Pal(OtaPalError<E>),
}

statemachine! {
    *Ready + Start [start_handler] = RequestingJob,
    RequestingJob + RequestJobDocument [request_job_handler] = WaitingForJob,
    RequestingJob + RequestTimer [request_job_handler] = WaitingForJob,
    WaitingForJob + ReceivedJobDocument((heapless::String<64>, OtaJob, Option<StatusDetails>)) [process_job_handler] = CreatingFile,
    CreatingFile + StartSelfTest [in_self_test_handler] = WaitingForJob,
    CreatingFile + CreateFile [init_file_handler] = RequestingFileBlock,
    CreatingFile + RequestTimer [init_file_handler] = RequestingFileBlock,
    RequestingFileBlock + RequestFileBlock [request_data_handler] = WaitingForFileBlock,
    RequestingFileBlock + RequestTimer [request_data_handler] = WaitingForFileBlock,
    WaitingForFileBlock + ReceivedFileBlock(&'a mut [u8]) [process_data_handler]  = WaitingForFileBlock,
    WaitingForFileBlock + RequestTimer [request_data_handler] = WaitingForFileBlock,
    WaitingForFileBlock + RequestFileBlock [request_data_handler] = WaitingForFileBlock,
    WaitingForFileBlock + RequestJobDocument [request_job_handler] = WaitingForJob,
    WaitingForFileBlock + ReceivedJobDocument((heapless::String<64>, OtaJob, Option<StatusDetails>)) [job_notification_handler] = RequestingJob,
    WaitingForFileBlock + CloseFile [close_file_handler] = WaitingForJob,
    Suspended + Resume = RequestingJob,
    Ready + Suspend = Suspended,
    RequestingJob + Suspend = Suspended,
    WaitingForJob + Suspend = Suspended,
    CreatingFile + Suspend = Suspended,
    RequestingFileBlock + Suspend = Suspended,
    WaitingForFileBlock + Suspend = Suspended,
    Ready + UserAbort [user_abort_handler] = WaitingForJob,
    RequestingJob + UserAbort [user_abort_handler] = WaitingForJob,
    WaitingForJob + UserAbort [user_abort_handler] = WaitingForJob,
    CreatingFile + UserAbort [user_abort_handler] = WaitingForJob,
    RequestingFileBlock + UserAbort [user_abort_handler] = WaitingForJob,
    WaitingForFileBlock + UserAbort [user_abort_handler] = WaitingForJob,
    Ready + Shutdown [shutdown_handler] = Ready,
    RequestingJob + Shutdown [shutdown_handler] = Ready,
    WaitingForJob + Shutdown [shutdown_handler] = Ready,
    CreatingFile + Shutdown [shutdown_handler] = Ready,
    RequestingFileBlock + Shutdown [shutdown_handler] = Ready,
    WaitingForFileBlock + Shutdown [shutdown_handler] = Ready,
}

pub(crate) enum Interface {
    Primary(FileContext),
    #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
    Secondary(FileContext),
}

impl Interface {
    pub fn file_ctx(&self) -> &FileContext {
        match self {
            Interface::Primary(i) => i,
            #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
            Interface::Secondary(i) => i,
        }
    }

    pub fn mut_file_ctx(&mut self) -> &mut FileContext {
        match self {
            Interface::Primary(i) => i,
            #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
            Interface::Secondary(i) => i,
        }
    }
}

macro_rules! data_interface {
    ($self:ident.$func:ident $(,$y:expr),*) => {
        match $self.active_interface {
            Some(Interface::Primary(ref mut ctx)) => $self.data_primary.$func(ctx, $($y),*),
            #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
            Some(Interface::Secondary(ref mut ctx)) => $self.data_secondary.as_mut().unwrap().$func(ctx, $($y),*),
            _ => Err(())
        }
    };
}

// Context of current OTA Job, keeping state
pub(crate) struct SmContext<'a, C, DP, DS, T, PAL, const L: usize>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    PAL: OtaPal,
{
    pub(crate) events: heapless::spsc::Queue<Events<'a>, L>,
    pub(crate) control: &'a C,
    pub(crate) data_primary: DP,
    #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
    pub(crate) data_secondary: Option<DS>,
    #[cfg(not(all(feature = "ota_mqtt_data", feature = "ota_http_data")))]
    pub(crate) data_secondary: core::marker::PhantomData<DS>,
    pub(crate) active_interface: Option<Interface>,
    pub(crate) pal: PAL,
    pub(crate) request_momentum: u8,
    pub(crate) request_timer: T,
    pub(crate) config: Config,
    // pub(crate) callback: F
}

impl<'a, C, DP, DS, T, PAL, const L: usize> SmContext<'a, C, DP, DS, T, PAL, L>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    PAL: OtaPal,
{
    /// Called to update the filecontext structure from the job
    fn get_file_context_from_job(
        &mut self,
        job_name: heapless::String<64>,
        ota_document: &OtaJob,
        status_details: Option<StatusDetails>,
    ) -> Result<FileContext, ()> {
        let file_idx = 0;

        if ota_document
            .files
            .get(file_idx)
            .map(|f| f.filesize)
            .unwrap_or_default()
            == 0
        {
            // Err(Error::ZeroFileSize)
            return Err(());
        }

        // If there's an active job, verify that it's the same as what's being
        // reported now
        let cur_file_ctx = self.active_interface.as_ref().map(|i| i.file_ctx().clone());
        let file_ctx = if let Some(mut file_ctx) = cur_file_ctx {
            if file_ctx.stream_name != ota_document.streamname {
                rustot_log!(info, "New job document received, aborting current job");

                // Abort the current job
                self.pal.set_platform_image_state(ImageState::Aborted).ok();

                // Abort any active file access and release the file resource,
                // if needed
                self.pal.abort(&file_ctx).ok();

                // Cleanup related to selected protocol
                data_interface!(self.cleanup, &self.config).ok();

                // Set new active job
                Ok(FileContext::new_from(
                    job_name,
                    ota_document,
                    status_details,
                    file_idx,
                    &self.config,
                    self.pal.get_active_firmware_version().map_err(drop)?,
                )?)
            } else {
                // The same job is being reported so update the url
                rustot_log!(info, "New job document ID is identical to the current job: Updating the URL based on the new job document");
                file_ctx.update_data_url = ota_document
                    .files
                    .get(0)
                    .map(|f| f.update_data_url.clone())
                    .ok_or(())?;

                Err(file_ctx)
            }
        } else {
            Ok(FileContext::new_from(
                job_name,
                ota_document,
                status_details,
                file_idx,
                &self.config,
                self.pal.get_active_firmware_version().map_err(drop)?,
            )?)
        };

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
        let file_ctx = match file_ctx {
            Ok(file_ctx) if file_ctx.self_test() => {
                self.handle_self_test_job(&file_ctx);
                file_ctx
            }
            Ok(file_ctx) => {
                rustot_log!(
                    info,
                    "Job document was accepted. Attempting to begin the update"
                );
                file_ctx
            }
            Err(file_ctx) => {
                rustot_log!(info, "Job document for receiving an update received");
                return Ok(file_ctx);
            }
        };

        if !self.platform_in_selftest() {
            // Create/Open the OTA file on the file system
            if let Err(e) = self.pal.create_file_for_rx(&file_ctx) {
                self.set_image_state_with_reason(
                    ImageState::Aborted,
                    Some(ImageStateReason::Pal(e)),
                )
                .map_err(drop)?;

                self.ota_close()?;
                return Err(());
            }
        }

        Ok(file_ctx)
    }

    fn select_interface(&self, file_ctx: FileContext, protocols: &[Protocol]) -> Option<Interface> {
        if protocols.contains(&DP::PROTOCOL) {
            Some(Interface::Primary(file_ctx))
        } else {
            #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
            return self
                .data_secondary
                .is_some()
                .then(|| Interface::Secondary(file_ctx))
                .filter(|_| protocols.contains(&DS::PROTOCOL));

            #[cfg(not(all(feature = "ota_mqtt_data", feature = "ota_http_data")))]
            None
        }
    }

    /// Check if the current image is `PendingCommit` and thus is in selftest
    fn platform_in_selftest(&self) -> bool {
        // Get the platform state from the OTA pal layer
        self.pal
            .get_platform_image_state()
            .map_or(false, |i| i == PalImageState::PendingCommit)
    }

    /// Validate update version when receiving job doc in self test state
    fn handle_self_test_job(&mut self, file_ctx: &FileContext) {
        rustot_log!(info, "In self test mode");

        let active_version = self
            .pal
            .get_active_firmware_version()
            .unwrap_or(Version::new(0, 0, 0));

        let version_check = if file_ctx.fileid == 0 && file_ctx.file_attributes == Some(0) {
            // Only check for versions if the target is self & always allow
            // updates if updated_by is not present.
            file_ctx.updated_by().map_or(true, |v| v > active_version)
        } else {
            true
        };

        if self.config.allow_downgrade || version_check {
            // The running firmware version is newer than the firmware that
            // performed the update or downgrade is allowed so this means we're
            // ready to start the self test phase.
            //
            // Set image state accordingly and update job status with self test
            // identifier.
            self.set_image_state_with_reason(
                ImageState::Testing,
                Some(ImageStateReason::VersionCheck),
            )
            .ok();
        } else {
            self.set_image_state_with_reason(
                ImageState::Rejected,
                Some(ImageStateReason::VersionCheck),
            )
            .ok();

            // TODO: Application callback for self-test failure.
            // self.OtaAppCallback( OtaJobEventSelfTestFailed, NULL );

            // Handle self-test failure in the platform specific implementation,
            // example, reset the device in case of firmware upgrade.
            self.pal.reset_device().ok();
        }
    }

    fn set_image_state_with_reason(
        &mut self,
        mut image_state: ImageState,
        mut reason: Option<ImageStateReason<PAL::Error>>,
    ) -> Result<(), ()> {
        // Call the platform specific code to set the image state
        if let Err(e) = self.pal.set_platform_image_state(image_state) {
            if image_state != ImageState::Aborted {
                // If the platform image state couldn't be set correctly, force
                // fail the update by setting the image state to "Rejected"
                // unless it's already in "Aborted".
                image_state = ImageState::Rejected;

                // Capture the failure reason if not already set (and we're not
                // already Aborted as checked above). Otherwise Keep the
                // original reject reason code since it is possible for the PAL
                // to fail to update the image state in some cases (e.g. a reset
                // already caused the bundle rollback and we failed to rollback
                // again).
                //
                // Intentionally override reason since we failed within this
                // function
                reason.get_or_insert(ImageStateReason::Pal(e));
            }
        }

        // Now update the image state and job status on service side
        if let Some(ref mut interface) = self.active_interface {
            // updateJobStatusFromImageState()
            match image_state {
                ImageState::Testing => {
                    // We discovered we're ready for test mode, put job status
                    // in self_test active
                    self.control
                        .update_job_status(
                            interface.mut_file_ctx(),
                            &self.config,
                            JobStatus::InProgress,
                            JobStatusReason::SelfTestActive,
                        )
                        .ok();
                }
                ImageState::Accepted => {
                    // Now that we have accepted the firmware update, we can
                    // complete the job
                    self.control
                        .update_job_status(
                            interface.mut_file_ctx(),
                            &self.config,
                            JobStatus::Succeeded,
                            JobStatusReason::Accepted,
                        )
                        .ok();
                }
                ImageState::Rejected => {
                    // The firmware update was rejected, complete the job as
                    // FAILED (Job service will not allow us to set REJECTED
                    // after the job has been started already).
                    self.control
                        .update_job_status(
                            interface.mut_file_ctx(),
                            &self.config,
                            JobStatus::Failed,
                            JobStatusReason::Rejected,
                        )
                        .ok();
                }
                _ => {
                    // The firmware update was aborted, complete the job as
                    // FAILED (Job service will not allow us to set REJECTED
                    // after the job has been started already).
                    self.control
                        .update_job_status(
                            interface.mut_file_ctx(),
                            &self.config,
                            JobStatus::Failed,
                            JobStatusReason::Aborted,
                        )
                        .ok();
                }
            }
            Ok(())
        } else {
            // Err(NoActiveJob)
            Err(())
        }
    }

    pub fn ota_close(&mut self) -> Result<(), ()> {
        // Cleanup related to selected protocol.
        data_interface!(self.cleanup, &self.config)?;

        // Abort any active file access and release the file resource, if needed
        let file_ctx = self.active_interface.as_ref().unwrap().file_ctx();
        self.pal.abort(file_ctx).map_err(drop)?;

        self.active_interface = None;
        Ok(())
    }

    fn ingest_data_block(&mut self, payload: &mut [u8]) -> Result<bool, ()> {
        let block = data_interface!(self.decode_file_block, payload)?;

        let file_ctx = self.active_interface.as_mut().unwrap().mut_file_ctx();

        if block.validate(self.config.block_size, file_ctx.filesize) {
            if !file_ctx
                .bitmap
                .get(block.block_id - file_ctx.block_offset as usize)
            {
                rustot_log!(
                    info,
                    "Block {:?} is a DUPLICATE. {:?} blocks remaining.",
                    block.block_id,
                    file_ctx.blocks_remaining
                );

                // Just return same progress as before
                return Ok(false);
            }

            self.pal
                .write_block(
                    &file_ctx,
                    block.block_id * self.config.block_size,
                    block.block_payload,
                )
                .ok();

            file_ctx
                .bitmap
                .set(block.block_id - file_ctx.block_offset as usize, false);

            file_ctx.blocks_remaining -= 1;

            if file_ctx.blocks_remaining == 0 {
                rustot_log!(info, "Received final expected block of file.");

                // Stop the request timer
                self.request_timer.try_cancel().ok();

                self.pal.close_file(&file_ctx).map_err(drop)?;

                // Return true to indicate end of file.
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            rustot_log!(
                error,
                "Error! Block {:?} out of expected range! Size {:?}",
                block.block_id,
                block.block_size
            );

            // Err(Error::BlockOutOfRange)
            Err(())
        }
    }
}

impl<'a, C, DP, DS, T, PAL, const L: usize> StateMachineContext
    for SmContext<'a, C, DP, DS, T, PAL, L>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    PAL: OtaPal,
{
    /// Start timers and initiate request for job document
    fn start_handler(&mut self) -> bool {
        // Start self-test timer, if platform is in self-test.
        if self.platform_in_selftest() {
            // TODO: Start self-test timer
        }

        // Send event to OTA task to get job document
        self.events.enqueue(Events::RequestJobDocument).is_ok()
        // .map_err(|_| Error::SignalEventFailed)?;
    }

    /// Initiate a request for a job
    fn request_job_handler(&mut self) -> bool {
        match self.control.request_job() {
            Err(_) => {
                if self.request_momentum < self.config.max_request_momentum {
                    // Start request timer
                    self.request_timer
                        .try_start(self.config.request_wait_ms)
                        .ok();

                    self.request_momentum += 1;
                    false
                } else {
                    // Stop request timer
                    self.request_timer.try_cancel().ok();

                    // Send shutdown event to the OTA Agent task
                    self.events.enqueue(Events::Shutdown).ok();
                    // .map_err(|_| Error::SignalEventFailed)?;

                    // Too many requests have been sent without a response or
                    // too many failures when trying to publish the request
                    // message. Abort.

                    // Err(Error::MomentumAbort)
                    false
                }
            }
            Ok(_) => {
                // Stop request timer
                self.request_timer.try_cancel().ok();

                // Reset the request momentum
                self.request_momentum = 0;
                true
            }
        }
    }

    /// Initialize and handle file transfer
    fn init_file_handler(&mut self) -> bool {
        match data_interface!(self.init_file_transfer) {
            Err(_) => {
                if self.request_momentum < self.config.max_request_momentum {
                    // Start request timer
                    self.request_timer
                        .try_start(self.config.request_wait_ms)
                        .ok();

                    self.request_momentum += 1;
                } else {
                    // Stop request timer
                    self.request_timer.try_cancel().ok();

                    // Send shutdown event to the OTA Agent task
                    self.events.enqueue(Events::Shutdown).ok();
                    // .map_err(|_| Error::SignalEventFailed)?;

                    // Too many requests have been sent without a response or
                    // too many failures when trying to publish the request
                    // message. Abort.

                    // Err(Error::MomentumAbort)
                }
                false
            }
            Ok(_) => {
                // Reset the request momentum
                self.request_momentum = 0;

                // TODO: Reset the OTA statistics

                rustot_log!(info, "Initialized file handler! Requesting file blocks");

                self.events.enqueue(Events::RequestFileBlock).ok();
                // .map_err(|_| Error::SignalEventFailed)?;

                true
            }
        }
    }

    /// Handle self test
    fn in_self_test_handler(&mut self) -> bool {
        rustot_log!(info, "Beginning self-test");
        // Check the platform's OTA update image state. It should also be in
        // self test
        if self.platform_in_selftest() {
            // TODO: Callback for application specific self-test.
            // otaAgent.OtaAppCallback( OtaJobEventStartTest, NULL );
            rustot_log!(info, "Application callback! OtaJobEventStartTest");

            // Clear self-test flag
            let file_ctx = self.active_interface.as_mut().unwrap().mut_file_ctx();
            file_ctx
                .status_details
                .insert(heapless::String::from("self_test"), heapless::String::new())
                .ok();

            // TODO: Stop the self test timer as it is no longer required
        } else {
            // The job is in self test but the platform image state is not so it
            // could be an attack on the platform image state. Reject the update
            // (this should also cause the image to be erased), aborting the job
            // and reset the device.
            rustot_log!(error,"Rejecting new image and rebooting: the job is in the self-test state while the platform is not.");
            self.set_image_state_with_reason(
                ImageState::Rejected,
                Some(ImageStateReason::ImageStateMismatch),
            )
            .ok();
            self.pal.reset_device().ok();
        }
        true
    }

    /// Update file context from job document
    fn process_job_handler(
        &mut self,
        data: &(heapless::String<64>, OtaJob, Option<StatusDetails>),
    ) -> bool {
        let (job_name, ota_document, status_details) = data;

        let file_ctx = self
            .get_file_context_from_job(job_name.clone(), ota_document, status_details.clone())
            .unwrap();

        // A null context here could either mean we didn't receive a valid job
        // or it could signify that we're in the self test phase (where the OTA
        // file transfer is already completed and we have reset the device and
        // are now running the new firmware). We will check the state to
        // determine which case we're in.
        if !self.platform_in_selftest() {
            if let Some(interface) = self.select_interface(file_ctx, &ota_document.protocols) {
                rustot_log!(info, "Setting OTA data interface");
                self.active_interface = Some(interface);

                // Received a valid context so send event to request file blocks
                self.events.enqueue(Events::CreateFile).ok();
                // .map_err(|_| Error::SignalEventFailed)?;

                true
            } else {
                // Failed to set the data interface so abort the OTA. If there
                // is a valid job id, then a job status update will be sent.
                rustot_log!(
                    error,
                    "Failed to set OTA data interface. Aborting current update."
                );

                self.set_image_state_with_reason(
                    ImageState::Aborted,
                    Some(ImageStateReason::InvalidDataProtocol),
                )
                .ok();
                false
            }
        } else {
            // Received a job that is not in self-test but platform is, so
            // reboot the device to allow roll back to previous image.
            rustot_log!(error, "Rejecting new image and rebooting: The platform is in the self-test state while the job is not.");
            self.pal.reset_device().ok();
            false
        }
    }

    /// Request for data blocks
    fn request_data_handler(&mut self) -> bool {
        let file_ctx = self.active_interface.as_ref().unwrap().file_ctx();
        if file_ctx.blocks_remaining > 0 {
            rustot_log!(
                debug,
                "Requesting data! Remaining: {:?}. momentum: {}",
                file_ctx.blocks_remaining,
                self.request_momentum
            );
            // Start the request timer
            self.request_timer
                .try_start(self.config.request_wait_ms)
                .ok();

            if self.request_momentum <= self.config.max_request_momentum {
                // Each request increases the momentum until a response is
                // received. Too much momentum is interpreted as a failure to
                // communicate and will cause us to abort the OTA.
                self.request_momentum += 1;

                // Request data blocks
                return data_interface!(self.request_file_block, &self.config).is_ok();
            } else {
                // Stop the request timer
                self.request_timer.try_cancel().ok();

                // Failed to send data request abort and close file.
                self.set_image_state_with_reason(ImageState::Aborted, None)
                    .ok();

                self.events.enqueue(Events::Shutdown).ok();
                // .map_err(|_| Error::SignalEventFailed)?;

                // Reset the request momentum
                self.request_momentum = 0;

                // Too many requests have been sent without a response or too
                // many failures when trying to publish the request message.
                // Abort. return Err(Error::MomentumAbort)
            }
        }
        false
    }

    /// Upon receiving a new job document cancel current job if present and
    /// initiate new download
    fn job_notification_handler(
        &mut self,
        _data: &(heapless::String<64>, OtaJob, Option<StatusDetails>),
    ) -> bool {
        // Stop the request timer
        self.request_timer.try_cancel().ok();

        // Abort the current job
        self.pal.set_platform_image_state(ImageState::Aborted).ok();
        self.ota_close().ok();

        true
    }

    /// Process incoming data blocks
    fn process_data_handler(&mut self, payload: &mut [u8]) -> bool {
        // Decode the file block received
        match self.ingest_data_block(payload) {
            Ok(true) => {
                let file_ctx = self.active_interface.as_mut().unwrap().mut_file_ctx();

                // File is completed! Update progress accordingly.
                let (status, reason, event) = if let Some(0) = file_ctx.file_attributes {
                    (
                        JobStatus::InProgress,
                        JobStatusReason::SigCheckPassed,
                        JobEvent::Activate,
                    )
                } else {
                    (
                        JobStatus::Succeeded,
                        JobStatusReason::Accepted,
                        JobEvent::UpdateComplete,
                    )
                };

                self.control
                    .update_job_status(file_ctx, &self.config, status, reason)
                    .ok();

                // Stop the request timer.
                self.request_timer.try_cancel().ok();

                // Send event to close file.
                self.events.enqueue(Events::CloseFile).ok();
                // .map_err(|_| Error::SignalEventFailed)?;

                // TODO: Last file block processed, increment the statistics
                // otaAgent.statistics.otaPacketsProcessed++;

                // TODO: Let main application know that update is complete
                // otaAgent.OtaAppCallback( otaJobEvent, &jobDoc );
                rustot_log!(info, "Application callback! {:?}", event);
            }
            Ok(false) => {
                let file_ctx = self.active_interface.as_mut().unwrap().mut_file_ctx();

                // File block processed, increment the statistics.
                // otaAgent.statistics.otaPacketsProcessed++;

                // Reset the momentum counter since we received a good block
                self.request_momentum = 0;

                // We're actively receiving a file so update the job status as
                // needed
                self.control
                    .update_job_status(
                        file_ctx,
                        &self.config,
                        JobStatus::InProgress,
                        JobStatusReason::Receiving,
                    )
                    .ok();

                if file_ctx.request_block_remaining > 1 {
                    file_ctx.request_block_remaining -= 1;
                } else {
                    file_ctx.block_offset += self.config.max_blocks_per_request;
                    file_ctx.bitmap = Bitmap::new(
                        file_ctx.filesize,
                        self.config.block_size,
                        file_ctx.block_offset,
                    );
                    file_ctx.request_block_remaining = core::cmp::min(
                        self.config.max_blocks_per_request,
                        file_ctx.blocks_remaining as u32,
                    );

                    // Start the request timer.
                    self.request_timer
                        .try_start(self.config.request_wait_ms)
                        .ok();

                    self.events.enqueue(Events::RequestFileBlock).ok();
                    // .map_err(|_| Error::SignalEventFailed)?;
                }
            }
            Err(_) => {
                let file_ctx = self.active_interface.as_mut().unwrap().mut_file_ctx();

                rustot_log!(error,
                    "Failed to ingest data block, rejecting image: ingest_data_block returned error"
                );

                // Call the platform specific code to reject the image
                self.pal.set_platform_image_state(ImageState::Rejected).ok();

                // TODO: Pal reason
                self.control
                    .update_job_status(
                        file_ctx,
                        &self.config,
                        JobStatus::Failed,
                        JobStatusReason::Pal(0),
                    )
                    .ok();

                // Stop the request timer.
                self.request_timer.try_cancel().ok();

                // Send event to close file.
                self.events.enqueue(Events::CloseFile).ok();
                // .map_err(|_| Error::SignalEventFailed)?;

                // TODO: otaAgent.OtaAppCallback( OtaJobEventFail, &jobDoc );
                rustot_log!(info, "Application callback! OtaJobEventFail");
                return false;
            }
        }

        // TODO: Application callback for event processed.
        rustot_log!(info, "Application callback! OtaJobEventProcessed");
        // otaAgent.OtaAppCallback( OtaJobEventProcessed, ( const void * ) pEventData );
        true
    }

    /// Close file opened for download
    fn close_file_handler(&mut self) -> bool {
        self.ota_close().is_ok()
    }

    /// Handle user interrupt to abort task
    fn user_abort_handler(&mut self) -> bool {
        if self.active_interface.is_some() {
            self.set_image_state_with_reason(
                ImageState::Aborted,
                Some(ImageStateReason::UserAbort),
            )
            .ok();
            self.ota_close().is_ok()
        } else {
            // Err(Error::NoActiveJob)
            false
        }
    }

    /// Handle user interrupt to abort task
    fn shutdown_handler(&mut self) -> bool {
        if self.active_interface.is_some() {
            self.set_image_state_with_reason(
                ImageState::Aborted,
                Some(ImageStateReason::UserAbort),
            )
            .ok();
            self.ota_close().ok();
        }
        true
    }
}
