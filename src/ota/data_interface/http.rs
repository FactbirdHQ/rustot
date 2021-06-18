use crate::ota::{
    config::Config,
    data_interface::{DataInterface, FileBlock, Protocol},
    encoding::FileContext,
};

pub struct HttpInterface {}

impl HttpInterface {
    pub fn new() -> Self {
        Self {}
    }
}

impl DataInterface for HttpInterface {
    const PROTOCOL: Protocol = Protocol::Http;

    fn init_file_transfer(&self, _file_ctx: &mut FileContext) -> Result<(), ()> {
        Ok(())
    }

    fn request_file_block(&self, _file_ctx: &mut FileContext, _config: &Config) -> Result<(), ()> {
        Ok(())
    }

    fn decode_file_block<'b>(
        &self,
        _file_ctx: &mut FileContext,
        _payload: &'b mut [u8],
    ) -> Result<FileBlock<'b>, ()> {
        unimplemented!()
    }

    fn cleanup(&self, _file_ctx: &mut FileContext, _config: &Config) -> Result<(), ()> {
        println!("Cleanup DataInterface for Http");
        Ok(())
    }
}
