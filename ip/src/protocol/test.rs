use std::io::Write;

use async_trait::async_trait;
use etherparse::Ipv4HeaderSlice;

use crate::route::ProtocolHandler;

#[derive(Default)]
pub struct TestHandler {}

#[async_trait]
impl ProtocolHandler for TestHandler {
    async fn handle_packet<'a>(&self, _header: &Ipv4HeaderSlice<'a>, payload: &[u8]) {
        std::io::stdout().write_all(payload).unwrap();
    }
}
