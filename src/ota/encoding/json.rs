use crate::ota::data_interface::Protocol;
use core::str::FromStr;
use serde::Deserialize;

/// OTA job document, compatible with FreeRTOS OTA process
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename = "afr_ota")]
pub struct OtaJob<'a> {
    pub protocols: heapless::Vec<Protocol, 2>,
    pub streamname: &'a str,
    pub files: heapless::Vec<FileDescription<'a>, 1>,
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
pub struct FileDescription<'a> {
    #[serde(rename = "filepath")]
    pub filepath: &'a str,
    #[serde(rename = "filesize")]
    pub filesize: usize,
    #[serde(rename = "fileid")]
    pub fileid: u8,
    #[serde(rename = "certfile")]
    pub certfile: &'a str,
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
    pub fn signature(&self) -> Signature {
        if let Some(sig) = self.sha1_rsa {
            return Signature::Sha1Rsa(heapless::String::try_from(sig).unwrap());
        }
        if let Some(sig) = self.sha256_rsa {
            return Signature::Sha256Rsa(heapless::String::try_from(sig).unwrap());
        }
        if let Some(sig) = self.sha1_ecdsa {
            return Signature::Sha1Ecdsa(heapless::String::try_from(sig).unwrap());
        }
        if let Some(sig) = self.sha256_ecdsa {
            return Signature::Sha256Ecdsa(heapless::String::try_from(sig).unwrap());
        }
        unreachable!()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JobStatusReason {
    Receiving,      /* Update progress status. */
    SigCheckPassed, /* Set status details to Self Test Ready. */
    SelfTestActive, /* Set status details to Self Test Active. */
    Accepted,       /* Set job state to Succeeded. */
    Rejected,       /* Set job state to Failed. */
    Aborted,        /* Set job state to Failed. */
    Pal(u32),
}

impl JobStatusReason {
    pub fn as_str(&self) -> &str {
        match self {
            JobStatusReason::Receiving => "receiving",
            JobStatusReason::SigCheckPassed => "ready",
            JobStatusReason::SelfTestActive => "active",
            JobStatusReason::Accepted => "accepted",
            JobStatusReason::Rejected => "rejected",
            JobStatusReason::Aborted => "aborted",
            JobStatusReason::Pal(_) => "pal err",
        }
    }
}

impl FromStr for JobStatusReason {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "receiving" => JobStatusReason::Receiving,
            "ready" => JobStatusReason::SigCheckPassed,
            "active" => JobStatusReason::SelfTestActive,
            "accepted" => JobStatusReason::Accepted,
            "rejected" => JobStatusReason::Rejected,
            "aborted" => JobStatusReason::Aborted,
            _ => return Err(()),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::jobs::StatusDetails;

    use super::*;

    #[test]
    fn job_status_reason_serialize() {
        let reasons = &[
            (JobStatusReason::Receiving, "receiving"),
            (JobStatusReason::SigCheckPassed, "ready"),
            (JobStatusReason::SelfTestActive, "active"),
            (JobStatusReason::Accepted, "accepted"),
            (JobStatusReason::Rejected, "rejected"),
            (JobStatusReason::Aborted, "aborted"),
        ];

        for (reason, exp) in reasons {
            let mut status_details = StatusDetails::new();

            status_details.insert("self_test", reason.as_str()).unwrap();

            assert_eq!(
                serde_json_core::to_string::<_, 128>(&status_details)
                    .unwrap()
                    .as_str(),
                format!("{{\"self_test\":\"{}\"}}", exp).as_str()
            );
        }
    }
}
