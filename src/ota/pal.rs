//! Platform abstraction trait for OTA updates
use super::encoding::OtaJobContext;
use super::StatusDetailsExt;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ImageStateReason {
    NewerJob,
    FailedIngest,
    MomentumAbort,
    ImageStateMismatch,
    SignatureCheckPassed,
    InvalidDataProtocol,
    UserAbort,
    VersionCheck,
    Pal(OtaPalError),
}

impl ImageStateReason {
    /// Returns a numeric error code for this reason.
    ///
    /// Error codes in the 2xxx range are image state reasons.
    /// Pal errors delegate to the inner OtaPalError (1xxx range).
    pub fn error_code(&self) -> u16 {
        match self {
            ImageStateReason::NewerJob => 2001,
            ImageStateReason::FailedIngest => 2002,
            ImageStateReason::MomentumAbort => 2003,
            ImageStateReason::ImageStateMismatch => 2004,
            ImageStateReason::InvalidDataProtocol => 2005,
            ImageStateReason::UserAbort => 2006,
            ImageStateReason::VersionCheck => 2007,
            ImageStateReason::SignatureCheckPassed => 0, // Not an error
            ImageStateReason::Pal(e) => e.error_code(),
        }
    }

    /// Returns a short string identifier for this reason suitable for status reporting.
    pub fn as_reason_str(&self) -> &'static str {
        match self {
            ImageStateReason::NewerJob => "newer_job",
            ImageStateReason::FailedIngest => "failed_ingest",
            ImageStateReason::MomentumAbort => "momentum_abort",
            ImageStateReason::ImageStateMismatch => "state_mismatch",
            ImageStateReason::InvalidDataProtocol => "invalid_protocol",
            ImageStateReason::UserAbort => "user_abort",
            ImageStateReason::VersionCheck => "version_check",
            ImageStateReason::SignatureCheckPassed => "sig_check_passed",
            ImageStateReason::Pal(e) => e.as_reason_str(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ImageState {
    Unknown,
    Aborted(ImageStateReason),
    Rejected(ImageStateReason),
    Accepted,
    Testing(ImageStateReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum OtaPalError {
    SignatureCheckFailed,
    FileWriteFailed,
    FileTooLarge,
    FileCloseFailed,
    BadFileHandle,
    Unsupported,
    BadImageState,
    CommitFailed,
    VersionCheck,
    Other,
}

impl OtaPalError {
    /// Returns a numeric error code for this error.
    ///
    /// Error codes in the 1xxx range are PAL-level errors.
    pub fn error_code(&self) -> u16 {
        match self {
            OtaPalError::SignatureCheckFailed => 1001,
            OtaPalError::FileWriteFailed => 1002,
            OtaPalError::FileTooLarge => 1003,
            OtaPalError::FileCloseFailed => 1004,
            OtaPalError::BadFileHandle => 1005,
            OtaPalError::Unsupported => 1006,
            OtaPalError::BadImageState => 1007,
            OtaPalError::CommitFailed => 1008,
            OtaPalError::VersionCheck => 1009,
            OtaPalError::Other => 1099,
        }
    }

    /// Returns a short string identifier for this error suitable for status reporting.
    pub fn as_reason_str(&self) -> &'static str {
        match self {
            OtaPalError::SignatureCheckFailed => "sig_check_failed",
            OtaPalError::FileWriteFailed => "file_write_failed",
            OtaPalError::FileTooLarge => "file_too_large",
            OtaPalError::FileCloseFailed => "file_close_failed",
            OtaPalError::BadFileHandle => "bad_file_handle",
            OtaPalError::Unsupported => "unsupported",
            OtaPalError::BadImageState => "bad_image_state",
            OtaPalError::CommitFailed => "commit_failed",
            OtaPalError::VersionCheck => "version_check",
            OtaPalError::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PalImageState {
    /// the new firmware image is in the self test phase
    PendingCommit,
    /// the new firmware image is already committed
    Valid,
    /// the new firmware image is invalid or non-existent
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum OtaEvent {
    /// OTA update is authenticated and ready to activate.
    Activate,
    /// OTA update failed. Unable to use this update.
    Fail,
    /// OTA job is now ready for optional user self tests.
    StartTest,

    SelfTestFailed,

    UpdateComplete,
}

/// Trait for writing OTA data blocks to storage.
///
/// This is the abstraction between the OTA orchestrator and the storage
/// backend. The orchestrator calls `write(offset, data)` for each received
/// block.
///
/// A blanket implementation is provided for all [`NorFlash`] types, so
/// embedded users can pass their flash partition directly. `std` users can
/// implement this for files, IPC streams, or any other target.
pub trait BlockWriter {
    type Error: core::fmt::Debug;

    /// Write `data` at the given byte `offset`.
    ///
    /// For random-access backends (flash, files): seek to `offset` and write.
    /// For streaming backends (swupdate IPC): `offset` may be ignored if the
    /// data interface delivers blocks sequentially (e.g. HTTP Range requests).
    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error>;
}

/// Blanket implementation: any [`NorFlash`] is a [`BlockWriter`].
impl<T: embedded_storage_async::nor_flash::NorFlash> BlockWriter for T {
    type Error = T::Error;

    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        embedded_storage_async::nor_flash::NorFlash::write(self, offset, data).await
    }
}

/// File-based block writer for `std` targets.
///
/// Writes OTA blocks to a file on disk using tokio. Suitable for testing
/// or Linux-based OTA targets.
#[cfg(feature = "std")]
pub struct FileWriter {
    file: tokio::fs::File,
}

#[cfg(feature = "std")]
impl FileWriter {
    /// Create a new `FileWriter` for the given file path.
    ///
    /// Creates or truncates the file.
    pub async fn create(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        Ok(Self {
            file: tokio::fs::File::create(path).await?,
        })
    }

    /// Create from an already-opened tokio file.
    pub fn from_file(file: tokio::fs::File) -> Self {
        Self { file }
    }
}

#[cfg(feature = "std")]
impl BlockWriter for FileWriter {
    type Error = std::io::Error;

    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        self.file
            .seek(std::io::SeekFrom::Start(offset as u64))
            .await?;
        self.file.write_all(data).await
    }
}

/// Platform abstraction layer for OTA jobs
pub trait OtaPal {
    type BlockWriter: BlockWriter;

    /// Extra status details to include in job status updates.
    ///
    /// Implement [`StatusDetailsExt`] on a custom struct to add
    /// platform-specific fields (e.g. device temperature, battery level)
    /// to every job status update sent to AWS IoT.
    ///
    /// Use `()` if no extra fields are needed.
    type StatusDetails: super::StatusDetailsExt + Clone;

    /// Returns a reference to the extra status details.
    fn status_details(&self) -> Self::StatusDetails;

    /// OTA abort.
    ///
    /// The user may register a callback function when initializing the OTA
    /// Agent. This callback is used to override the behavior of how a job is
    /// aborted.
    ///
    /// - `file`: [`FileContext`] File description of the job being aborted
    async fn abort(
        &mut self,
        file: &OtaJobContext<'_, impl StatusDetailsExt>,
    ) -> Result<(), OtaPalError>;

    /// Activate the newest MCU image received via OTA.
    ///
    /// This function shall do whatever is necessary to activate the newest MCU
    /// firmware received via OTA. It is typically just a reset of the device.
    ///
    /// **note** This function SHOULD not return. If it does, the platform
    /// doesn't support an automatic reset or an error occurred.
    ///
    /// **return**: The OTA PAL layer error code combined with the MCU specific
    /// error code.
    async fn activate_new_image(&mut self) -> Result<(), OtaPalError> {
        self.reset_device().await
    }

    /// OTA create file to store received data.
    ///
    /// The user may register a callback function when initializing the OTA
    /// Agent. This callback is used to override the behavior of how a new file
    /// is created.
    ///
    /// - `file`: [`FileContext`] File description of the job being aborted
    async fn create_file_for_rx(
        &mut self,
        file: &OtaJobContext<'_, impl StatusDetailsExt>,
    ) -> Result<&mut Self::BlockWriter, OtaPalError>;

    /// Get the state of the OTA update image.
    ///
    /// We read this at OTA_Init time and when the latest OTA job reports itself
    /// in self test. If the update image is in the "pending commit" state, we
    /// start a self test timer to assure that we can successfully connect to
    /// the OTA services and accept the OTA update image within a reasonable
    /// amount of time (user configurable). If we don't satisfy that
    /// requirement, we assume there is something wrong with the firmware and
    /// automatically reset the device, causing it to roll back to the
    /// previously known working code.
    ///
    /// If the update image state is not in "pending commit," the self test
    /// timer is not started.
    ///
    /// **return** An [`PalImageState`].
    async fn get_platform_image_state(&mut self) -> Result<PalImageState, OtaPalError>;

    /// Attempt to set the state of the OTA update image.
    ///
    /// Do whatever is required by the platform to Accept/Reject the OTA update
    /// image (or bundle). Refer to the PAL implementation to determine what
    /// happens on your platform.
    ///
    /// - `state`: [`ImageState`] The desired state of the OTA update image.
    ///
    /// **return** The [`OtaPalError`] error code combined with the MCU specific
    /// error code.
    async fn set_platform_image_state(
        &mut self,
        image_state: ImageState,
    ) -> Result<(), OtaPalError>;

    /// Reset the device.
    ///
    /// This function shall reset the MCU and cause a reboot of the system.
    ///
    /// **note** This function SHOULD not return. If it does, the platform
    /// doesn't support an automatic reset or an error occurred.
    ///
    /// **return** The OTA PAL layer error code combined with the MCU specific
    /// error code.
    async fn reset_device(&mut self) -> Result<(), OtaPalError>;

    /// Authenticate and close the underlying receive file in the specified OTA
    /// context.
    ///
    /// If the signature verification fails, file close should still be
    /// attempted.
    ///
    /// - `file`: [`FileContext`] File description of the job being aborted
    ///
    /// **return** The OTA PAL layer error code combined with the MCU specific
    /// error code.
    async fn close_file(
        &mut self,
        file: &OtaJobContext<'_, impl StatusDetailsExt>,
    ) -> Result<(), OtaPalError>;

    /// OTA update complete.
    ///
    /// The user may register a callback function when initializing the OTA
    /// Agent. This callback is used to notify the main application when the OTA
    /// update job is complete. Typically, it is used to reset the device after
    /// a successful update by calling `OtaPal::activate_new_image()` and may
    /// also be used to kick off user specified self tests during the Self Test
    /// phase. If the user does not supply a custom callback function, a default
    /// callback handler is used that automatically calls
    /// `OtaPal::activate_new_image()` after a successful update.
    ///
    /// **note**:
    ///
    /// The callback function is called with one of the following arguments:
    ///
    /// - `OtaEvent::Activate`      OTA update is authenticated and ready to
    ///   activate.
    /// - `OtaEvent::Fail`          OTA update failed. Unable to use this
    ///   update.
    /// - `OtaEvent::StartTest`     OTA job is now ready for optional user self
    ///   tests.
    ///
    /// When `OtaEvent::Activate` is received, the job status details have been
    /// updated with the state as ready for Self Test. After reboot, the new
    /// firmware will (normally) be notified that it is in the Self Test phase
    /// via the callback and the application may then optionally run its own
    /// tests before committing the new image.
    ///
    /// If the callback function is called with a result of `OtaEvent::Fail`,
    /// the OTA update job has failed in some way and should be rejected.
    ///
    /// - `event` [`OtaEvent`] An OTA update event from the `OtaEvent` enum.
    async fn complete_callback(&mut self, event: OtaEvent) -> Result<(), OtaPalError> {
        match event {
            OtaEvent::Activate => self.activate_new_image().await,
            OtaEvent::Fail | OtaEvent::UpdateComplete => {
                // Nothing special to do. The OTA agent handles it
                Ok(())
            }
            OtaEvent::StartTest => {
                // Accept the image since it was a good transfer
                // and networking and services are all working.
                self.set_platform_image_state(ImageState::Accepted).await?;
                Ok(())
            }
            OtaEvent::SelfTestFailed => {
                // Requires manual activation of previous image as self-test for
                // new image downloaded failed.*/
                error!("Self-test failed, shutting down OTA Agent.");

                // Shutdown OTA Agent, if it is required that the unsubscribe operations are not
                // performed while shutting down please set the second parameter to 0 instead of 1.
                Ok(())
            }
        }
    }
}
