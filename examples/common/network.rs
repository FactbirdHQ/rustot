use embedded_nal::{AddrType, Dns, IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpClientStack};
use heapless::String;
use native_tls::{Certificate, Identity, TlsConnector, TlsStream};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};

pub struct Network;

pub struct TcpSocket {
    pub stream: Option<TlsStream<TcpStream>>,
}

impl TcpSocket {
    pub fn new() -> Self {
        TcpSocket { stream: None }
    }
}

impl Dns for Network {
    type Error = ();

    fn get_host_by_name(
        &self,
        hostname: &str,
        _addr_type: AddrType,
    ) -> nb::Result<IpAddr, Self::Error> {
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
            None => Err(nb::Error::Other(())),
        }
    }
    fn get_host_by_address(&self, _addr: IpAddr) -> nb::Result<String<256>, Self::Error> {
        unimplemented!()
    }
}

impl TcpClientStack for Network {
    type Error = ();
    type TcpSocket = TcpSocket;

    fn socket(
        &mut self,
    ) -> Result<<Self as TcpClientStack>::TcpSocket, <Self as TcpClientStack>::Error> {
        Ok(TcpSocket::new())
    }

    fn receive(
        &mut self,
        network: &mut <Self as TcpClientStack>::TcpSocket,
        buf: &mut [u8],
    ) -> Result<usize, nb::Error<<Self as TcpClientStack>::Error>> {
        if let Some(ref mut stream) = network.stream {
            stream.read(buf).map_err(|e| match e.kind() {
                ErrorKind::WouldBlock => nb::Error::WouldBlock,
                _ => nb::Error::Other(()),
            })
        } else {
            Err(nb::Error::Other(()))
        }
    }

    fn send(
        &mut self,
        network: &mut <Self as TcpClientStack>::TcpSocket,
        buf: &[u8],
    ) -> Result<usize, nb::Error<<Self as TcpClientStack>::Error>> {
        if let Some(ref mut stream) = network.stream {
            Ok(stream.write(buf).map_err(|_| nb::Error::Other(()))?)
        } else {
            Err(nb::Error::Other(()))
        }
    }

    fn is_connected(
        &mut self,
        _network: &<Self as TcpClientStack>::TcpSocket,
    ) -> Result<bool, <Self as TcpClientStack>::Error> {
        Ok(true)
    }

    fn connect(
        &mut self,
        network: &mut <Self as TcpClientStack>::TcpSocket,
        remote: SocketAddr,
    ) -> nb::Result<(), <Self as TcpClientStack>::Error> {
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

        match TcpStream::connect(format!("{}", remote)) {
            Ok(stream) => {
                stream.set_write_timeout(None).unwrap();
                stream.set_read_timeout(None).unwrap();

                let tls_stream = connector
                    .connect("a3f8k0ccx04zas.iot.eu-west-1.amazonaws.com", stream)
                    .unwrap();
                network.stream.replace(tls_stream);
            }
            Err(_e) => return Err(nb::Error::Other(())),
        }
        Ok(())
    }

    fn close(
        &mut self,
        _network: <Self as TcpClientStack>::TcpSocket,
    ) -> Result<(), <Self as TcpClientStack>::Error> {
        Ok(())
    }
}
