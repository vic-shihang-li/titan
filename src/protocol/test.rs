use std::fmt;
use std::net::Ipv4Addr;

use async_trait::async_trait;
use etherparse::{Ipv4Header, Ipv4HeaderSlice};

use crate::drop_policy::DropPolicy;
use crate::net::vtlink::VtLinkNet;
use crate::protocol::ProtocolHandler;
use crate::Message;

#[derive(Default)]
pub struct TestHandler {}

pub struct TestMessage {
    header: Ipv4Header,
    payload: Vec<u8>,
}

impl TestMessage {
    pub fn from_packet(header: &Ipv4HeaderSlice, payload: &[u8]) -> Self {
        let header = Ipv4Header::new(
            header.payload_len(),
            header.ttl() - 1,
            header.protocol(),
            header.source(),
            header.destination(),
        );
        Self {
            header,
            payload: payload.to_vec(),
        }
    }
    pub fn from_payload(payload: String) -> Self {
        let source: [u8; 4] = [0, 0, 0, 0];
        let header = Ipv4Header::new(payload.len() as u16, 15, 0, source, source);
        let payload = payload.into_bytes();
        Self { header, payload }
    }
}

impl Message for TestMessage {
    fn into_bytes(self) -> Vec<u8> {
        self.payload
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        let header = Ipv4HeaderSlice::from_slice(bytes).unwrap();
        let payload = &bytes[header.slice().len()..];
        Self::from_packet(&header, payload)
    }
}

#[async_trait]
impl<DP: DropPolicy> ProtocolHandler<DP> for TestHandler {
    async fn handle_packet<'a>(
        &self,
        _header: &Ipv4HeaderSlice<'a>,
        payload: &[u8],
        _net: &VtLinkNet<DP>,
    ) {
        let message = TestMessage::from_packet(_header, payload);
        print!("{message}");
    }
}

impl fmt::Display for TestMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "---Node received packet!---\n\
        \tsource IP\t:\t{:?}\n\
        \tdestination IP\t:\t{:?}\n\
        \tprotocol\t:\t{}\n\
        \tpayload length\t:\t{}\n\
        \tpayload\t\t:\t{}\n\
        ---------------------------
        ",
            Ipv4Addr::from(self.header.source),
            Ipv4Addr::from(self.header.destination),
            self.header.protocol,
            self.header.payload_len,
            String::from_utf8_lossy(&self.payload)
        )
    }
}
