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
pub enum CustomMetric {
    Number(i64),
    NumberList(Vec<u64, MAX_METRICS>),
    StringList(Vec<String<LOCAL_INTERFACE_SIZE>, MAX_METRICS>),
    IpList(Vec<String<REMOTE_ADDR_SIZE>, MAX_METRICS>),
}

impl CustomMetric {
    pub fn new_number(value: i64) -> heapless::Vec<Self, 1> {
        let mut custom_metric_map = Vec::new();

        custom_metric_map.push(CustomMetric::Number(value)).unwrap();

        custom_metric_map
    }

    pub fn new_number_list(values: &[u64]) -> heapless::Vec<Self, 1> {
        let mut custom_metric_map = Vec::new();

        let mut vec = Vec::new();
        for &v in values {
            vec.push(v).unwrap();
        }

        custom_metric_map
            .push(CustomMetric::NumberList(vec))
            .unwrap();

        custom_metric_map
    }

    pub fn new_string_list(values: &[&str]) -> heapless::Vec<Self, 1> {
        let mut custom_metric_map = Vec::new();

        let mut vec = Vec::new();
        for &v in values {
            vec.push(String::from_str(v).unwrap()).unwrap();
        }
        custom_metric_map
            .push(CustomMetric::StringList(vec))
            .unwrap();

        custom_metric_map
    }

    pub fn new_ip_list(values: &[&str]) -> heapless::Vec<Self, 1> {
        let mut custom_metric_map = Vec::new();

        let mut vec = Vec::new();
        for &v in values {
            vec.push(String::from_str(v).unwrap()).unwrap();
        }
        custom_metric_map.push(CustomMetric::IpList(vec)).unwrap();

        custom_metric_map
    }
}
