#[cfg(feature = "ota_mqtt_data")]
pub mod cbor;
pub mod json;

use core::ops::{Deref, DerefMut};
use serde::{Serialize, Serializer};

use crate::jobs::StatusDetailsOwned;

use self::json::{JobStatusReason, Signature};

use super::config::Config;
use super::data_interface::Protocol;
use super::error::OtaError;
use super::JobEventData;

#[derive(Clone, Debug, PartialEq)]
pub struct Bitmap(bitmaps::Bitmap<32>);

impl Bitmap {
    pub fn new(file_size: usize, block_size: usize, block_offset: u32) -> Self {
        // Total number of blocks in file, rounded up
        let total_num_blocks = file_size.div_ceil(block_size);

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
    pub certfile: Option<heapless::String<64>>,
    pub update_data_url: Option<heapless::String<64>>,
    pub auth_scheme: Option<heapless::String<64>>,
    pub signature: Option<Signature>,
    pub file_type: Option<u32>,
    pub protocols: heapless::Vec<Protocol, 2>,

    pub status_details: StatusDetailsOwned,
    pub block_offset: u32,
    pub blocks_remaining: usize,
    pub request_block_remaining: u32,
    pub job_name: heapless::String<64>,
    pub stream_name: heapless::String<64>,
    pub bitmap: Bitmap,
}

impl FileContext {
    pub fn new_from(
        job_data: JobEventData<'_>,
        file_idx: usize,
        config: &Config,
    ) -> Result<Self, OtaError> {
        if job_data
            .ota_document
            .files
            .get(file_idx)
            .map(|f| f.filesize)
            .unwrap_or_default()
            == 0
        {
            return Err(OtaError::ZeroFileSize);
        }

        let file_desc = job_data
            .ota_document
            .files
            .get(file_idx)
            .ok_or(OtaError::InvalidFile)?
            .clone();

        let signature = file_desc.signature();

        let block_offset = 0;
        let bitmap = Bitmap::new(file_desc.filesize, config.block_size, block_offset);

        Ok(FileContext {
            filepath: heapless::String::try_from(file_desc.filepath).unwrap(),
            filesize: file_desc.filesize,
            protocols: job_data.ota_document.protocols,
            fileid: file_desc.fileid,
            certfile: file_desc
                .certfile
                .map(|cert| heapless::String::try_from(cert).unwrap()),
            update_data_url: file_desc
                .update_data_url
                .map(|s| heapless::String::try_from(s).unwrap()),
            auth_scheme: file_desc
                .auth_scheme
                .map(|s| heapless::String::try_from(s).unwrap()),
            signature,
            file_type: file_desc.file_type,

            status_details: job_data
                .status_details
                .map(|s| {
                    s.iter()
                        .map(|(&k, &v)| {
                            (
                                heapless::String::try_from(k).unwrap(),
                                heapless::String::try_from(v).unwrap(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),

            job_name: heapless::String::try_from(job_data.job_name).unwrap(),
            block_offset,
            request_block_remaining: (bitmap.len() as u32).min(config.max_blocks_per_request),
            blocks_remaining: file_desc.filesize.div_ceil(config.block_size),
            stream_name: heapless::String::try_from(job_data.ota_document.streamname).unwrap(),
            bitmap,
        })
    }

    pub fn self_test(&self) -> bool {
        self.status_details
            .get(&heapless::String::try_from("self_test").unwrap())
            .and_then(|f| f.parse().ok())
            .map(|reason: JobStatusReason| {
                reason == JobStatusReason::SigCheckPassed
                    || reason == JobStatusReason::SelfTestActive
            })
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_masking() {
        let bitmap = Bitmap::new(255000, 256, 0);

        let true_indices: Vec<usize> = bitmap.into_iter().collect();
        assert_eq!((0..31).collect::<Vec<usize>>(), true_indices);
    }
}
