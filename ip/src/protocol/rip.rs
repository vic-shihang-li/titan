use crate::route::ProtocolHandler;

pub struct RipHandler {}

impl ProtocolHandler for RipHandler {
    fn handle_packet(&self, payload: &[u8]) {}
}
