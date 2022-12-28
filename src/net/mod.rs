pub mod vtlink;

use std::net::Ipv4Addr;

use async_trait::async_trait;

#[derive(Debug)]
pub enum SendError {
    NoForwardingEntry,
    Unreachable,
    NoLink,
    Transport(vtlink::Error),
}

#[async_trait]
pub trait Net: 'static + Send + Sync {
    async fn get_outbound_ip(&self, dest: Ipv4Addr) -> Option<[u8; 4]>;

    async fn send<P: Into<u8> + Send>(
        &self,
        payload: &[u8],
        protocol: P,
        dest: Ipv4Addr,
    ) -> Result<(), SendError>;
}
