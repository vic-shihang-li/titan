// TODO: remove this once the rest of TCP is implemented
#[allow(dead_code)]
mod buf;

use crate::{net::Net, protocol::ProtocolHandler, route::Router};
use async_trait::async_trait;
use etherparse::Ipv4HeaderSlice;

#[derive(Default)]
pub struct TcpHandler {}

#[async_trait]
impl ProtocolHandler for TcpHandler {
    async fn handle_packet<'a>(
        &self,
        _header: &Ipv4HeaderSlice<'a>,
        _payload: &[u8],
        _router: &Router,
        _net: &Net,
    ) {
        todo!()
    }
}
