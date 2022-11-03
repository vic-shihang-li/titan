mod buf;

use crate::route::ProtocolHandler;
use async_trait::async_trait;
use etherparse::Ipv4HeaderSlice;

#[derive(Default)]
pub struct TcpHandler {}

#[async_trait]
impl ProtocolHandler for TcpHandler {
    async fn handle_packet<'a>(&self, _header: &Ipv4HeaderSlice<'a>, _payload: &[u8]) {
        todo!()
    }
}
