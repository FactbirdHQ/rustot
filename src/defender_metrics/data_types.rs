use core::str::FromStr;

use heapless::{String, Vec};
use serde::Serialize;

// Constants for heapless container sizes
pub const HEADER_VERSION_SIZE: usize = 6;
pub const REMOTE_ADDR_SIZE: usize = 64;
pub const LOCAL_INTERFACE_SIZE: usize = 32;
pub const MAX_METRICS: usize = 8;
pub const MAX_CUSTOM_METRICS: usize = 16;
pub const MAX_CUSTOM_METRICS_NAME: usize = 32;

pub enum MetricError {
    Malformed,
    Throttled,
    MissingHeader,
    Other,
}

#[derive(Debug, Serialize)]
pub struct Metric<C: Serialize> {
    #[serde(rename = "hed")]
    pub header: Header,

    #[serde(rename = "cmet")]
    pub custom_metrics: C,
}

impl<C: Serialize> Metric<C> {
    pub fn new(custom_metrics: C, timestamp: i64) -> Self {
        let header = Header {
            report_id: timestamp,
            version: String::<HEADER_VERSION_SIZE>::from_str("1.0").unwrap(), //FIXME: Don't
        };

        Self {
            header,
            custom_metrics,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Header {
    /// Monotonically increasing value. Epoch timestamp recommended.
    #[serde(rename = "rid")]
    pub report_id: i64,

    /// Version in Major.Minor format.
    #[serde(rename = "v")]
    pub version: String<HEADER_VERSION_SIZE>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomMetric<'a> {
    Number(i64),
    NumberList(&'a [u64]),
    StringList(&'a [&'a str]),
    IpList(&'a [&'a str]),
}
