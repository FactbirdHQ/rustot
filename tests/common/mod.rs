#[allow(dead_code)]
pub mod aws_ota;
pub mod credentials;
pub mod file_handler;
pub mod network;

#[allow(dead_code)]
pub fn handle_ota(
    message: impl rustot::mqtt::MqttMessage,
    config: &rustot::ota::config::Config,
) -> Option<rustot::ota::encoding::FileContext> {
    use rustot::{
        jobs::{
            self,
            data_types::{DescribeJobExecutionResponse, NextJobExecutionChanged},
        },
        ota::{
            encoding::{json::OtaJob, FileContext},
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

    let topic = jobs::Topic::from_str(message.topic_name());
    log::debug!(
        "handle_ota: topic={:?} payload_len={}",
        message.topic_name(),
        message.payload().len()
    );

    // Use serde_json (std) instead of serde_json_core for test deserialization.
    // serde_json_core has a fixed-size scratch buffer that overflows on the
    // long pre-signed S3 URLs in HTTP OTA job documents (~1000+ chars with
    // JSON-escaped forward slashes).
    let job = match topic {
        Some(jobs::Topic::NotifyNext) => {
            match serde_json::from_slice::<NextJobExecutionChanged<Jobs>>(message.payload()) {
                Ok(execution_changed) => execution_changed.execution?,
                Err(e) => {
                    log::error!("handle_ota: failed to deserialize NotifyNext: {:?}", e);
                    return None;
                }
            }
        }
        Some(jobs::Topic::DescribeAccepted(_)) => {
            match serde_json::from_slice::<DescribeJobExecutionResponse<Jobs>>(message.payload()) {
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
            log::warn!("handle_ota: unexpected topic: {:?}", message.topic_name());
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

    match FileContext::new_from(
        JobEventData {
            job_name: job.job_id,
            ota_document: ota_job,
            status_details: job.status_details,
        },
        0,
        config,
    ) {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            log::error!("handle_ota: FileContext::new_from failed: {:?}", e);
            None
        }
    }
}
