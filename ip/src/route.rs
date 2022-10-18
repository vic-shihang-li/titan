use crate::net;
use std::{net::Ipv4Addr, time::Instant};
use std::fmt;

pub struct RoutingTable {
    routes: Vec<Entry>
}

impl RoutingTable {
    pub fn new() -> Self {
        Self {
            routes: Vec::new()
        }
    }

    pub fn add_route(&mut self, entry: Entry) {
        self.routes.push(entry);
    }

    pub fn get_route(&self, dest: Ipv4Addr) -> Option<&Entry> {
        self.routes.iter().find(|entry| entry.destination == dest)
    }

    pub fn get_route_mut(&mut self, dest: Ipv4Addr) -> Option<&mut Entry> {
        self.routes.iter_mut().find(|entry| entry.destination == dest)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.routes.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entry> {
        self.routes.iter_mut()
    }

    pub fn remove_route(&mut self, dest: Ipv4Addr) {
        self.routes.retain(|entry| entry.destination != dest);
    }
}

impl fmt::Display for RoutingTable {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.routes.iter().fold(Ok(()), |acc, entry| {
            acc.and_then(|_| writeln!(f, "{}", entry))
        })
    }
}

pub struct Entry {
    destination: Ipv4Addr,
    next_hop: Ipv4Addr,
    cost: u16,
    last_updated: Instant,
}

impl Entry {
    pub fn new(destination: Ipv4Addr, next_hop: Ipv4Addr, cost: u16) -> Self {
        Self {
            destination,
            next_hop,
            cost,
            last_updated: Instant::now(),
        }
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}\t{}\t{}", self.destination, self.next_hop, self.cost)
    }
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


pub fn bootstrap_routing_table() -> RoutingTable {
    let mut rt = RoutingTable::new();
    rt.add_route(Entry::new(Ipv4Addr::new(10, 0, 0, 0), Ipv4Addr::new(10, 0, 0, 0), 1));
    rt.add_route(Entry::new(Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 1), 1));
    rt.add_route(Entry::new(Ipv4Addr::new(10, 0, 0, 2), Ipv4Addr::new(10, 0, 0, 2), 1));
    rt.add_route(Entry::new(Ipv4Addr::new(10, 0, 0, 3), Ipv4Addr::new(10, 0, 0, 3), 1));
    rt
}