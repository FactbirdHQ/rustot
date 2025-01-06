use core::fmt::Display;

use embassy_time::Instant;
use serde::Serialize;

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
    pub fn new(custom_metrics: C) -> Self {
        let header = Header {
            report_id: Instant::now().as_millis() as i64,
            version: Version(1, 0),
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
    pub version: Version,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomMetric<'a> {
    Number(i64),
    NumberList(&'a [u64]),
    StringList(&'a [&'a str]),
    IpList(&'a [&'a str]),
}

/// Format is `Version(Majo, Minor)`
#[derive(Debug)]
pub struct Version(u8, u8);

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(&self)
    }
}

impl Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}", self.0, self.1,)
    }
}
