use core::str::FromStr;

use heapless::{LinearMap, String, Vec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct TcpConnections {
    #[serde(rename = "ec")]
    pub established_connections: Option<EstablishedConnections>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EstablishedConnections {
    #[serde(rename = "cs")]
    pub connections: Option<Vec<Connection, { MAX_CONNECTIONS }>>,

    #[serde(rename = "t")]
    pub total: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Connection {
    #[serde(rename = "rad")]
    pub remote_addr: String<REMOTE_ADDR_SIZE>,

    /// Port number, must be >= 0
    #[serde(rename = "lp")]
    pub local_port: Option<u16>,

    /// Interface name
    #[serde(rename = "li")]
    pub local_interface: Option<String<LOCAL_INTERFACE_SIZE>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListeningTcpPorts {
    #[serde(rename = "pts")]
    pub ports: Option<Vec<TcpPort, MAX_PORTS>>,

    #[serde(rename = "t")]
    pub total: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TcpPort {
    #[serde(rename = "pt")]
    pub port: u16,

    #[serde(rename = "if")]
    pub interface: Option<String<LOCAL_INTERFACE_SIZE>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListeningUdpPorts {
    #[serde(rename = "pts")]
    pub ports: Option<Vec<UdpPort, MAX_PORTS>>,

    #[serde(rename = "t")]
    pub total: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UdpPort {
    #[serde(rename = "pt")]
    pub port: u16,

    #[serde(rename = "if")]
    pub interface: Option<String<LOCAL_INTERFACE_SIZE>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NetworkStats {
    #[serde(rename = "bi")]
    pub bytes_in: Option<u64>,

    #[serde(rename = "bo")]
    pub bytes_out: Option<u64>,

    #[serde(rename = "pi")]
    pub packets_in: Option<u64>,

    #[serde(rename = "po")]
    pub packets_out: Option<u64>,
}
