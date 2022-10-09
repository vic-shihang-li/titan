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

pub trait ProtocolHandler: Send {
    fn handle_packet(&self, packet: Vec<u8>);
}

/// Provide a handler for a protocol.
///
/// Replaces any handler that is associated with the protocol.
pub fn register_handler<H: ProtocolHandler + 'static>(protocol: u8, handler: H) {}

/// Subscribe to a stream of packets received by this host.
///
/// The received data is a packet in its binary format.
async fn listen() -> Receiver<Vec<u8>> {
    todo!()
}

fn handle_packets() {
    // Note: on program start, some thread can be spawned to run this function.

    while let Ok(bytes) = listen() {
        // 0. parse bytes to packet
        // 1. drop if packet is not valid or TTL = 0
        // 2. if packet is for "me", pass packet to the correct protocol handler
        // 3. if forwarding table has rule for packet, send to the next-hop interface
    }
}
