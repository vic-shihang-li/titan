use crate::link::ProtocolHandler;

pub struct RipHandler {}

impl ProtocolHandler for RipHandler {
    fn handle_packet(&self, packet: Vec<u8>) {}
}
