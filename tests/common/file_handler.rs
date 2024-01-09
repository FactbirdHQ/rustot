use core::ops::Deref;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use rustot::ota::{
    self,
    pal::{OtaPal, OtaPalError, PalImageState},
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Cursor, Read, Write},
};

pub struct FileHandler {
    filebuf: Option<Cursor<Vec<u8>>>,
    compare_file_path: String,
}

impl FileHandler {
    pub fn new(compare_file_path: String) -> Self {
        FileHandler {
            filebuf: None,
            compare_file_path,
        }
    }
}

impl OtaPal for FileHandler {
    async fn abort(
        &mut self,
        _file: &rustot::ota::encoding::FileContext,
    ) -> Result<(), OtaPalError> {
        Ok(())
    }

    async fn create_file_for_rx(
        &mut self,
        file: &rustot::ota::encoding::FileContext,
    ) -> Result<(), OtaPalError> {
        self.filebuf = Some(Cursor::new(Vec::with_capacity(file.filesize)));
        Ok(())
    }

    async fn get_platform_image_state(&mut self) -> Result<PalImageState, OtaPalError> {
        Ok(PalImageState::Valid)
    }

    async fn set_platform_image_state(
        &mut self,
        _image_state: rustot::ota::pal::ImageState,
    ) -> Result<(), OtaPalError> {
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
                buf.get_ref().len(),
                file.filesize,
                file.filepath.as_str()
            );

            let mut expected_data = std::fs::read(self.compare_file_path.as_str()).unwrap();
            let mut expected_hasher = <Sha256 as Digest>::new();
            expected_hasher.update(&expected_data);
            let expected_hash = expected_hasher.finalize();

            log::info!(
                "Comparing {:?} with {:?}",
                self.compare_file_path,
                file.filepath.as_str()
            );
            assert_eq!(buf.get_ref().len(), file.filesize);

            let mut hasher = <Sha256 as Digest>::new();
            hasher.update(&buf.get_ref());
            assert_eq!(hasher.finalize().deref(), expected_hash.deref());

            // Check file signature
            match &file.signature {
                ota::encoding::json::Signature::Sha1Rsa(_) => {
                    panic!("Unexpected signature format: Sha1Rsa. Expected Sha256Ecdsa")
                }
                ota::encoding::json::Signature::Sha256Rsa(_) => {
                    panic!("Unexpected signature format: Sha256Rsa. Expected Sha256Ecdsa")
                }
                ota::encoding::json::Signature::Sha1Ecdsa(_) => {
                    panic!("Unexpected signature format: Sha1Ecdsa. Expected Sha256Ecdsa")
                }
                ota::encoding::json::Signature::Sha256Ecdsa(sig) => {
                    assert_eq!(sig.as_str(), "This is my custom signature\\n")
                }
            }

            Ok(())
        } else {
            Err(OtaPalError::BadFileHandle)
        }
    }

    async fn write_block(
        &mut self,
        _file: &mut rustot::ota::encoding::FileContext,
        block_offset: usize,
        block_payload: &[u8],
    ) -> Result<usize, OtaPalError> {
        if let Some(ref mut buf) = &mut self.filebuf {
            buf.set_position(block_offset as u64);
            buf.write(block_payload)
                .map_err(|_e| OtaPalError::FileWriteFailed)?;
            Ok(block_payload.len())
        } else {
            Err(OtaPalError::BadFileHandle)
        }
    }
}
