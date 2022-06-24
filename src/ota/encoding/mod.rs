#[cfg(feature = "ota_mqtt_data")]
pub mod cbor;
pub mod json;

use core::ops::{Deref, DerefMut};
use core::str::FromStr;
use serde::{Serialize, Serializer};

use crate::jobs::StatusDetails;

use self::json::{JobStatusReason, OtaJob, Signature};

use super::error::OtaError;
use super::{config::Config, pal::Version};

#[derive(Clone, PartialEq)]
pub struct Bitmap(bitmaps::Bitmap<32>);

impl Bitmap {
    pub fn new(file_size: usize, block_size: usize, block_offset: u32) -> Self {
        // Total number of blocks in file, rounded up
        let total_num_blocks = (file_size + block_size - 1) / block_size;

        Self(bitmaps::Bitmap::mask(core::cmp::min(
            32 - 1,
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
    pub filepath: heapless::String<64>,
    pub filesize: usize,
    pub fileid: u8,
    pub certfile: heapless::String<64>,
    pub update_data_url: Option<heapless::String<64>>,
    pub auth_scheme: Option<heapless::String<64>>,
    pub signature: Signature,
    pub file_type: Option<u32>,

    pub status_details: StatusDetails,
    pub block_offset: u32,
    pub blocks_remaining: usize,
    pub request_block_remaining: u32,
    pub job_name: heapless::String<64>,
    pub stream_name: heapless::String<64>,
    pub bitmap: Bitmap,
}

impl FileContext {
    pub fn new_from(
        job_name: &str,
        ota_job: &OtaJob,
        status_details: Option<StatusDetails>,
        file_idx: usize,
        config: &Config,
        current_version: Version,
    ) -> Result<Self, OtaError> {
        let file_desc = ota_job
            .files
            .get(file_idx)
            .ok_or(OtaError::InvalidFile)?
            .clone();

        // Initialize new `status_details' if not already present
        let status = if let Some(details) = status_details {
            details
        } else {
            let mut status = StatusDetails::new();
            status
                .insert(
                    heapless::String::from("updated_by"),
                    current_version.to_string(),
                )
                .map_err(|_| OtaError::Overflow)?;
            status
        };

        let signature = file_desc.signature();

        let block_offset = 0;
        let bitmap = Bitmap::new(file_desc.filesize, config.block_size, block_offset);

        Ok(FileContext {
            filepath: heapless::String::from(file_desc.filepath),
            filesize: file_desc.filesize,
            fileid: file_desc.fileid,
            certfile: heapless::String::from(file_desc.certfile),
            update_data_url: file_desc.update_data_url.map(heapless::String::from),
            auth_scheme: file_desc.auth_scheme.map(heapless::String::from),
            signature,
            file_type: file_desc.file_type,

            status_details: status,

            job_name: heapless::String::from(job_name),
            block_offset,
            request_block_remaining: bitmap.len() as u32,
            blocks_remaining: (file_desc.filesize + config.block_size - 1) / config.block_size,
            stream_name: heapless::String::from(ota_job.streamname),
            bitmap,
        })
    }

    pub fn self_test(&self) -> bool {
        self.status_details
            .get(&heapless::String::from("self_test"))
            .and_then(|f| f.parse().ok())
            .map(|reason: JobStatusReason| {
                reason == JobStatusReason::SigCheckPassed
                    || reason == JobStatusReason::SelfTestActive
            })
            .unwrap_or(false)
    }

    pub fn updated_by(&self) -> Option<Version> {
        self.status_details
            .get(&heapless::String::from("updated_by"))
            .and_then(|s| Version::from_str(s.as_str()).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_masking() {
        let bitmap = Bitmap::new(255000, 256, 0);

        let true_indices: Vec<usize> = bitmap.into_iter().collect();
        assert_eq!((0..31).into_iter().collect::<Vec<usize>>(), true_indices);
    }
}
