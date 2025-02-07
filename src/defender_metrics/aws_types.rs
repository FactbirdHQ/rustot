use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct TcpConnections<'a> {
    #[serde(rename = "ec")]
    pub established_connections: Option<&'a EstablishedConnections<'a>>,
}

#[derive(Debug, Serialize)]
pub struct EstablishedConnections<'a> {
    #[serde(rename = "cs")]
    pub connections: Option<&'a [&'a Connection<'a>]>,

    #[serde(rename = "t")]
    pub total: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct Connection<'a> {
    #[serde(rename = "rad")]
    pub remote_addr: &'a str,

    /// Port number, must be >= 0
    #[serde(rename = "lp")]
    pub local_port: Option<u16>,

    /// Interface name
    #[serde(rename = "li")]
    pub local_interface: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct ListeningTcpPorts<'a> {
    #[serde(rename = "pts")]
    pub ports: Option<&'a [&'a TcpPort<'a>]>,

    #[serde(rename = "t")]
    pub total: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct TcpPort<'a> {
    #[serde(rename = "pt")]
    pub port: u16,

    #[serde(rename = "if")]
    pub interface: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct ListeningUdpPorts<'a> {
    #[serde(rename = "pts")]
    pub ports: Option<&'a [&'a UdpPort<'a>]>,

    #[serde(rename = "t")]
    pub total: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct UdpPort<'a> {
    #[serde(rename = "pt")]
    pub port: u16,

    #[serde(rename = "if")]
    pub interface: Option<&'a str>,
}

#[derive(Debug, Serialize)]
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
