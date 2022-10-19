use crate::{net, net::LinkDefinition, Args};
use lazy_static::lazy_static;
use std::fmt;
use std::{net::Ipv4Addr, time::Instant};
use tokio::sync::RwLock;

lazy_static! {
    static ref ROUTING_TABLE: RwLock<RoutingTable> = RwLock::new(RoutingTable::new());
}

pub struct InterfaceTable {
    interfaces: Vec<Interface>,
}

impl InterfaceTable {
    pub fn new() -> Self {
        Self {
            interfaces: Vec::new(),
        }
    }

    pub fn add_interface(&mut self, interface: Interface) {
        self.interfaces.push(interface);
    }

    pub fn remove_by_id(&mut self, id: u16) {
        self.interfaces.retain(|i| i.id != id);
    }

    pub fn get(&self, id: u16) -> Option<&Interface> {
        self.interfaces.iter().find(|i| i.id == id)
    }

    pub fn get_mut(&mut self, id: u16) -> Option<&mut Interface> {
        self.interfaces.iter_mut().find(|i| i.id == id)
    }

    pub fn get_by_ip(&self, ip: Ipv4Addr) -> Option<&Interface> {
        self.interfaces.iter().find(|i| i.local_ip == ip)
    }
}

impl fmt::Display for InterfaceTable {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.interfaces.iter().fold(Ok(()), |acc, interface| {
            acc.and_then(|_| writeln!(f, "{}", interface))
        })
    }
}

pub struct Interface {
    pub id: u16,
    pub state: bool,
    pub local_ip: Ipv4Addr,
    pub remote_ip: Ipv4Addr,
    pub port: u16,
}

impl Interface {
    pub fn new(id: u16, local_ip: Ipv4Addr, remote_ip: Ipv4Addr, port: u16) -> Self {
        Self {
            id,
            state: true,
            local_ip,
            remote_ip,
            port,
        }
    }
}

impl fmt::Display for Interface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = if self.state { "up" } else { "down" };
        write!(
            f,
            "{}\t{}\t{}\t{}\t{}",
            self.id, s, self.local_ip, self.remote_ip, self.port
        )
    }
}

pub struct RoutingTable {
    routes: Vec<Entry>,
}

impl RoutingTable {
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    pub fn add_route(&mut self, entry: Entry) {
        self.routes.push(entry);
    }

    pub fn get_route(&self, dest: Ipv4Addr) -> Option<&Entry> {
        self.routes.iter().find(|entry| entry.destination == dest)
    }

    pub fn get_route_mut(&mut self, dest: Ipv4Addr) -> Option<&mut Entry> {
        self.routes
            .iter_mut()
            .find(|entry| entry.destination == dest)
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
    fn handle_packet(&self, payload: &[u8]);
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

pub async fn bootstrap(args: &Args) {
    let mut rt = ROUTING_TABLE.write().await;

    for link in &args.links {
        // Add entry to my interface with a cost of 0.
        rt.add_route(Entry::new(link.interface_ip, link.interface_ip, 0));

        // Add entry to my neighbor with a cost of 1.
        rt.add_route(Entry::new(link.dest_ip, link.dest_ip, 1));
    }
}
