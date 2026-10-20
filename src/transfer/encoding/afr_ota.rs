//! AWS FreeRTOS OTA job document template (`afr_ota`).
//!
//! This module provides the [`OtaJob`] type for deserializing OTA job
//! documents in the AWS FreeRTOS format. It implements [`JobDocument`]
//! so it can be used directly with [`JobContext::new`](super::JobContext::new).

use serde::Deserialize;

use super::json::Signature;
use super::{FileInfo, JobDocument};
use crate::transfer::data_interface::Protocol;

/// OTA job document, compatible with the FreeRTOS OTA process.
///
/// Deserialized from the `afr_ota` key in an AWS IoT Job document.
///
/// `N` is the maximum number of files accepted. Documents with more files
/// fail to deserialize.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename = "afr_ota")]
pub struct OtaJob<'a, const N: usize = 1> {
    pub protocols: heapless::Vec<Protocol, 2>,
    #[serde(default)]
    pub streamname: Option<&'a str>,
    #[serde(borrow)]
    pub files: heapless::Vec<FileDescription<'a>, N>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct FileDescription<'a> {
    #[serde(rename = "filepath")]
    pub filepath: &'a str,
    #[serde(rename = "filesize")]
    pub filesize: usize,
    #[serde(rename = "fileid")]
    pub fileid: u8,
    #[serde(rename = "certfile")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certfile: Option<&'a str>,
    #[serde(rename = "update_data_url")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_data_url: Option<&'a str>,
    #[serde(rename = "auth_scheme")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<&'a str>,

    #[serde(rename = "sig-sha1-rsa")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1_rsa: Option<&'a str>,
    #[serde(rename = "sig-sha256-rsa")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256_rsa: Option<&'a str>,
    #[serde(rename = "sig-sha1-ecdsa")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1_ecdsa: Option<&'a str>,
    #[serde(rename = "sig-sha256-ecdsa")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256_ecdsa: Option<&'a str>,

    #[serde(rename = "fileType")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_type: Option<u32>,
}

impl<'a> FileDescription<'a> {
    pub fn signature(&self) -> Option<Signature<'a>> {
        if let Some(sig) = self.sha1_rsa {
            return Some(Signature::Sha1Rsa(sig));
        }
        if let Some(sig) = self.sha256_rsa {
            return Some(Signature::Sha256Rsa(sig));
        }
        if let Some(sig) = self.sha1_ecdsa {
            return Some(Signature::Sha1Ecdsa(sig));
        }
        if let Some(sig) = self.sha256_ecdsa {
            return Some(Signature::Sha256Ecdsa(sig));
        }
        None
    }
}

impl<'a, const N: usize> JobDocument<'a> for OtaJob<'a, N> {
    fn protocols(&self) -> &[Protocol] {
        &self.protocols
    }

    fn stream_name(&self) -> Option<&'a str> {
        self.streamname
    }

    fn file(&self, idx: usize) -> Option<FileInfo<'a>> {
        let f = self.files.get(idx)?;
        Some(FileInfo {
            filepath: f.filepath,
            filesize: f.filesize,
            fileid: f.fileid,
            certfile: f.certfile,
            update_data_url: f.update_data_url,
            auth_scheme: f.auth_scheme,
            signature: f.signature(),
            file_type: f.file_type,
        })
    }

    fn file_count(&self) -> usize {
        self.files.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_ota_job_document() {
        let data = r#"{
            "protocols": [
            "MQTT"
            ],
            "streamname": "AFR_OTA-d11032e9-38d5-4dca-8c7c-1e6f24533ede",
            "files": [
            {
                "filepath": "3.8.4",
                "filesize": 537600,
                "fileid": 0,
                "certfile": null,
                "fileType": 0,
                "update_data_url": null,
                "auth_scheme": null,
                "sig--": null
            }
            ]
        }"#;

        serde_json_core::from_str::<OtaJob>(data).unwrap();
    }

    const TWO_FILES: &str = r#"{
        "protocols": ["MQTT", "HTTP"],
        "streamname": "AFR_OTA-stream",
        "files": [
            {
                "filepath": "model.onnx",
                "filesize": 1024,
                "fileid": 0,
                "fileType": 7,
                "sig-sha256-ecdsa": "c2ln"
            },
            {
                "filepath": "labels.json",
                "filesize": 12,
                "fileid": 1,
                "fileType": 8
            }
        ]
    }"#;

    #[test]
    fn deserialize_multi_file_document() {
        let (job, _) = serde_json_core::from_str::<OtaJob<2>>(TWO_FILES).unwrap();

        assert_eq!(job.file_count(), 2);
        assert_eq!(job.file(0).unwrap().filepath, "model.onnx");
        assert_eq!(
            job.file(0).unwrap().signature,
            Some(Signature::Sha256Ecdsa("c2ln"))
        );
        assert_eq!(job.file(1).unwrap().filepath, "labels.json");
        assert_eq!(job.file(1).unwrap().file_type, Some(8));
        assert!(job.file(2).is_none());
    }

    #[test]
    fn too_many_files_fail_to_deserialize() {
        assert!(serde_json_core::from_str::<OtaJob>(TWO_FILES).is_err());
    }

    #[test]
    fn job_context_takes_every_file() {
        use crate::transfer::encoding::{FileType, JobContext};
        use crate::transfer::error::TransferError;

        let (doc, _) = serde_json_core::from_str::<OtaJob<2>>(TWO_FILES).unwrap();

        let ctx = JobContext::<(), 2>::new(&doc, "job", None).unwrap();
        assert_eq!(ctx.files.len(), 2);
        assert_eq!(ctx.files[0].file_type, Some(FileType::Other(7)));
        assert_eq!(ctx.files[1].fileid, 1);
        assert_eq!(ctx.total_size(), 1036);

        assert!(matches!(
            JobContext::<(), 1>::new(&doc, "job", None),
            Err(TransferError::Overflow)
        ));
    }
}
