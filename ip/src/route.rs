use crate::net;
use std::{net::Ipv4Addr, time::Instant};

struct RoutingTable {}

struct Entry {
    destination: Ipv4Addr,
    next_hop: Ipv4Addr,
    cost: u16,
    last_updated: Instant,
}

pub trait ProtocolHandler: Send {
    fn handle_packet(&self, packet: Vec<u8>);
}

/// Provide a handler for a protocol.
///
/// Replaces any handler that is associated with the protocol.
pub fn register_handler<H: ProtocolHandler + 'static>(protocol: u8, handler: H) {
    todo!()
}

pub struct Router {}

impl Router {
    async fn run(&self) {
        while let Ok(bytes) = net::listen().recv().await {
            // 0. parse bytes to packet
            // 1. drop if packet is not valid or TTL = 0
            // 2. if packet is for "me", pass packet to the correct protocol handler
            // 3. if forwarding table has rule for packet, send to the next-hop interface
        }
    }
}
