use std::io::Write;

use crate::route::ProtocolHandler;

pub struct TestHandler {}

impl ProtocolHandler for TestHandler {
    fn handle_packet(&self, payload: &[u8]) {
        std::io::stdout().write_all(payload).unwrap();
    }
}
