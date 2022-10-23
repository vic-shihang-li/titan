use etherparse::Ipv4Header;
use std::net::{Ipv4Addr, SocketAddr};

use super::Link;

pub fn localhost_with_port(port: u16) -> SocketAddr {
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port)
}

#[derive(Default, Copy, Clone)]
pub struct Ipv4PacketBuilder<'a> {
    payload: Option<&'a [u8]>,
    ttl: Option<u8>,
    protocol: Option<u8>,
    src: Option<Ipv4Addr>,
    dst: Option<Ipv4Addr>,
}

#[derive(Debug)]
pub enum BuildError {
    NoPayload,
    NoProtocol,
    NoSourceAddress,
    NoDestinationAddress,
    PayloadTooLong,
}

impl<'a> Ipv4PacketBuilder<'a> {
    pub fn with_payload(&mut self, payload: &'a [u8]) -> &mut Self {
        self.payload = Some(payload);
        self
    }

    pub fn with_ttl(&mut self, ttl: u8) -> &mut Self {
        self.ttl = Some(ttl);
        self
    }

    pub fn with_protocol(&mut self, protocol: u8) -> &mut Self {
        self.protocol = Some(protocol);
        self
    }

    pub fn with_src(&mut self, src: Ipv4Addr) -> &mut Self {
        self.src = Some(src);
        self
    }

    pub fn with_dst(&mut self, dst: Ipv4Addr) -> &mut Self {
        self.dst = Some(dst);
        self
    }

    pub fn using_link(&mut self, link: &Link) -> &mut Self {
        self.src = Some(link.source());
        self.dst = Some(link.dest());
        self
    }

    pub fn build(self) -> Result<Vec<u8>, BuildError> {
        let mut buf = Vec::new();
        let payload = self.payload.ok_or(BuildError::NoPayload)?;
        let payload_len: u16 = payload
            .len()
            .try_into()
            .map_err(|_| BuildError::PayloadTooLong)?;
        let protocol = self.protocol.ok_or(BuildError::NoProtocol)?;
        let src = self.src.ok_or(BuildError::NoSourceAddress)?;
        let dst = self.dst.ok_or(BuildError::NoDestinationAddress)?;
        let ttl = self.ttl.unwrap_or(Ipv4PacketBuilder::default_ttl());

        let ip_header = Ipv4Header::new(payload_len, ttl, protocol, src.octets(), dst.octets());

        ip_header
            .write(&mut buf)
            .expect("IP header serialization error");

        buf.extend_from_slice(&payload);

        Ok(buf)
    }

    fn default_ttl() -> u8 {
        15
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_packet() {
        let payload = vec![1; 16];
        let src = Ipv4Addr::new(127, 0, 0, 1);
        let dst = Ipv4Addr::new(127, 0, 0, 2);
        let ttl = 10;
        let protocol = 12;

        let packet_bytes = Ipv4PacketBuilder::default()
            .with_src(src)
            .with_dst(dst)
            .with_ttl(ttl)
            .with_payload(payload.as_slice())
            .with_protocol(protocol)
            .build()
            .unwrap();

        let mut expected = Vec::new();
        Ipv4Header::new(
            payload.len().try_into().unwrap(),
            ttl,
            protocol,
            src.octets(),
            dst.octets(),
        )
        .write(&mut expected)
        .unwrap();
        expected.extend_from_slice(&payload);

        assert_eq!(expected, packet_bytes);
    }
}
