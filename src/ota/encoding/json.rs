use serde::{Deserialize, Serialize};

use crate::ota::data_interface::Protocol;

/// OTA job document, compatible with FreeRTOS OTA process
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename = "afr_ota")]
pub struct OtaJob {
    pub protocols: heapless::Vec<Protocol, 2>,
    pub streamname: heapless::String<64>,
    pub files: heapless::Vec<FileDescription, 1>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum Signature {
    #[serde(rename = "sig-sha1-rsa")]
    Sha1Rsa(heapless::String<64>),
    #[serde(rename = "sig-sha256-rsa")]
    Sha256Rsa(heapless::String<64>),
    #[serde(rename = "sig-sha1-ecdsa")]
    Sha1Ecdsa(heapless::String<64>),
    #[serde(rename = "sig-sha256-ecdsa")]
    Sha256Ecdsa(heapless::String<64>),
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct FileDescription {
    #[serde(rename = "filepath")]
    pub filepath: heapless::String<64>,
    #[serde(rename = "filesize")]
    pub filesize: usize,
    #[serde(rename = "fileid")]
    pub fileid: u8,
    #[serde(rename = "certfile")]
    pub certfile: heapless::String<64>,
    #[serde(rename = "update_data_url")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_data_url: Option<heapless::String<64>>,
    #[serde(rename = "auth_scheme")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<heapless::String<64>>,

    #[serde(rename = "sig-sha1-rsa")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1_rsa: Option<heapless::String<64>>,
    #[serde(rename = "sig-sha256-rsa")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256_rsa: Option<heapless::String<64>>,
    #[serde(rename = "sig-sha1-ecdsa")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1_ecdsa: Option<heapless::String<64>>,
    #[serde(rename = "sig-sha256-ecdsa")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256_ecdsa: Option<heapless::String<64>>,

    #[serde(rename = "attr")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_attributes: Option<u32>,
}

impl FileDescription {
    pub fn signature(&self) -> Signature {
        if let Some(ref sig) = self.sha1_rsa {
            return Signature::Sha1Rsa(sig.clone());
        }
        if let Some(ref sig) = self.sha256_rsa {
            return Signature::Sha256Rsa(sig.clone());
        }
        if let Some(ref sig) = self.sha1_ecdsa {
            return Signature::Sha1Ecdsa(sig.clone());
        }
        if let Some(ref sig) = self.sha256_ecdsa {
            return Signature::Sha256Ecdsa(sig.clone());
        }
        unreachable!()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum JobStatusReason {
    #[serde(rename = "")]
    Receiving, /* Update progress status. */
    #[serde(rename = "ready")]
    SigCheckPassed, /* Set status details to Self Test Ready. */
    #[serde(rename = "active")]
    SelfTestActive, /* Set status details to Self Test Active. */
    #[serde(rename = "accepted")]
    Accepted, /* Set job state to Succeeded. */
    #[serde(rename = "rejected")]
    Rejected, /* Set job state to Failed. */
    #[serde(rename = "aborted")]
    Aborted, /* Set job state to Failed. */
    Pal(u32),
}
