use crate::ota::{
    encoding::FileContext,
    pal::{ImageState, OtaPal, OtaPalError, PalImageState, Version},
};

use super::TEST_TIMER_HZ;

///
/// Mock timer used for unit tests. Implements `fugit_timer::Timer` trait.
///
pub struct MockTimer {
    pub is_started: bool,
}
impl MockTimer {
    pub fn new() -> Self {
        Self { is_started: false }
    }
}

impl fugit_timer::Timer<TEST_TIMER_HZ> for MockTimer {
    type Error = ();

    fn now(&mut self) -> fugit_timer::TimerInstantU32<TEST_TIMER_HZ> {
        todo!()
    }

    fn start(
        &mut self,
        _duration: fugit_timer::TimerDurationU32<TEST_TIMER_HZ>,
    ) -> Result<(), Self::Error> {
        self.is_started = true;
        Ok(())
    }

    fn cancel(&mut self) -> Result<(), Self::Error> {
        self.is_started = false;
        Ok(())
    }

    fn wait(&mut self) -> nb::Result<(), Self::Error> {
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
        _image_state: ImageState<Self::Error>,
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
        Ok(Version::new(1, 0, 0))
    }
}
