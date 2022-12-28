use std::fmt;
use std::net::Ipv4Addr;
use std::sync::Arc;

use tokio::net::UdpSocket;

use crate::utils::net::localhost_with_port;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LinkDefinition {
    /// The port where the connected host runs.
    pub dest_port: u16,
    /// The virtual IP of this host's interface.
    pub interface_ip: Ipv4Addr,
    /// The virtual IP of the connected host's interface.
    pub dest_ip: Ipv4Addr,
}

pub struct Link {
    dest_port: u16,
    dest_virtual_ip: Ipv4Addr,
    src_virtual_ip: Ipv4Addr,
    activated: bool,
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
            activated: true,
            sock: udp_socket,
        }
    }
}

#[derive(Debug)]
pub enum SendError {
    LinkInactive,
}

impl Link {
    /// On this link, send a message conforming to one of the supported protocols.
    pub async fn send(&self, payload: &[u8]) -> Result<(), SendError> {
        if !self.activated {
            return Err(SendError::LinkInactive);
        }

        self.sock
            .send_to(payload, localhost_with_port(self.dest_port))
            .await
            .unwrap();

        Ok(())
    }

    pub async fn activate(&mut self) {
        if !self.activated {
            self.activated = true;
        }
    }

    pub async fn deactivate(&mut self) {
        if self.activated {
            self.activated = false;
        }
    }

    pub fn is_disabled(&self) -> bool {
        !self.activated
    }

    pub fn dest(&self) -> Ipv4Addr {
        self.dest_virtual_ip
    }

    pub fn source(&self) -> Ipv4Addr {
        self.src_virtual_ip
    }

    pub fn clone_socket(&self) -> Arc<UdpSocket> {
        self.sock.clone()
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = if self.activated { "up" } else { "down" };
        write!(
            f,
            "{}\t{}\t{}\t{}",
            state, self.src_virtual_ip, self.dest_virtual_ip, self.dest_port
        )
    }
}
