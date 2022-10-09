use std::net::Ipv4Addr;

use tokio::sync::broadcast::Receiver;

#[derive(Copy, Clone)]
pub struct Link {}

/// Send bytes to a destination.
///
/// The destination is typically the next-hop address for a packet.
pub fn send(bytes: &[u8], dest: Ipv4Addr) {
    todo!()
}

/// Turns on a link interface.
pub fn activate(link_no: u16) {
    todo!()
}

/// Turns off a link interface.
pub fn deactivate(link_no: u16) {
    todo!()
}

/// Iterate all links (both active and inactive) for this host.
///
/// This is useful for sending out periodic RIP messages to all links.
pub fn iter_links() -> LinkIter {
    todo!()
}

pub struct LinkIter {}

impl Iterator for LinkIter {
    type Item = Link;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

/// Subscribe to a stream of packets received by this host.
///
/// The received data is a packet in its binary format.
pub fn listen() -> Receiver<Vec<u8>> {
    todo!()
}
