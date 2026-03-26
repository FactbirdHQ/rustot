#[allow(dead_code)]
pub mod aws_ota;
pub mod credentials;
pub mod file_handler;
pub mod network;

use rustot::ota::StatusDetailsExt;

#[allow(dead_code)]
pub fn handle_ota<'a, E: StatusDetailsExt>(
    topic: &str,
    payload: &'a [u8],
    extra_status: E,
) -> Option<rustot::ota::encoding::OtaJobContext<'a, E>> {
    use rustot::{
        jobs::{
            self,
            data_types::{DescribeJobExecutionResponse, NextJobExecutionChanged},
        },
        ota::{
            encoding::{json::OtaJob, OtaJobContext},
            JobEventData,
        },
    };
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    pub enum Jobs<'b> {
        #[serde(rename = "afr_ota")]
        #[serde(borrow)]
        Ota(OtaJob<'b>),
    }

    impl<'b> Jobs<'b> {
        pub fn ota_job(self) -> Option<OtaJob<'b>> {
            match self {
                Jobs::Ota(ota_job) => Some(ota_job),
            }
        }
    }

    let parsed_topic = jobs::Topic::from_str(topic);
    log::debug!(
        "handle_ota: topic={:?} payload_len={}",
        topic,
        payload.len()
    );

    // Use serde_json (std) instead of serde_json_core for test deserialization.
    // serde_json_core has a fixed-size scratch buffer that overflows on the
    // long pre-signed S3 URLs in HTTP OTA job documents.
    let job = match parsed_topic {
        Some(jobs::Topic::NotifyNext) => {
            match serde_json::from_slice::<NextJobExecutionChanged<Jobs>>(payload) {
                Ok(execution_changed) => execution_changed.execution?,
                Err(e) => {
                    log::error!("handle_ota: failed to deserialize NotifyNext: {:?}", e);
                    return None;
                }
            }
        }
        Some(jobs::Topic::DescribeAccepted(_)) => {
            match serde_json::from_slice::<DescribeJobExecutionResponse<Jobs>>(payload) {
                Ok(execution_changed) => {
                    if execution_changed.execution.is_none() {
                        if std::env::var("CI").is_ok() {
                            panic!("No OTA jobs queued?");
                        }
                        return None;
                    }
                    execution_changed.execution?
                }
                Err(e) => {
                    log::error!(
                        "handle_ota: failed to deserialize DescribeAccepted: {:?}",
                        e
                    );
                    return None;
                }
            }
        }
        _ => {
            log::warn!("handle_ota: unexpected topic: {:?}", topic);
            return None;
        }
    };

    let ota_job = match job.job_document {
        Some(doc) => match doc.ota_job() {
            Some(job) => job,
            None => {
                log::error!("handle_ota: job_document present but not an OTA job");
                return None;
            }
        },
        None => {
            log::error!("handle_ota: no job_document in execution");
            return None;
        }
    };

    match OtaJobContext::new_from(
        JobEventData {
            job_name: job.job_id,
            ota_document: ota_job,
            status_details: job.status_details,
        },
        0,
        extra_status,
    ) {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            log::error!("handle_ota: OtaJobContext::new_from failed: {:?}", e);
            None
        }
    }
}
