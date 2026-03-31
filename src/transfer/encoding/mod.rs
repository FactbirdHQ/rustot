pub mod afr_ota;
#[cfg(feature = "transfer_mqtt")]
pub mod cbor;
pub mod json;

use core::ops::{Deref, DerefMut};
use serde::{Serialize, Serializer};

use self::json::{JobStatusReason, Signature};

use super::data_interface::Protocol;
use super::error::TransferError;
use super::status_details::{StatusDetails, StatusDetailsExt};

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

/// File-level metadata extracted from a job document.
///
/// Returned by [`JobDocument::file`]. Contains the raw values from the
/// wire format — conversion to typed fields (e.g. [`FileType`]) happens
/// in [`JobContext::new`].
pub struct FileInfo<'a> {
    pub filepath: &'a str,
    pub filesize: usize,
    pub fileid: u8,
    pub certfile: Option<&'a str>,
    pub update_data_url: Option<&'a str>,
    pub auth_scheme: Option<&'a str>,
    pub signature: Option<Signature<'a>>,
    pub file_type: Option<u32>,
}

/// The type of file being transferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum FileType {
    /// Firmware update (`file_type == 0` in AWS IoT).
    /// Triggers self-test, image activation, and device reset via [`OtaPal`](super::pal::OtaPal).
    Firmware,
    /// Non-firmware file.
    Other(u32),
}

/// Trait for job document formats that describe downloadable files.
///
/// Implement this for custom job document types to enable constructing
/// a [`JobContext`] via [`JobContext::new`]. The library provides an
/// implementation for [`OtaJob`](afr_ota::OtaJob) (the `afr_ota` format).
pub trait JobDocument<'a> {
    /// Protocols supported by this job (MQTT, HTTP, or both).
    fn protocols(&self) -> &[Protocol];
    /// MQTT stream name, if applicable.
    fn stream_name(&self) -> Option<&'a str>;
    /// File metadata for the given index. Returns `None` if out of bounds.
    fn file(&self, idx: usize) -> Option<FileInfo<'a>>;
}

/// Context for an active transfer job. Borrows all string data from the
/// caller's payload buffer — zero string copies.
///
/// Constructed from any [`JobDocument`] via [`JobContext::new`].
///
/// `E` is the user's [`StatusDetailsExt`] type, the same type used by the PAL
/// for outgoing status serialization. During construction, incoming status
/// details are routed: known keys → [`StatusDetails`], unknown keys →
/// `E::accept_entry()`.
#[derive(Clone)]
pub struct JobContext<'a, E: StatusDetailsExt = ()> {
    pub job_name: &'a str,
    pub filepath: &'a str,
    pub filesize: usize,
    pub fileid: u8,
    pub certfile: Option<&'a str>,
    pub update_data_url: Option<&'a str>,
    pub auth_scheme: Option<&'a str>,
    pub signature: Option<Signature<'a>>,
    pub file_type: Option<FileType>,
    pub protocols: heapless::Vec<Protocol, 2>,
    pub stream_name: Option<&'a str>,
    pub status: StatusDetails,
    pub extra_status: E,
}

impl<'a, E: StatusDetailsExt> JobContext<'a, E> {
    /// Construct a [`JobContext`] from any [`JobDocument`].
    pub fn new<D: JobDocument<'a>>(
        doc: &D,
        job_name: &'a str,
        file_idx: usize,
        status_details: Option<&crate::jobs::StatusDetails<'a>>,
    ) -> Result<Self, TransferError> {
        let file = doc.file(file_idx).ok_or(TransferError::InvalidFile)?;

        if file.filesize == 0 {
            return Err(TransferError::ZeroFileSize);
        }

        let mut extra_status = E::default();
        let mut status = StatusDetails::new();
        if let Some(details) = status_details {
            for (&key, &value) in details.iter() {
                match key {
                    "self_test" => status.set_self_test(value),
                    // progress, reason, error_code are recomputed during transfer
                    "progress" | "reason" | "error_code" => {}
                    _ => {
                        extra_status.accept_entry(key, value);
                    }
                }
            }
        }

        Ok(JobContext {
            job_name,
            filepath: file.filepath,
            filesize: file.filesize,
            fileid: file.fileid,
            certfile: file.certfile,
            update_data_url: file.update_data_url,
            auth_scheme: file.auth_scheme,
            signature: file.signature,
            file_type: file.file_type.map(|t| match t {
                0 => FileType::Firmware,
                n => FileType::Other(n),
            }),
            protocols: heapless::Vec::from_slice(doc.protocols())
                .map_err(|_| TransferError::Overflow)?,
            stream_name: doc.stream_name(),
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
