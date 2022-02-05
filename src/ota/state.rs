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

use crate::jobs::{data_types::JobStatus, StatusDetails};
use crate::ota::encoding::Bitmap;
use crate::ota::pal::OtaEvent;
use crate::rustot_log;

use super::{
    error::OtaError,
    pal::{ImageState, PalImageState},
};

#[derive(Clone, Copy)]
#[cfg_attr(feature = "defmt-impl", derive(defmt::Format))]
pub enum ImageStateReason<E: Copy> {
    NewerJob,
    FailedIngest,
    MomentumAbort,
    ImageStateMismatch,
    SignatureCheckPassed,
    InvalidDataProtocol,
    UserAbort,
    VersionCheck,
    Pal(OtaPalError<E>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RestartReason {
    Activate(u8),
    Restart(u8),
}

impl RestartReason {
    #[must_use]
    pub fn inc(self) -> Self {
        match self {
            Self::Activate(cnt) => Self::Activate(cnt + 1),
            Self::Restart(cnt) => Self::Restart(cnt + 1),
        }
    }
}

#[derive(PartialEq)]
pub struct JobEventData<'a> {
    pub job_name: &'a str,
    pub ota_document: &'a OtaJob<'a>,
    pub status_details: Option<&'a StatusDetails>,
}

statemachine! {
    guard_error: OtaError,
    transitions: {
        *Ready + Start [start_handler] = RequestingJob,
        RequestingJob | WaitingForFileBlock + RequestJobDocument [request_job_handler] = WaitingForJob,
        RequestingJob + RequestTimer [request_job_handler] = WaitingForJob,
        WaitingForJob + RequestJobDocument [request_job_handler] = WaitingForJob,
        WaitingForJob + ReceivedJobDocument(JobEventData<'a>) [process_job_handler] = CreatingFile,
        WaitingForJob + Start [request_job_handler] = WaitingForJob,
        CreatingFile + StartSelfTest [in_self_test_handler] = WaitingForJob,
        CreatingFile + CreateFile [init_file_handler] = RequestingFileBlock,
        CreatingFile + RequestTimer [init_file_handler] = RequestingFileBlock,
        CreatingFile | WaitingForJob | Restarting + Restart(RestartReason) [restart_handler] = Restarting,
        RequestingFileBlock | WaitingForFileBlock + RequestFileBlock [request_data_handler] = WaitingForFileBlock,
        RequestingFileBlock | WaitingForFileBlock + RequestTimer [request_data_handler] = WaitingForFileBlock,
        WaitingForFileBlock + ReceivedFileBlock(&'a mut [u8]) [process_data_handler]  = WaitingForFileBlock,
        WaitingForFileBlock + ReceivedJobDocument(JobEventData<'a>) [job_notification_handler] = RequestingJob,
        WaitingForFileBlock + CloseFile [close_file_handler] = WaitingForJob,
        Suspended + Resume [resume_job_handler] = RequestingJob,
        Ready | RequestingJob | WaitingForJob | CreatingFile | RequestingFileBlock | WaitingForFileBlock + Suspend = Suspended,
        Ready | RequestingJob | WaitingForJob | CreatingFile | RequestingFileBlock | WaitingForFileBlock + UserAbort [user_abort_handler] = WaitingForJob,
        Ready | RequestingJob | WaitingForJob | CreatingFile | RequestingFileBlock | WaitingForFileBlock + Shutdown [shutdown_handler] = Ready,
    }
}

#[cfg(feature = "defmt-impl")]
impl defmt::Format for Error {
    fn format(&self, fmt: defmt::Formatter) {
        match self {
            Error::InvalidEvent => defmt::write!(fmt, "Error::InvalidEvent"),
            Error::GuardFailed(e) => defmt::write!(fmt, "Error::GuardFailed({:?})", e),
        }
    }
}

pub(crate) enum Interface {
    Primary(FileContext),
    #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
    Secondary(FileContext),
}

impl Interface {
    pub const fn file_ctx(&self) -> &FileContext {
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
            Some(Interface::Secondary(ref mut ctx)) => $self.data_secondary.as_mut().ok_or(OtaError::InvalidInterface)?.$func(ctx, $($y),*),
            _ => Err(OtaError::InvalidInterface)
        }
    };
}

// Context of current OTA Job, keeping state
pub(crate) struct SmContext<'a, C, DP, DS, T, ST, PAL, const L: usize>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::nb::CountDown + timer::nb::Cancel,
    T::Time: From<u32>,
    ST: timer::nb::CountDown + timer::nb::Cancel,
    ST::Time: From<u32>,
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
    pub(crate) self_test_timer: Option<ST>,
    pub(crate) config: Config,
    pub(crate) image_state: ImageState<PAL::Error>,
}

impl<'a, C, DP, DS, T, ST, PAL, const L: usize> SmContext<'a, C, DP, DS, T, ST, PAL, L>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::nb::CountDown + timer::nb::Cancel,
    T::Time: From<u32>,
    ST: timer::nb::CountDown + timer::nb::Cancel,
    ST::Time: From<u32>,
    PAL: OtaPal,
{
    /// Called to update the filecontext structure from the job
    fn get_file_context_from_job(
        &mut self,
        job_name: &str,
        ota_document: &OtaJob,
        status_details: Option<StatusDetails>,
    ) -> Result<FileContext, OtaError> {
        let file_idx = 0;

        if ota_document
            .files
            .get(file_idx)
            .map(|f| f.filesize)
            .unwrap_or_default()
            == 0
        {
            return Err(OtaError::ZeroFileSize);
        }

        // If there's an active job, verify that it's the same as what's being
        // reported now
        let cur_file_ctx = self.active_interface.as_mut().map(|i| i.mut_file_ctx());
        let file_ctx = if let Some(mut file_ctx) = cur_file_ctx {
            if file_ctx.stream_name != ota_document.streamname {
                rustot_log!(info, "New job document received, aborting current job");

                // Abort the current job
                // TODO:??
                self.pal
                    .set_platform_image_state(ImageState::Aborted(ImageStateReason::NewerJob))?;

                // Abort any active file access and release the file resource,
                // if needed
                self.pal.abort(file_ctx)?;

                // Cleanup related to selected protocol
                data_interface!(self.cleanup, &self.config)?;

                // Set new active job
                Ok(FileContext::new_from(
                    job_name,
                    ota_document,
                    status_details,
                    file_idx,
                    &self.config,
                    self.pal.get_active_firmware_version()?,
                )?)
            } else {
                // The same job is being reported so update the url
                rustot_log!(info, "New job document ID is identical to the current job: Updating the URL based on the new job document");
                file_ctx.update_data_url = ota_document
                    .files
                    .get(0)
                    .map(|f| f.update_data_url.map(heapless::String::from))
                    .ok_or(OtaError::InvalidFile)?;

                Err(file_ctx.clone())
            }
        } else {
            Ok(FileContext::new_from(
                job_name,
                ota_document,
                status_details,
                file_idx,
                &self.config,
                self.pal.get_active_firmware_version()?,
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
        let mut file_ctx = match file_ctx {
            Ok(mut file_ctx) if file_ctx.self_test() => {
                self.handle_self_test_job(&mut file_ctx)?;
                return Ok(file_ctx);
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
                // Don't create file again on update.
                return Ok(file_ctx);
            }
        };

        // Create/Open the OTA file on the file system
        if let Err(e) = self.pal.create_file_for_rx(&file_ctx) {
            self.image_state = Self::set_image_state_with_reason(
                self.control,
                &mut self.pal,
                &self.config,
                &mut file_ctx,
                ImageState::Aborted(ImageStateReason::Pal(e)),
            )?;

            self.ota_close()?;
            return Err(e.into());
        }

        Ok(file_ctx)
    }

    fn select_interface(
        &self,
        file_ctx: FileContext,
        protocols: &[Protocol],
    ) -> Result<Interface, FileContext> {
        if protocols.contains(&DP::PROTOCOL) {
            Ok(Interface::Primary(file_ctx))
        } else {
            #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
            if protocols.contains(&DS::PROTOCOL) && self.data_secondary.is_some() {
                Ok(Interface::Secondary(file_ctx))
            } else {
                Err(file_ctx)
            }

            #[cfg(not(all(feature = "ota_mqtt_data", feature = "ota_http_data")))]
            Err(file_ctx)
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
    fn handle_self_test_job(&mut self, file_ctx: &mut FileContext) -> Result<(), OtaError> {
        rustot_log!(info, "In self test mode");

        let active_version = self.pal.get_active_firmware_version().unwrap_or_default();

        let version_check = if file_ctx.fileid == 0 && file_ctx.file_type == Some(0) {
            // Only check for versions if the target is self & always allow
            // updates if updated_by is not present.
            file_ctx
                .updated_by()
                .map_or(true, |updated_by| active_version > updated_by)
        } else {
            true
        };

        rustot_log!(info, "Version check: {:?}", version_check);

        if self.config.allow_downgrade || version_check {
            // The running firmware version is newer than the firmware that
            // performed the update or downgrade is allowed so this means we're
            // ready to start the self test phase.
            //
            // Set image state accordingly and update job status with self test
            // identifier.
            self.image_state = Self::set_image_state_with_reason(
                self.control,
                &mut self.pal,
                &self.config,
                file_ctx,
                ImageState::Testing(ImageStateReason::VersionCheck),
            )?;

            Ok(())
        } else {
            self.image_state = Self::set_image_state_with_reason(
                self.control,
                &mut self.pal,
                &self.config,
                file_ctx,
                ImageState::Rejected(ImageStateReason::VersionCheck),
            )?;

            self.pal.complete_callback(OtaEvent::SelfTestFailed)?;

            // Handle self-test failure in the platform specific implementation,
            // example, reset the device in case of firmware upgrade.
            self.events
                .enqueue(Events::Restart(RestartReason::Restart(0)))
                .map_err(|_| OtaError::SignalEventFailed)?;
            Ok(())
        }
    }

    fn set_image_state_with_reason(
        control: &C,
        pal: &mut PAL,
        config: &Config,
        file_ctx: &mut FileContext,
        image_state: ImageState<PAL::Error>,
    ) -> Result<ImageState<PAL::Error>, OtaError> {
        // rustot_log!(debug, "set_image_state_with_reason {:?}", image_state);
        // Call the platform specific code to set the image state
        let image_state = match pal.set_platform_image_state(image_state) {
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
                //
                // Intentionally override reason since we failed within this
                // function
                ImageState::Rejected(ImageStateReason::Pal(e))
            }
            _ => image_state,
        };

        // Now update the image state and job status on server side
        match image_state {
            ImageState::Testing(_) => {
                // We discovered we're ready for test mode, put job status
                // in self_test active
                control.update_job_status(
                    file_ctx,
                    config,
                    JobStatus::InProgress,
                    JobStatusReason::SelfTestActive,
                )?;
            }
            ImageState::Accepted => {
                // Now that we have accepted the firmware update, we can
                // complete the job
                control.update_job_status(
                    file_ctx,
                    config,
                    JobStatus::Succeeded,
                    JobStatusReason::Accepted,
                )?;
            }
            ImageState::Rejected(_) => {
                // The firmware update was rejected, complete the job as
                // FAILED (Job service will not allow us to set REJECTED
                // after the job has been started already).
                control.update_job_status(
                    file_ctx,
                    config,
                    JobStatus::Failed,
                    JobStatusReason::Rejected,
                )?;
            }
            _ => {
                // The firmware update was aborted, complete the job as
                // FAILED (Job service will not allow us to set REJECTED
                // after the job has been started already).
                control.update_job_status(
                    file_ctx,
                    config,
                    JobStatus::Failed,
                    JobStatusReason::Aborted,
                )?;
            }
        }
        Ok(image_state)
    }

    pub fn ota_close(&mut self) -> Result<(), OtaError> {
        // Cleanup related to selected protocol.
        data_interface!(self.cleanup, &self.config)?;

        // Abort any active file access and release the file resource, if needed
        let file_ctx = self
            .active_interface
            .as_ref()
            .ok_or(OtaError::InvalidInterface)?
            .file_ctx();

        self.pal.abort(file_ctx)?;

        self.active_interface = None;
        Ok(())
    }

    fn ingest_data_block(&mut self, payload: &mut [u8]) -> Result<bool, OtaError> {
        let block = data_interface!(self.decode_file_block, payload)?;

        let file_ctx = self
            .active_interface
            .as_mut()
            .ok_or(OtaError::InvalidInterface)?
            .mut_file_ctx();

        if block.validate(self.config.block_size, file_ctx.filesize) {
            if block.block_id < file_ctx.block_offset as usize
                || !file_ctx
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

            self.pal.write_block(
                file_ctx,
                block.block_id * self.config.block_size,
                block.block_payload,
            )?;

            file_ctx
                .bitmap
                .set(block.block_id - file_ctx.block_offset as usize, false);

            file_ctx.blocks_remaining -= 1;

            if file_ctx.blocks_remaining == 0 {
                rustot_log!(info, "Received final expected block of file.");

                // Stop the request timer
                self.request_timer.cancel().map_err(|_| OtaError::Timer)?;

                self.pal.close_file(file_ctx)?;

                // Return true to indicate end of file.
                Ok(true)
            } else {
                if file_ctx.bitmap.is_empty() {
                    file_ctx.block_offset += 31;
                    file_ctx.bitmap = Bitmap::new(
                        file_ctx.filesize,
                        self.config.block_size,
                        file_ctx.block_offset,
                    );
                }

                Ok(false)
            }
        } else {
            rustot_log!(
                error,
                "Error! Block {:?} out of expected range! Size {:?}",
                block.block_id,
                block.block_size
            );

            Err(OtaError::BlockOutOfRange)
        }
    }
}

impl<'a, C, DP, DS, T, ST, PAL, const L: usize> StateMachineContext
    for SmContext<'a, C, DP, DS, T, ST, PAL, L>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::nb::CountDown + timer::nb::Cancel,
    T::Time: From<u32>,
    ST: timer::nb::CountDown + timer::nb::Cancel,
    ST::Time: From<u32>,
    PAL: OtaPal,
{
    fn restart_handler(&mut self, reason: &RestartReason) -> Result<(), OtaError> {
        rustot_log!(debug, "restart_handler");
        match reason {
            RestartReason::Activate(cnt) if *cnt > self.config.activate_delay => {
                rustot_log!(info, "Application callback! OtaEvent::Activate");
                self.pal.complete_callback(OtaEvent::Activate)?;
            }
            RestartReason::Restart(cnt) if *cnt > self.config.activate_delay => {
                self.pal.reset_device()?;
            }
            r => {
                self.events
                    .enqueue(Events::Restart(r.inc()))
                    .map_err(|_| OtaError::SignalEventFailed)?;
            }
        }
        Ok(())
    }

    /// Start timers and initiate request for job document
    fn start_handler(&mut self) -> Result<(), OtaError> {
        rustot_log!(debug, "start_handler");
        // Start self-test timer, if platform is in self-test.
        if self.platform_in_selftest() {
            // Start self-test timer
            if let Some(ref mut self_test_timer) = self.self_test_timer {
                self_test_timer
                    .start(self.config.self_test_timeout_ms)
                    .map_err(|_| OtaError::Timer)?;
            }
        }

        // Initialize the control interface
        self.control.init()?;

        // Send event to OTA task to get job document
        self.events
            .enqueue(Events::RequestJobDocument)
            .map_err(|_| OtaError::SignalEventFailed)
    }

    fn resume_job_handler(&mut self) -> Result<(), OtaError> {
        rustot_log!(debug, "resume_job_handler");
        // Send signal to request job document
        self.events
            .enqueue(Events::RequestJobDocument)
            .map_err(|_| OtaError::SignalEventFailed)
    }

    /// Initiate a request for a job
    fn request_job_handler(&mut self) -> Result<(), OtaError> {
        rustot_log!(debug, "request_job_handler");
        match self.control.request_job() {
            Err(e) => {
                if self.request_momentum < self.config.max_request_momentum {
                    // Start request timer
                    self.request_timer
                        .start(self.config.request_wait_ms)
                        .map_err(|_| OtaError::Timer)?;

                    self.request_momentum += 1;
                    Err(e)
                } else {
                    // Stop request timer
                    self.request_timer.cancel().map_err(|_| OtaError::Timer)?;

                    // Send shutdown event to the OTA Agent task
                    self.events
                        .enqueue(Events::Shutdown)
                        .map_err(|_| OtaError::SignalEventFailed)?;

                    // Too many requests have been sent without a response or
                    // too many failures when trying to publish the request
                    // message. Abort.
                    Err(OtaError::MomentumAbort)
                }
            }
            Ok(_) => {
                // Stop request timer
                self.request_timer.cancel().map_err(|_| OtaError::Timer)?;

                // Reset the request momentum
                self.request_momentum = 0;
                Ok(())
            }
        }
    }

    /// Initialize and handle file transfer
    fn init_file_handler(&mut self) -> Result<(), OtaError> {
        rustot_log!(debug, "init_file_handler");
        match data_interface!(self.init_file_transfer) {
            Err(e) => {
                if self.request_momentum < self.config.max_request_momentum {
                    // Start request timer
                    self.request_timer
                        .start(self.config.request_wait_ms)
                        .map_err(|_| OtaError::Timer)?;

                    self.request_momentum += 1;
                    Err(e)
                } else {
                    // Stop request timer
                    self.request_timer.cancel().map_err(|_| OtaError::Timer)?;

                    // Send shutdown event to the OTA Agent task
                    self.events
                        .enqueue(Events::Shutdown)
                        .map_err(|_| OtaError::SignalEventFailed)?;

                    // Too many requests have been sent without a response or
                    // too many failures when trying to publish the request
                    // message. Abort.

                    Err(OtaError::MomentumAbort)
                }
            }
            Ok(_) => {
                // Reset the request momentum
                self.request_momentum = 0;

                // TODO: Reset the OTA statistics

                rustot_log!(info, "Initialized file handler! Requesting file blocks");

                self.events
                    .enqueue(Events::RequestFileBlock)
                    .map_err(|_| OtaError::SignalEventFailed)?;

                Ok(())
            }
        }
    }

    /// Handle self test
    fn in_self_test_handler(&mut self) -> Result<(), OtaError> {
        rustot_log!(info, "Beginning self-test");
        // Check the platform's OTA update image state. It should also be in
        // self test
        let in_self_test = self.platform_in_selftest();
        // Clear self-test flag
        let file_ctx = self
            .active_interface
            .as_mut()
            .ok_or(OtaError::InvalidInterface)?
            .mut_file_ctx();

        if in_self_test {
            self.pal.complete_callback(OtaEvent::StartTest)?;
            rustot_log!(info, "Application callback! OtaEvent::StartTest");

            self.image_state = ImageState::Accepted;
            self.control.update_job_status(
                file_ctx,
                &self.config,
                JobStatus::Succeeded,
                JobStatusReason::Accepted,
            )?;

            file_ctx
                .status_details
                .insert(
                    heapless::String::from("self_test"),
                    heapless::String::from(JobStatusReason::Accepted.as_str()),
                )
                .map_err(|_| OtaError::Overflow)?;

            // Stop the self test timer as it is no longer required
            if let Some(ref mut self_test_timer) = self.self_test_timer {
                self_test_timer.cancel().map_err(|_| OtaError::Timer)?;
            }
        } else {
            // The job is in self test but the platform image state is not so it
            // could be an attack on the platform image state. Reject the update
            // (this should also cause the image to be erased), aborting the job
            // and reset the device.
            rustot_log!(error,"Rejecting new image and rebooting: the job is in the self-test state while the platform is not.");
            self.image_state = Self::set_image_state_with_reason(
                self.control,
                &mut self.pal,
                &self.config,
                file_ctx,
                ImageState::Rejected(ImageStateReason::ImageStateMismatch),
            )?;

            self.events
                .enqueue(Events::Restart(RestartReason::Restart(0)))
                .map_err(|_| OtaError::SignalEventFailed)?;
        }
        Ok(())
    }

    /// Update file context from job document
    fn process_job_handler(&mut self, data: &JobEventData<'_>) -> Result<(), OtaError> {
        let JobEventData {
            job_name,
            ota_document,
            status_details,
        } = data;

        let file_ctx = self.get_file_context_from_job(
            job_name,
            ota_document,
            status_details.map(Clone::clone),
        )?;

        match self.select_interface(file_ctx, &ota_document.protocols) {
            Ok(interface) => {
                rustot_log!(info, "Setting OTA data interface");
                self.active_interface = Some(interface);
            }
            Err(mut file_ctx) => {
                // Failed to set the data interface so abort the OTA. If there
                // is a valid job id, then a job status update will be sent.
                rustot_log!(
                    error,
                    "Failed to set OTA data interface. Aborting current update."
                );

                self.image_state = Self::set_image_state_with_reason(
                    self.control,
                    &mut self.pal,
                    &self.config,
                    &mut file_ctx,
                    ImageState::Aborted(ImageStateReason::InvalidDataProtocol),
                )?;
                return Err(OtaError::InvalidInterface);
            }
        }

        if self
            .active_interface
            .as_mut()
            .ok_or(OtaError::InvalidInterface)?
            .file_ctx()
            .self_test()
        {
            // If the OTA job is in the self_test state, alert the application layer.
            if matches!(self.image_state, ImageState::Testing(_)) {
                self.events
                    .enqueue(Events::StartSelfTest)
                    .map_err(|_| OtaError::SignalEventFailed)?;

                Ok(())
            } else {
                Err(OtaError::InvalidFile)
            }
        } else {
            if !self.platform_in_selftest() {
                // Received a valid context so send event to request file blocks
                self.events
                    .enqueue(Events::CreateFile)
                    .map_err(|_| OtaError::SignalEventFailed)?;
            } else {
                // Received a job that is not in self-test but platform is, so
                // reboot the device to allow roll back to previous image.
                rustot_log!(error, "Rejecting new image and rebooting: The platform is in the self-test state while the job is not.");
                self.events
                    .enqueue(Events::Restart(RestartReason::Restart(0)))
                    .map_err(|_| OtaError::SignalEventFailed)?;
            }
            Ok(())
        }
    }

    /// Request for data blocks
    fn request_data_handler(&mut self) -> Result<(), OtaError> {
        let file_ctx = self
            .active_interface
            .as_mut()
            .ok_or(OtaError::InvalidInterface)?
            .mut_file_ctx();
        if file_ctx.blocks_remaining > 0 {
            // Start the request timer
            self.request_timer
                .start(self.config.request_wait_ms)
                .map_err(|_| OtaError::Timer)?;

            if self.request_momentum <= self.config.max_request_momentum {
                // Each request increases the momentum until a response is
                // received. Too much momentum is interpreted as a failure to
                // communicate and will cause us to abort the OTA.
                self.request_momentum += 1;

                // Request data blocks
                data_interface!(self.request_file_block, &self.config)
            } else {
                // Stop the request timer
                self.request_timer.cancel().map_err(|_| OtaError::Timer)?;

                // Failed to send data request abort and close file.
                self.image_state = Self::set_image_state_with_reason(
                    self.control,
                    &mut self.pal,
                    &self.config,
                    file_ctx,
                    ImageState::Aborted(ImageStateReason::MomentumAbort),
                )?;

                rustot_log!(warn, "Shutdown [request_data_handler]");
                self.events
                    .enqueue(Events::Shutdown)
                    .map_err(|_| OtaError::SignalEventFailed)?;

                // Reset the request momentum
                self.request_momentum = 0;

                // Too many requests have been sent without a response or too
                // many failures when trying to publish the request message.
                // Abort.
                Err(OtaError::MomentumAbort)
            }
        } else {
            Err(OtaError::BlockOutOfRange)
        }
    }

    /// Upon receiving a new job document cancel current job if present and
    /// initiate new download
    fn job_notification_handler(&mut self, _data: &JobEventData<'_>) -> Result<(), OtaError> {
        // Stop the request timer
        self.request_timer.cancel().map_err(|_| OtaError::Timer)?;

        // Abort the current job
        // TODO: This should never write to current image flags?!
        self.pal
            .set_platform_image_state(ImageState::Aborted(ImageStateReason::NewerJob))?;
        self.ota_close()
    }

    /// Process incoming data blocks
    fn process_data_handler(&mut self, payload: &mut [u8]) -> Result<(), OtaError> {
        // Decode the file block received
        match self.ingest_data_block(payload) {
            Ok(true) => {
                let file_ctx = self
                    .active_interface
                    .as_mut()
                    .ok_or(OtaError::InvalidInterface)?
                    .mut_file_ctx();

                // File is completed! Update progress accordingly.
                let (status, reason, event) = if let Some(0) = file_ctx.file_type {
                    (
                        JobStatus::InProgress,
                        JobStatusReason::SigCheckPassed,
                        OtaEvent::Activate,
                    )
                } else {
                    (
                        JobStatus::Succeeded,
                        JobStatusReason::Accepted,
                        OtaEvent::UpdateComplete,
                    )
                };

                self.control
                    .update_job_status(file_ctx, &self.config, status, reason)?;

                // Send event to close file.
                self.events
                    .enqueue(Events::CloseFile)
                    .map_err(|_| OtaError::SignalEventFailed)?;

                // TODO: Last file block processed, increment the statistics
                // otaAgent.statistics.otaPacketsProcessed++;

                match event {
                    OtaEvent::Activate => {
                        self.events
                            .enqueue(Events::Restart(RestartReason::Activate(0)))
                            .map_err(|_| OtaError::SignalEventFailed)?;
                    }
                    event => self.pal.complete_callback(event)?,
                };
            }
            Ok(false) => {
                let file_ctx = self
                    .active_interface
                    .as_mut()
                    .ok_or(OtaError::InvalidInterface)?
                    .mut_file_ctx();

                // File block processed, increment the statistics.
                // otaAgent.statistics.otaPacketsProcessed++;

                // Reset the momentum counter since we received a good block
                self.request_momentum = 0;

                // We're actively receiving a file so update the job status as
                // needed
                self.control.update_job_status(
                    file_ctx,
                    &self.config,
                    JobStatus::InProgress,
                    JobStatusReason::Receiving,
                )?;

                if file_ctx.request_block_remaining > 1 {
                    file_ctx.request_block_remaining -= 1;
                } else {
                    // Start the request timer.
                    self.request_timer
                        .start(self.config.request_wait_ms)
                        .map_err(|_| OtaError::Timer)?;

                    self.events
                        .enqueue(Events::RequestFileBlock)
                        .map_err(|_| OtaError::SignalEventFailed)?;
                }
            }
            Err(e) => {
                let file_ctx = self
                    .active_interface
                    .as_mut()
                    .ok_or(OtaError::InvalidInterface)?
                    .mut_file_ctx();

                rustot_log!(error,
                    "Failed to ingest data block, rejecting image: ingest_data_block returned error"
                );

                // Call the platform specific code to reject the image
                // TODO: This should never write to current image flags?!
                self.pal.set_platform_image_state(ImageState::Rejected(
                    ImageStateReason::FailedIngest,
                ))?;

                // TODO: Pal reason
                self.control.update_job_status(
                    file_ctx,
                    &self.config,
                    JobStatus::Failed,
                    JobStatusReason::Pal(0),
                )?;

                // Stop the request timer.
                self.request_timer.cancel().map_err(|_| OtaError::Timer)?;

                // Send event to close file.
                self.events
                    .enqueue(Events::CloseFile)
                    .map_err(|_| OtaError::SignalEventFailed)?;

                self.pal.complete_callback(OtaEvent::Fail)?;
                rustot_log!(info, "Application callback! OtaEvent::Fail");
                return Err(e);
            }
        }

        // TODO: Application callback for event processed.
        // otaAgent.OtaAppCallback( OtaJobEventProcessed, ( const void * ) pEventData );
        Ok(())
    }

    /// Close file opened for download
    fn close_file_handler(&mut self) -> Result<(), OtaError> {
        self.ota_close()
    }

    /// Handle user interrupt to abort task
    fn user_abort_handler(&mut self) -> Result<(), OtaError> {
        rustot_log!(warn, "User abort OTA!");
        if let Some(ref mut interface) = self.active_interface {
            self.image_state = Self::set_image_state_with_reason(
                self.control,
                &mut self.pal,
                &self.config,
                interface.mut_file_ctx(),
                ImageState::Aborted(ImageStateReason::UserAbort),
            )?;
            self.ota_close()
        } else {
            Err(OtaError::NoActiveJob)
        }
    }

    /// Handle user interrupt to abort task
    fn shutdown_handler(&mut self) -> Result<(), OtaError> {
        rustot_log!(warn, "Shutting down OTA!");
        if let Some(ref mut interface) = self.active_interface {
            self.image_state = Self::set_image_state_with_reason(
                self.control,
                &mut self.pal,
                &self.config,
                interface.mut_file_ctx(),
                ImageState::Aborted(ImageStateReason::UserAbort),
            )?;
            self.ota_close()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ota::{
            agent::OtaAgent,
            pal::Version,
            test::{
                mock::{MockPal, MockTimer},
                test_job_doc,
            },
        },
        test::MockMqtt,
    };

    use super::*;

    #[test]
    fn version_check_success() {
        // The version check is run after swapping & rebooting, so the PAL will
        // return the version of the newly flashed firmware, and `FileContext`
        // will contain the `updated_by` version, which is the old firmware
        // version.

        let mqtt = MockMqtt::new();

        let request_timer = MockTimer::new();
        let self_test_timer = MockTimer::new();
        let pal = MockPal {};

        let mut agent = OtaAgent::builder(&mqtt, &mqtt, request_timer, pal)
            .with_self_test_timeout(self_test_timer, 32000)
            .build();

        let ota_job = test_job_doc();
        let mut file_ctx = FileContext::new_from(
            "Job-name",
            &ota_job,
            None,
            0,
            &Config::default(),
            Version::new(0, 1, 0),
        )
        .unwrap();

        let context = agent.state.context_mut();

        assert_eq!(context.handle_self_test_job(&mut file_ctx), Ok(()));

        assert!(
            matches!(context.image_state, ImageState::Testing(_)),
            "Unexpected image state"
        );
    }

    #[test]
    fn version_check_rejected() {
        let mqtt = MockMqtt::new();

        let request_timer = MockTimer::new();
        let self_test_timer = MockTimer::new();
        let pal = MockPal {};

        let mut agent = OtaAgent::builder(&mqtt, &mqtt, request_timer, pal)
            .with_self_test_timeout(self_test_timer, 32000)
            .build();

        let ota_job = test_job_doc();
        let mut file_ctx = FileContext::new_from(
            "Job-name",
            &ota_job,
            None,
            0,
            &Config::default(),
            Version::new(1, 1, 0),
        )
        .unwrap();

        let context = agent.state.context_mut();

        assert_eq!(context.handle_self_test_job(&mut file_ctx), Ok(()));

        assert!(
            matches!(context.image_state, ImageState::Rejected(_)),
            "Unexpected image state"
        );
    }

    #[test]
    fn version_check_allow_donwgrade() {
        let mqtt = MockMqtt::new();

        let request_timer = MockTimer::new();
        let self_test_timer = MockTimer::new();
        let pal = MockPal {};

        let mut agent = OtaAgent::builder(&mqtt, &mqtt, request_timer, pal)
            .with_self_test_timeout(self_test_timer, 32000)
            .allow_downgrade()
            .build();

        let ota_job = test_job_doc();
        let mut file_ctx = FileContext::new_from(
            "Job-name",
            &ota_job,
            None,
            0,
            &Config::default(),
            Version::new(1, 1, 0),
        )
        .unwrap();

        let context = agent.state.context_mut();

        assert_eq!(context.handle_self_test_job(&mut file_ctx), Ok(()));

        assert!(
            matches!(context.image_state, ImageState::Testing(_)),
            "Unexpected image state"
        );
    }
}
