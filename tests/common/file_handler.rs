use core::ops::Deref;
use embedded_storage_async::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use rustot::ota::{
    encoding::json,
    pal::{OtaPal, OtaPalError, PalImageState},
};
use sha2::{Digest, Sha256};
use std::{
    convert::Infallible,
    io::{Cursor, Write},
};

#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Swap,
    Boot,
}

pub struct BlockFile {
    filebuf: Cursor<Vec<u8>>,
}

impl NorFlash for BlockFile {
    const WRITE_SIZE: usize = 1;

    const ERASE_SIZE: usize = 1;

    async fn erase(&mut self, _from: u32, _to: u32) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.filebuf.set_position(offset as u64);
        self.filebuf.write_all(bytes).unwrap();
        Ok(())
    }
}

impl ReadNorFlash for BlockFile {
    const READ_SIZE: usize = 1;

    async fn read(&mut self, _offset: u32, _bytes: &mut [u8]) -> Result<(), Self::Error> {
        todo!()
    }

    fn capacity(&self) -> usize {
        self.filebuf.get_ref().capacity()
    }
}

impl ErrorType for BlockFile {
    type Error = Infallible;
}

pub struct FileHandler {
    filebuf: Option<BlockFile>,
    compare_file_path: String,
    pub plateform_state: State,
}

impl FileHandler {
    #[allow(dead_code)]
    pub fn new(compare_file_path: String) -> Self {
        FileHandler {
            filebuf: None,
            compare_file_path,
            plateform_state: State::Boot,
        }
    }
}

impl OtaPal for FileHandler {
    type BlockWriter = BlockFile;

    async fn abort(
        &mut self,
        _file: &rustot::ota::encoding::FileContext,
    ) -> Result<(), OtaPalError> {
        Ok(())
    }

    async fn create_file_for_rx(
        &mut self,
        file: &rustot::ota::encoding::FileContext,
    ) -> Result<&mut Self::BlockWriter, OtaPalError> {
        Ok(self.filebuf.get_or_insert(BlockFile {
            filebuf: Cursor::new(Vec::with_capacity(file.filesize)),
        }))
    }

    async fn get_platform_image_state(&mut self) -> Result<PalImageState, OtaPalError> {
        Ok(match self.plateform_state {
            State::Swap => PalImageState::PendingCommit,
            State::Boot => PalImageState::Valid,
        })
    }

    async fn set_platform_image_state(
        &mut self,
        image_state: rustot::ota::pal::ImageState,
    ) -> Result<(), OtaPalError> {
        if matches!(image_state, rustot::ota::pal::ImageState::Accepted) {
            self.plateform_state = State::Boot;
        }

        Ok(())
    }

    async fn reset_device(&mut self) -> Result<(), OtaPalError> {
        Ok(())
    }

    async fn close_file(
        &mut self,
        file: &rustot::ota::encoding::FileContext,
    ) -> Result<(), OtaPalError> {
        if let Some(ref mut buf) = &mut self.filebuf {
            log::debug!(
                "Closing completed file. Len: {}/{} -> {}",
                buf.filebuf.get_ref().len(),
                file.filesize,
                file.filepath.as_str()
            );

            let expected_data = std::fs::read(self.compare_file_path.as_str()).unwrap();
            let mut expected_hasher = <Sha256 as Digest>::new();
            expected_hasher.update(&expected_data);
            let expected_hash = expected_hasher.finalize();

            log::info!(
                "Comparing {:?} with {:?}",
                self.compare_file_path,
                file.filepath.as_str()
            );
            assert_eq!(buf.filebuf.get_ref().len(), file.filesize);

            let mut hasher = <Sha256 as Digest>::new();
            hasher.update(buf.filebuf.get_ref());
            assert_eq!(hasher.finalize().deref(), expected_hash.deref());

            // Check file signature
            let signature = match file.signature.as_ref() {
                Some(json::Signature::Sha256Ecdsa(ref s)) => s.as_str(),
                sig => panic!("Unexpected signature format! {:?}", sig),
            };

            assert_eq!(signature, "This is my custom signature\\n");

            self.plateform_state = State::Swap;

            Ok(())
        } else {
            Err(OtaPalError::BadFileHandle)
        }
    }
}
