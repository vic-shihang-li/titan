use crate::route::ProtocolHandler;

#[derive(Default)]
pub struct RipHandler {}

impl ProtocolHandler for RipHandler {
    fn handle_packet(&self, payload: &[u8]) {
        // TODO:
        // 1. update the routing table (what APIs does the routing module need to provide?)
        // 2. send out triggered updates (use `net::iter_links()` and `link.send(rip_message)`)
    }
}
