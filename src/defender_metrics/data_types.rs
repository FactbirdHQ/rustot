use core::fmt::{Display, Write};

use bon::Builder;
use embassy_time::Instant;
use serde::{ser::SerializeStruct, Deserialize, Serialize};

use super::aws_types::{ListeningTcpPorts, ListeningUdpPorts, NetworkStats, TcpConnections};

#[derive(Debug, Serialize, Builder)]
pub struct Metric<'a, C: Serialize> {
    #[serde(rename = "hed")]
    pub header: Header,

    #[serde(rename = "met")]
    pub metrics: Option<Metrics<'a>>,

    #[serde(rename = "cmet")]
    pub custom_metrics: Option<C>,
}

#[derive(Debug, Serialize)]
pub struct Metrics<'a> {
    listening_tcp_ports: Option<ListeningTcpPorts<'a>>,
    listening_udp_ports: Option<ListeningUdpPorts<'a>>,
    network_stats: Option<NetworkStats>,
    tcp_connections: Option<TcpConnections<'a>>,
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

impl Default for Header {
    fn default() -> Self {
        Self {
            report_id: Instant::now().as_millis() as i64,
            version: Default::default(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomMetric<'a> {
    Number(i64),
    NumberList(&'a [u64]),
    StringList(&'a [&'a str]),
    IpList(&'a [&'a str]),
}

/// Format is `Version(Major, Minor)`
#[derive(Debug, PartialEq, Deserialize)]
pub struct Version(pub u8, pub u8);

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut st: heapless::String<7> = heapless::String::new();
        st.write_fmt(format_args!("{}.{}", self.0, self.1)).ok();

        serializer.serialize_str(&st)
    }
}

impl Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}", self.0, self.1,)
    }
}

impl Default for Version {
    fn default() -> Self {
        Self(1, 0)
    }
}
