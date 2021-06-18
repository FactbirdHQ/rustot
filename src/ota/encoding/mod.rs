#[cfg(feature = "ota_mqtt_data")]
pub mod cbor;
pub mod json;

use core::ops::{Deref, DerefMut};
use core::str::FromStr;
use serde::{Serialize, Serializer};

use crate::job::data_types::StatusDetails;

use self::json::OtaJob;

use super::{config::Config, pal::Version};

#[derive(Clone, PartialEq)]
pub struct Bitmap(bitmaps::Bitmap<32>);

impl Bitmap {
    pub fn new(file_size: usize, block_size: usize, block_offset: u32) -> Self {
        // Total number of blocks in file, rounded up
        let total_num_blocks = (file_size + block_size - 1) / block_size;

        Self(bitmaps::Bitmap::mask(core::cmp::min(
            31,
            total_num_blocks - block_offset as usize,
        )))
    }
}

impl Deref for Bitmap {
    type Target = bitmaps::Bitmap<32>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Bitmap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Serialize for Bitmap {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Serializer::serialize_bytes(serializer, &self.deref().into_value().to_le_bytes())
    }
}



/// A `FileContext` denotes an active context of a single file. An ota job can
/// contain multiple files, each with their own `FileContext` built from a
/// corresponding `FileDescription`.
#[derive(Clone)]
pub struct FileContext {
    pub(crate) filepath: heapless::String<64>,
    pub(crate) filesize: usize,
    pub(crate) fileid: u8,
    pub(crate) certfile: heapless::String<64>,
    pub(crate) update_data_url: Option<heapless::String<64>>,
    pub(crate) auth_scheme: Option<heapless::String<64>>,
    pub(crate) sig_sha1_rsa: heapless::String<64>,
    pub(crate) file_attributes: Option<u32>,

    pub(crate) status_details: StatusDetails,
    pub(crate) block_offset: u32,
    pub(crate) blocks_remaining: usize,
    pub(crate) request_block_remaining: u32,
    pub(crate) stream_name: heapless::String<64>,
    pub(crate) bitmap: Bitmap,
}

impl FileContext {
    pub fn new_from(
        ota_job: &OtaJob,
        status_details: Option<StatusDetails>,
        file_idx: usize,
        config: &Config,
        current_version: Version,
    ) -> Result<Self, ()> {
        let file_desc = ota_job.files.get(file_idx).ok_or(())?.clone();

        // Initialize new `status_details' if not already present
        let mut status = if let Some(details) = status_details {
            details
        } else {
            StatusDetails::new()
        };

        status
            .insert(
                heapless::String::from("updated_by"),
                current_version.to_string(),
            )
            .map_err(drop)?;

        Ok(FileContext {
            filepath: file_desc.filepath,
            filesize: file_desc.filesize,
            fileid: file_desc.fileid,
            certfile: file_desc.certfile,
            update_data_url: file_desc.update_data_url,
            auth_scheme: file_desc.auth_scheme,
            sig_sha1_rsa: file_desc.sig_sha1_rsa,
            file_attributes: file_desc.file_attributes,

            status_details: status,

            block_offset: 0,
            request_block_remaining: config.max_blocks_per_request,
            blocks_remaining: (file_desc.filesize + config.block_size - 1) / config.block_size,
            stream_name: ota_job.streamname.clone(),
            bitmap: Bitmap::new(file_desc.filesize, config.block_size, 0),
        })
    }

    pub fn self_test(&self) -> bool {
        self.status_details
            .get(&heapless::String::from("self_test"))
            .map(|f| f.as_str() == "ready")
            .unwrap_or(false)
    }

    pub fn updated_by(&self) -> Option<Version> {
        self.status_details
            .get(&heapless::String::from("updated_by"))
            .and_then(|s| Version::from_str(s.as_str()).ok())
    }
}
