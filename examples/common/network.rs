use embedded_nal::{AddrType, Dns, IpAddr, Ipv4Addr, Ipv6Addr, Mode, SocketAddr, TcpStack};
use heapless::{consts, String};
use native_tls::{Certificate, Identity, TlsConnector, TlsStream};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};

pub struct Network;

pub struct TcpSocket {
    pub stream: Option<TlsStream<TcpStream>>,
    mode: Mode,
}

impl TcpSocket {
    pub fn new(mode: Mode) -> Self {
        TcpSocket { stream: None, mode }
    }
}

impl Dns for Network {
    type Error = ();

    fn gethostbyname(&self, hostname: &str, _addr_type: AddrType) -> Result<IpAddr, Self::Error> {
        match hostname.to_socket_addrs().map_err(|_| ())?.next() {
            Some(socketaddr) => match socketaddr {
                std::net::SocketAddr::V4(socketv4) => {
                    let ipv4 = socketv4.ip().octets();
                    Ok(IpAddr::V4(Ipv4Addr::new(
                        ipv4[0], ipv4[1], ipv4[2], ipv4[3],
                    )))
                }
                std::net::SocketAddr::V6(socketv6) => {
                    let ipv6 = socketv6.ip().segments();
                    Ok(IpAddr::V6(Ipv6Addr::new(
                        ipv6[0], ipv6[1], ipv6[2], ipv6[3], ipv6[4], ipv6[5], ipv6[6], ipv6[7],
                    )))
                }
            },
            None => Err(()),
        }
    }
    fn gethostbyaddr(&self, _addr: IpAddr) -> Result<String<consts::U256>, Self::Error> {
        unimplemented!()
    }
}

impl TcpStack for Network {
    type Error = ();
    type TcpSocket = TcpSocket;

    fn open(&self, mode: Mode) -> Result<<Self as TcpStack>::TcpSocket, <Self as TcpStack>::Error> {
        Ok(TcpSocket::new(mode))
    }

    fn read_with<F>(&self, network: &mut Self::TcpSocket, f: F) -> nb::Result<usize, Self::Error>
    where
        F: FnOnce(&[u8], Option<&[u8]>) -> usize,
    {
        let buf = &mut [0u8; 512];
        let len = self.read(network, buf)?;
        Ok(f(&buf[..len], None))
    }

    fn read(
        &self,
        network: &mut <Self as TcpStack>::TcpSocket,
        buf: &mut [u8],
    ) -> Result<usize, nb::Error<<Self as TcpStack>::Error>> {
        if let Some(ref mut stream) = network.stream {
            stream.read(buf).map_err(|e| match e.kind() {
                ErrorKind::WouldBlock => nb::Error::WouldBlock,
                _ => nb::Error::Other(()),
            })
        } else {
            Err(nb::Error::Other(()))
        }
    }

    fn write(
        &self,
        network: &mut <Self as TcpStack>::TcpSocket,
        buf: &[u8],
    ) -> Result<usize, nb::Error<<Self as TcpStack>::Error>> {
        if let Some(ref mut stream) = network.stream {
            Ok(stream.write(buf).map_err(|_| nb::Error::Other(()))?)
        } else {
            Err(nb::Error::Other(()))
        }
    }

    fn is_connected(
        &self,
        _network: &<Self as TcpStack>::TcpSocket,
    ) -> Result<bool, <Self as TcpStack>::Error> {
        Ok(true)
    }

    fn connect(
        &self,
        network: <Self as TcpStack>::TcpSocket,
        remote: SocketAddr,
    ) -> Result<<Self as TcpStack>::TcpSocket, <Self as TcpStack>::Error> {
        let connector = TlsConnector::builder()
            .identity(
                Identity::from_pkcs12(include_bytes!("../secrets_mini_2/identity.pfx"), "")
                    .unwrap(),
            )
            .add_root_certificate(
                Certificate::from_pem(include_bytes!("../secrets_mini_2/certificate.pem.crt"))
                    .unwrap(),
            )
            .build()
            .unwrap();

        Ok(match TcpStream::connect(format!("{}", remote)) {
            Ok(stream) => {
                match network.mode {
                    Mode::Blocking => {
                        stream.set_write_timeout(None).unwrap();
                        stream.set_read_timeout(None).unwrap();
                    }
                    Mode::NonBlocking => panic!("Nonblocking socket mode not supported!"),
                    Mode::Timeout(t) => {
                        stream
                            .set_write_timeout(Some(std::time::Duration::from_millis(t as u64)))
                            .unwrap();
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_millis(t as u64)))
                            .unwrap();
                    }
                };
                let tls_stream = connector
                    .connect("a3f8k0ccx04zas.iot.eu-west-1.amazonaws.com", stream)
                    .unwrap();
                TcpSocket {
                    stream: Some(tls_stream),
                    mode: network.mode,
                }
            }
            Err(_e) => return Err(()),
        })
    }

    fn close(
        &self,
        _network: <Self as TcpStack>::TcpSocket,
    ) -> Result<(), <Self as TcpStack>::Error> {
        Ok(())
    }
}
