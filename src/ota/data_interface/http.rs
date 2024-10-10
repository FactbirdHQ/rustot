use crate::ota::{
    config::Config,
    data_interface::{DataInterface, FileBlock, Protocol},
    encoding::FileContext,
    error::OtaError,
};

pub struct HttpInterface {}

impl HttpInterface {
    pub fn new() -> Self {
        Self {}
    }
}

impl DataInterface for HttpInterface {
    const PROTOCOL: Protocol = Protocol::Http;

    fn init_file_transfer(&self, _file_ctx: &mut FileContext) -> Result<(), OtaError> {
        Ok(())
    }

    fn request_file_blocks(
        &self,
        _file_ctx: &mut FileContext,
        _config: &Config,
    ) -> Result<(), OtaError> {
        Ok(())
    }
}
