use core::ops::Deref;
use rustot::transfer::{
    encoding::json,
    pal::{BlockWriter, OtaPal, PalError, PalImageState, TransferPal},
    StatusDetailsExt,
};
use serde::ser::SerializeMap;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};

/// Custom status details to test StatusDetailsExt integration.
#[derive(Debug, Clone, Default)]
pub struct TestStatusDetails {
    pub firmware_version: &'static str,
}

impl StatusDetailsExt for TestStatusDetails {
    fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error> {
        map.serialize_entry("firmware_version", self.firmware_version)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Swap,
    Boot,
}

/// In-memory block writer backed by a `Vec<u8>`.
pub struct MemWriter {
    buf: Cursor<Vec<u8>>,
}

impl MemWriter {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: Cursor::new(Vec::with_capacity(capacity)),
        }
    }

    pub fn data(&self) -> &[u8] {
        self.buf.get_ref()
    }
}

impl BlockWriter for MemWriter {
    type Error = std::io::Error;

    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        self.buf.set_position(offset as u64);
        self.buf.write_all(data)
    }
}

pub struct FileHandler {
    writer: Option<MemWriter>,
    compare_file_path: String,
    pub plateform_state: State,
    /// If set, `close_file` will return this error instead of succeeding
    pub fail_close_with: Option<PalError>,
    pub extra_status: TestStatusDetails,
}

impl FileHandler {
    #[allow(dead_code)]
    pub fn new(compare_file_path: String) -> Self {
        FileHandler {
            writer: None,
            compare_file_path,
            plateform_state: State::Boot,
            fail_close_with: None,
            extra_status: TestStatusDetails {
                firmware_version: "0.1.0-test",
            },
        }
    }

    /// Configure the handler to fail on close_file with the given error
    #[allow(dead_code)]
    pub fn with_close_failure(mut self, error: PalError) -> Self {
        self.fail_close_with = Some(error);
        self
    }
}

impl TransferPal for FileHandler {
    type BlockWriter = MemWriter;
    type StatusDetails = TestStatusDetails;

    fn status_details(&self) -> Self::StatusDetails {
        self.extra_status.clone()
    }

    async fn abort(
        &mut self,
        _file: &rustot::transfer::encoding::JobContext<'_, impl rustot::transfer::StatusDetailsExt>,
    ) -> Result<(), PalError> {
        Ok(())
    }

    async fn create_file_for_rx(
        &mut self,
        file: &rustot::transfer::encoding::JobContext<'_, impl rustot::transfer::StatusDetailsExt>,
    ) -> Result<&mut Self::BlockWriter, PalError> {
        Ok(self.writer.get_or_insert(MemWriter::new(file.filesize)))
    }

    async fn close_file(
        &mut self,
        file: &rustot::transfer::encoding::JobContext<'_, impl rustot::transfer::StatusDetailsExt>,
    ) -> Result<(), PalError> {
        // Check for configured failure
        if let Some(error) = self.fail_close_with {
            log::info!("Simulating close_file failure: {:?}", error);
            return Err(error);
        }

        if let Some(ref writer) = self.writer {
            log::debug!(
                "Closing completed file. Len: {}/{} -> {}",
                writer.data().len(),
                file.filesize,
                file.filepath
            );

            let expected_data = std::fs::read(self.compare_file_path.as_str()).unwrap();
            let mut expected_hasher = <Sha256 as Digest>::new();
            expected_hasher.update(&expected_data);
            let expected_hash = expected_hasher.finalize();

            log::info!(
                "Comparing {:?} with {:?}",
                self.compare_file_path,
                file.filepath
            );
            assert_eq!(writer.data().len(), file.filesize);

            let mut hasher = <Sha256 as Digest>::new();
            hasher.update(writer.data());
            assert_eq!(hasher.finalize().deref(), expected_hash.deref());

            // Check file signature
            let signature = match file.signature {
                Some(json::Signature::Sha256Ecdsa(s)) => s,
                ref sig => panic!("Unexpected signature format! {:?}", sig),
            };

            assert_eq!(signature, "This is my custom signature");

            self.plateform_state = State::Swap;

            Ok(())
        } else {
            Err(PalError::BadFileHandle)
        }
    }
}

impl OtaPal for FileHandler {
    async fn get_platform_image_state(&mut self) -> Result<PalImageState, PalError> {
        Ok(match self.plateform_state {
            State::Swap => PalImageState::PendingCommit,
            State::Boot => PalImageState::Valid,
        })
    }

    async fn set_platform_image_state(
        &mut self,
        image_state: rustot::transfer::pal::ImageState,
    ) -> Result<(), PalError> {
        if matches!(image_state, rustot::transfer::pal::ImageState::Accepted) {
            self.plateform_state = State::Boot;
        }

        Ok(())
    }

    async fn reset_device(&mut self) -> Result<(), PalError> {
        Ok(())
    }
}
