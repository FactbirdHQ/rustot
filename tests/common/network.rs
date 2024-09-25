use std::net::SocketAddr;

use ::native_tls::Identity;
use embedded_io_adapters::tokio_1::FromTokio;
use embedded_nal_async::{AddrType, Dns, IpAddr, Ipv4Addr, Ipv6Addr, TcpConnect};
use tokio_native_tls::native_tls;

use super::credentials;

#[derive(Debug, Clone, Copy)]
pub struct Network;

impl Network {
    pub const fn new() -> Self {
        Self
    }
}

impl TcpConnect for Network {
    type Error = std::io::Error;

    type Connection<'a> = FromTokio<tokio::net::TcpStream>
	    where
		    Self: 'a;

    async fn connect<'a>(
        &'a self,
        remote: embedded_nal_async::SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        let stream = tokio::net::TcpStream::connect(format!("{}", remote)).await?;
        Ok(FromTokio::new(stream))
    }
}

impl Dns for Network {
    type Error = std::io::Error;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        for ip in tokio::net::lookup_host(format!("{}:0", host)).await? {
            match (&addr_type, ip) {
                (AddrType::IPv4 | AddrType::Either, SocketAddr::V4(ip)) => {
                    return Ok(IpAddr::V4(Ipv4Addr::from(ip.ip().octets())))
                }
                (AddrType::IPv6 | AddrType::Either, SocketAddr::V6(ip)) => {
                    return Ok(IpAddr::V6(Ipv6Addr::from(ip.ip().octets())))
                }
                (_, _) => {}
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "",
        ))
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        unimplemented!()
    }
}

pub struct TlsNetwork {
    identity: Identity,
    domain: String,
}

impl TlsNetwork {
    pub const fn new(domain: String, identity: Identity) -> Self {
        Self { identity, domain }
    }
}

impl TcpConnect for TlsNetwork {
    type Error = std::io::Error;

    type Connection<'a> = FromTokio<tokio_native_tls::TlsStream<tokio::net::TcpStream>>
	    where
		    Self: 'a;

    async fn connect<'a>(
        &'a self,
        remote: embedded_nal_async::SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        log::info!("Connecting to {:?}", remote);
        let connector = tokio_native_tls::TlsConnector::from(
            native_tls::TlsConnector::builder()
                .identity(self.identity.clone())
                .add_root_certificate(credentials::root_ca())
                .build()
                .unwrap(),
        );
        let stream = tokio::net::TcpStream::connect(format!("{}", remote)).await?;
        let tls_stream = connector
            .connect(self.domain.as_str(), stream)
            .await
            .unwrap();
        Ok(FromTokio::new(tls_stream))
    }
}

impl Dns for TlsNetwork {
    type Error = std::io::Error;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        log::info!("Looking up {}", host);
        for ip in tokio::net::lookup_host(format!("{}:0", host)).await? {
            log::info!("Found IP {}", ip);

            match (&addr_type, ip) {
                (AddrType::IPv4 | AddrType::Either, SocketAddr::V4(ip)) => {
                    return Ok(IpAddr::V4(Ipv4Addr::from(ip.ip().octets())))
                }
                (AddrType::IPv6 | AddrType::Either, SocketAddr::V6(ip)) => {
                    return Ok(IpAddr::V6(Ipv6Addr::from(ip.ip().octets())))
                }
                (_, _) => {}
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "",
        ))
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        unimplemented!()
    }
}
