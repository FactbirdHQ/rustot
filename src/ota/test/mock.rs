use crate::ota::{encoding::FileContext, pal::{ImageState, OtaPal, OtaPalError, PalImageState, Version}};

///
/// Mock timer used for unit tests. Implements `embedded_hal::timer::CountDown`
/// & `embedded_hal::timer::Cancel` traits.
///
pub struct MockTimer {
    pub is_started: bool,
}
impl MockTimer {
    pub fn new() -> Self {
        Self { is_started: false }
    }
}
impl embedded_hal::timer::CountDown for MockTimer {
    type Error = ();

    type Time = u32;

    fn try_start<T>(&mut self, _count: T) -> Result<(), Self::Error>
    where
        T: Into<Self::Time>,
    {
        self.is_started = true;
        Ok(())
    }

    fn try_wait(&mut self) -> nb::Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_hal::timer::Cancel for MockTimer {
    fn try_cancel(&mut self) -> Result<(), Self::Error> {
        self.is_started = false;
        Ok(())
    }
}

///
/// Mock Platform abstration layer used for unit tests. Implements `OtaPal`
/// trait.
///
pub struct MockPal {}

impl OtaPal for MockPal {
    type Error = ();

    fn abort(&mut self, _file: &FileContext) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn create_file_for_rx(&mut self, _file: &FileContext) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn get_platform_image_state(&self) -> Result<PalImageState, OtaPalError<Self::Error>> {
        Ok(PalImageState::Valid)
    }

    fn set_platform_image_state(
        &mut self,
        _image_state: ImageState,
    ) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn reset_device(&mut self) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn close_file(&mut self, _file: &FileContext) -> Result<(), OtaPalError<Self::Error>> {
        Ok(())
    }

    fn write_block(
        &mut self,
        _file: &FileContext,
        _block_offset: usize,
        block_payload: &[u8],
    ) -> Result<usize, OtaPalError<Self::Error>> {
        Ok(block_payload.len())
    }

    fn get_active_firmware_version(&self) -> Result<Version, OtaPalError<Self::Error>> {
        Ok(Version::default())
    }
}
