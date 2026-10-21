//! Platform abstraction traits for file transfers and OTA updates.
use super::encoding::{FileContext, FileType};

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
    Pal(PalError),
}

impl ImageStateReason {
    /// Returns a numeric error code for this reason.
    ///
    /// Error codes in the 2xxx range are image state reasons.
    /// Pal errors delegate to the inner PalError (1xxx range).
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
pub enum PalError {
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

impl PalError {
    /// Returns a numeric error code for this error.
    ///
    /// Error codes in the 1xxx range are PAL-level errors.
    pub fn error_code(&self) -> u16 {
        match self {
            PalError::SignatureCheckFailed => 1001,
            PalError::FileWriteFailed => 1002,
            PalError::FileTooLarge => 1003,
            PalError::FileCloseFailed => 1004,
            PalError::BadFileHandle => 1005,
            PalError::Unsupported => 1006,
            PalError::BadImageState => 1007,
            PalError::CommitFailed => 1008,
            PalError::VersionCheck => 1009,
            PalError::Other => 1099,
        }
    }

    /// Returns a short string identifier for this error suitable for status reporting.
    pub fn as_reason_str(&self) -> &'static str {
        match self {
            PalError::SignatureCheckFailed => "sig_check_failed",
            PalError::FileWriteFailed => "file_write_failed",
            PalError::FileTooLarge => "file_too_large",
            PalError::FileCloseFailed => "file_close_failed",
            PalError::BadFileHandle => "bad_file_handle",
            PalError::Unsupported => "unsupported",
            PalError::BadImageState => "bad_image_state",
            PalError::CommitFailed => "commit_failed",
            PalError::VersionCheck => "version_check",
            PalError::Other => "other",
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

/// Trait for writing data blocks to storage.
///
/// This is the abstraction between the transfer orchestrator and the storage
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
#[cfg(feature = "sequential_storage")]
impl<T: embedded_storage_async::nor_flash::NorFlash> BlockWriter for T {
    type Error = T::Error;

    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        embedded_storage_async::nor_flash::NorFlash::write(self, offset, data).await
    }
}

/// File-based block writer for `std` targets.
///
/// Writes data blocks to a file on disk using tokio. Suitable for testing
/// or Linux-based targets.
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

/// Platform abstraction for file transfers.
///
/// Implement this trait for any target that receives downloaded files.
/// This covers storage creation, finalization, cleanup on abort, and
/// optional extra status details for job status reporting.
///
/// A job may contain several files. The engine downloads them in document
/// order, calling [`create_file_for_rx`](Self::create_file_for_rx) and
/// [`close_file`](Self::close_file) once per file. Nothing should be applied
/// until the whole job has completed: if any file fails, the engine calls
/// [`abort`](Self::abort) once and every file staged for the job must be
/// discarded.
///
/// For OTA updates that also need image state management, self-test and
/// device reset, implement [`OtaPal`] (which extends this trait).
pub trait TransferPal {
    type BlockWriter: BlockWriter;

    /// Extra status details to include in job status updates.
    ///
    /// Implement [`StatusDetailsExt`](super::StatusDetailsExt) on a custom
    /// struct to add platform-specific fields (e.g. device temperature,
    /// battery level) to every job status update sent to AWS IoT.
    ///
    /// Use `()` if no extra fields are needed.
    type StatusDetails: super::StatusDetailsExt + Clone;

    /// Returns the extra status details.
    fn status_details(&self) -> Self::StatusDetails;

    /// Whether a completed job containing `file` must be activated before it
    /// can succeed.
    ///
    /// If any file in the job requires activation, the job is left
    /// `IN_PROGRESS` with self-test `ready` once all files are downloaded,
    /// instead of being marked `SUCCEEDED`. [`Transfer::perform_ota`] then
    /// fires [`OtaEvent::Activate`]; with [`Transfer::perform`] the caller
    /// finishes the job itself (e.g. via
    /// [`JobAgent::succeed_job`](crate::jobs::stream::JobAgent::succeed_job)).
    ///
    /// Defaults to `true` for [`FileType::Firmware`] (`fileType == 0`).
    ///
    /// [`Transfer::perform`]: super::Transfer::perform
    /// [`Transfer::perform_ota`]: super::Transfer::perform_ota
    fn requires_activation(&self, file: &FileContext<'_>) -> bool {
        file.file_type == Some(FileType::Firmware)
    }

    /// Create or open the file to store received data.
    async fn create_file_for_rx(
        &mut self,
        file: &FileContext<'_>,
    ) -> Result<&mut Self::BlockWriter, PalError>;

    /// Authenticate and close the received file.
    ///
    /// If signature verification fails, file close should still be attempted.
    async fn close_file(&mut self, file: &FileContext<'_>) -> Result<(), PalError>;

    /// Abort the job and discard everything staged for it.
    ///
    /// Called once when a transfer fails or is cancelled, regardless of how
    /// many files were already closed. Delete partially written and completed
    /// files from this job, release file handles, etc.
    async fn abort(&mut self, job_name: &str) -> Result<(), PalError>;
}

/// Platform abstraction for OTA updates.
///
/// Extends [`TransferPal`] with image state management, self-test, device
/// reset, and OTA lifecycle callbacks. Activation applies to the job as a
/// whole: it happens once, after every file has been downloaded and
/// verified. Which files require it is decided by
/// [`TransferPal::requires_activation`].
pub trait OtaPal: TransferPal {
    /// Get the state of the OTA update image.
    ///
    /// Read at init time and when the latest OTA job reports itself in
    /// self test. If the image is in "pending commit" state, a self test
    /// timer is started. If the timer expires, the device resets to roll
    /// back to the previous firmware.
    async fn get_platform_image_state(&mut self) -> Result<PalImageState, PalError>;

    /// Set the state of the OTA update image.
    ///
    /// Accept, reject, or abort the firmware image on the platform.
    async fn set_platform_image_state(&mut self, image_state: ImageState) -> Result<(), PalError>;

    /// Activate the newest MCU image received via OTA.
    ///
    /// Typically just a device reset. This function SHOULD not return.
    async fn activate_new_image(&mut self) -> Result<(), PalError> {
        self.reset_device().await
    }

    /// Reset the device.
    ///
    /// This function SHOULD not return. If it does, the platform doesn't
    /// support automatic reset or an error occurred.
    async fn reset_device(&mut self) -> Result<(), PalError>;

    /// OTA lifecycle callback.
    ///
    /// Called with:
    /// - `Activate`: update is authenticated and ready to activate
    /// - `Fail`: update failed
    /// - `StartTest`: job is ready for optional self tests
    /// - `UpdateComplete`: transfer completed and no file requires activation
    async fn complete_callback(&mut self, event: OtaEvent) -> Result<(), PalError> {
        match event {
            OtaEvent::Activate => self.activate_new_image().await,
            OtaEvent::Fail | OtaEvent::UpdateComplete => Ok(()),
            OtaEvent::StartTest => {
                self.set_platform_image_state(ImageState::Accepted).await?;
                Ok(())
            }
            OtaEvent::SelfTestFailed => {
                error!("Self-test failed, shutting down OTA Agent.");
                Ok(())
            }
        }
    }
}
