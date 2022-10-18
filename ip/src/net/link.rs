use std::net::Ipv4Addr;
use std::sync::Arc;

use tokio::net::UdpSocket;

use super::utils::localhost_with_port;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LinkDefinition {
    /// The port where the connected host runs.
    pub dest_port: u16,
    /// The virtual IP of this host's interface.
    pub interface_ip: Ipv4Addr,
    /// The virtual IP of the connected host's interface.
    pub dest_ip: Ipv4Addr,
}

#[derive(Clone, Debug)]
pub struct Link {
    dest_port: u16,
    dest_virtual_ip: Ipv4Addr,
    src_virtual_ip: Ipv4Addr,
    sock: Arc<UdpSocket>,
}

#[derive(Debug)]
pub enum ParseLinkError {
    NoIp,
    NoPort,
    NoSrcVirtualIp,
    NoDstVirtualIp,
    MalformedPort,
    MalformedIp,
}

impl LinkDefinition {
    pub fn try_parse(raw_link: &str) -> Result<Self, ParseLinkError> {
        let mut split = raw_link.split_whitespace();

        split.next().ok_or(ParseLinkError::NoIp)?;

        let dest_port = split
            .next()
            .ok_or(ParseLinkError::NoPort)?
            .parse::<u16>()
            .map_err(|_| ParseLinkError::MalformedPort)?;

        let interface_ip = split
            .next()
            .ok_or(ParseLinkError::NoSrcVirtualIp)?
            .parse()
            .map_err(|_| ParseLinkError::MalformedIp)?;

        let dest_ip = split
            .next()
            .ok_or(ParseLinkError::NoDstVirtualIp)?
            .parse()
            .map_err(|_| ParseLinkError::MalformedIp)?;

        Ok(LinkDefinition {
            dest_port,
            interface_ip,
            dest_ip,
        })
    }

    pub fn into_link(self, udp_socket: Arc<UdpSocket>) -> Link {
        Link {
            dest_port: self.dest_port,
            dest_virtual_ip: self.dest_ip,
            src_virtual_ip: self.interface_ip,
            sock: udp_socket,
        }
    }
}

impl Link {
    pub async fn send(&self, bytes: &[u8]) {
        self.sock
            .send_to(bytes, localhost_with_port(self.dest_port))
            .await
            .unwrap();
    }
}
