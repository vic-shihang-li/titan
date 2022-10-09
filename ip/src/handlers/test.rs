use crate::link::ProtocolHandler;

pub struct TestHandler {}

impl ProtocolHandler for TestHandler {
    fn handle_packet(&self, packet: Vec<u8>) {}
}
