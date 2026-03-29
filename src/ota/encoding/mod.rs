#[cfg(feature = "ota_mqtt_data")]
pub mod cbor;
pub mod json;

use core::ops::{Deref, DerefMut};
use serde::{Serialize, Serializer};

use self::json::{JobStatusReason, Signature};

use super::data_interface::Protocol;
use super::error::OtaError;
use super::status_details::{OtaStatusDetails, StatusDetailsExt};
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

/// Context for an active OTA job. Borrows all string data from the caller's
/// payload buffer — zero string copies, no `heapless::String` sizing decisions.
///
/// `E` is the user's [`StatusDetailsExt`] type, the same type used by the PAL
/// for outgoing status serialization. During construction, incoming status
/// details are routed: known OTA keys → [`OtaStatusDetails`], unknown keys →
/// `E::accept_entry()`.
#[derive(Clone)]
pub struct OtaJobContext<'a, E: StatusDetailsExt = ()> {
    pub job_name: &'a str,
    pub filepath: &'a str,
    pub filesize: usize,
    pub fileid: u8,
    pub certfile: Option<&'a str>,
    pub update_data_url: Option<&'a str>,
    pub auth_scheme: Option<&'a str>,
    pub signature: Option<Signature<'a>>,
    pub file_type: Option<u32>,
    pub protocols: heapless::Vec<Protocol, 2>,
    pub stream_name: Option<&'a str>,
    pub status: OtaStatusDetails,
    pub extra_status: E,
}

impl<'a, E: StatusDetailsExt> OtaJobContext<'a, E> {
    pub fn new_from(job_data: JobEventData<'a>, file_idx: usize) -> Result<Self, OtaError> {
        let file_desc = job_data
            .ota_document
            .files
            .get(file_idx)
            .ok_or(OtaError::InvalidFile)?;

        if file_desc.filesize == 0 {
            return Err(OtaError::ZeroFileSize);
        }

        let mut extra_status = E::default();
        let mut status = OtaStatusDetails::new();
        if let Some(ref details) = job_data.status_details {
            for (&key, &value) in details.iter() {
                match key {
                    "self_test" => status.set_self_test(value),
                    // progress, reason, error_code are recomputed during OTA
                    "progress" | "reason" | "error_code" => {}
                    _ => {
                        extra_status.accept_entry(key, value);
                    }
                }
            }
        }

        Ok(OtaJobContext {
            job_name: job_data.job_name,
            filepath: file_desc.filepath,
            filesize: file_desc.filesize,
            fileid: file_desc.fileid,
            certfile: file_desc.certfile,
            update_data_url: file_desc.update_data_url,
            auth_scheme: file_desc.auth_scheme,
            signature: file_desc.signature(),
            file_type: file_desc.file_type,
            protocols: job_data.ota_document.protocols,
            stream_name: job_data.ota_document.streamname,
            status,
            extra_status,
        })
    }

    pub fn self_test(&self) -> bool {
        self.status
            .self_test
            .as_ref()
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
