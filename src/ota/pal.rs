//! Platform abstraction trait for OTA updates

use crate::{jobs::FileDescription, ota::ota::ImageState};

#[derive(Debug)]
pub enum OtaPalError<E> {
    SignatureCheckFailed,
    FileWriteFailed,
    FileTooLarge,
    FileCloseFailed,
    BadFileHandle,
    Unsupported,
    CommitFailed,
    BadImageState,
    Custom(E),
}

pub enum PalImageState {
    /// the new firmware image is in the self test phase
    PendingCommit,
    /// the new firmware image is already committed
    Valid,
    /// the new firmware image is invalid or non-existent
    Invalid,
}

pub enum OtaEvent {
    /// OTA update is authenticated and ready to activate.
    Activate,
    /// OTA update failed. Unable to use this update.
    Fail,
    /// OTA job is now ready for optional user self tests.
    StartTest,
}

/// Platform abstraction layer for OTA jobs
pub trait OtaPal {
    type Error;

    /// OTA abort.
    ///
    /// The user may register a callback function when initializing the OTA
    /// Agent. This callback is used to override the behavior of how a job is
    /// aborted.
    ///
    /// - `file`: [`FileDescription`] File description of the job being aborted
    fn abort(&mut self, file: &FileDescription) -> Result<(), OtaPalError<Self::Error>>;

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
    fn activate_new_image(&mut self) -> Result<(), OtaPalError<Self::Error>> {
        self.reset_device()
    }

    /// OTA create file to store received data.
    ///
    /// The user may register a callback function when initializing the OTA
    /// Agent. This callback is used to override the behavior of how a new file
    /// is created.
    ///
    /// - `file`: [`FileDescription`] File description of the job being aborted
    fn create_file_for_rx(
        &mut self,
        file: &FileDescription,
    ) -> Result<(), OtaPalError<Self::Error>>;

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
    fn get_platform_image_state(&mut self) -> Result<PalImageState, OtaPalError<Self::Error>>;

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
    fn set_platform_image_state(
        &mut self,
        image_state: ImageState,
    ) -> Result<(), OtaPalError<Self::Error>>;

    /// Reset the device.
    ///
    /// This function shall reset the MCU and cause a reboot of the system.
    ///
    /// **note** This function SHOULD not return. If it does, the platform
    /// doesn't support an automatic reset or an error occurred.
    ///
    /// **return** The OTA PAL layer error code combined with the MCU specific
    /// error code.
    fn reset_device(&mut self) -> Result<(), OtaPalError<Self::Error>>;

    /// Authenticate and close the underlying receive file in the specified OTA
    /// context.
    ///
    /// If the signature verification fails, file close should still be
    /// attempted.
    ///
    /// - `file`: [`FileDescription`] File description of the job being aborted
    ///
    /// **return** The OTA PAL layer error code combined with the MCU specific
    /// error code.
    fn close_file(&mut self, file: &FileDescription) -> Result<(), OtaPalError<Self::Error>>;

    /// Write a block of data to the specified file at the given offset.
    ///
    /// - `file`: [`FileDescription`] File description of the job being aborted.
    /// - `block_offset`: Byte offset to write to from the beginning of the
    ///   file.
    /// - `block_payload`: Byte array of data to write.
    ///
    /// **return** The number of bytes written on a success, or a negative error
    /// code from the platform abstraction layer.
    fn write_block(
        &mut self,
        file: &FileDescription,
        block_offset: usize,
        block_payload: &[u8],
    ) -> Result<usize, OtaPalError<Self::Error>>;

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
    ///      OtaEvent::Activate      OTA update is authenticated and ready to activate.
    ///      OtaEvent::Fail          OTA update failed. Unable to use this update.
    ///      OtaEvent::StartTest     OTA job is now ready for optional user self tests.
    ///
    /// When OtaEvent::Activate is received, the job status details have been
    /// updated with the state as ready for Self Test. After reboot, the new
    /// firmware will (normally) be notified that it is in the Self Test phase
    /// via the callback and the application may then optionally run its own
    /// tests before committing the new image.
    ///
    /// If the callback function is called with a result of OtaEvent::Fail, the
    /// OTA update job has failed in some way and should be rejected.
    ///
    /// - `event` [`OtaEvent`] An OTA update event from the OtaEvent enum.
    fn complete_callback(&mut self, event: OtaEvent) -> Result<(), OtaPalError<Self::Error>> {
        match event {
            OtaEvent::Activate => self.activate_new_image(),
            OtaEvent::Fail => {
                // Nothing special to do. The OTA agent handles it
                Ok(())
            }
            OtaEvent::StartTest => {
                // Accept the image since it was a good transfer
                // and networking and services are all working.
                self.set_platform_image_state(ImageState::Accepted)
            }
        }
    }
}
