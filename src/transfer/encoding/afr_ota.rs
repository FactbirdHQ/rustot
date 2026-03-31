//! AWS FreeRTOS OTA job document template (`afr_ota`).
//!
//! This module provides the [`OtaJob`] type for deserializing OTA job
//! documents in the AWS FreeRTOS format. It implements [`JobDocument`]
//! so it can be used directly with [`JobContext::new`].

use serde::Deserialize;

use super::json::Signature;
use super::{FileInfo, JobDocument};
use crate::transfer::data_interface::Protocol;

/// OTA job document, compatible with the FreeRTOS OTA process.
///
/// Deserialized from the `afr_ota` key in an AWS IoT Job document.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename = "afr_ota")]
pub struct OtaJob<'a> {
    pub protocols: heapless::Vec<Protocol, 2>,
    #[serde(default)]
    pub streamname: Option<&'a str>,
    pub files: heapless::Vec<FileDescription<'a>, 1>,
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

impl<'a> JobDocument<'a> for OtaJob<'a> {
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
}
