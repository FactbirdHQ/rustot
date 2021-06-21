use heapless::Vec;
use serde::{Serialize, Deserialize};

use crate::ota::data_interface::Protocol;

/// OTA job document, compatible with FreeRTOS OTA process
#[derive(Debug, PartialEq, Deserialize)]
pub struct OtaJob {
    pub protocols: Vec<Protocol, 2>,
    pub streamname: heapless::String<64>,
    pub files: Vec<FileDescription, 1>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
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
    pub sig_sha1_rsa: heapless::String<64>,
    #[serde(rename = "attr")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_attributes: Option<u32>,
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
