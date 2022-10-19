use crate::{net, net::LinkDefinition, Args};
use etherparse::{InternetSlice, SlicedPacket};
use lazy_static::lazy_static;
use std::fmt;
use std::{net::Ipv4Addr, time::Instant};
use tokio::sync::{RwLock, RwLockReadGuard};

lazy_static! {
    static ref ROUTING_TABLE: RwLock<Vec<Entry>> = RwLock::new(Vec::new());
}


pub async fn get_routing_table() -> RwLockReadGuard<'static, Vec<Entry>> {
    ROUTING_TABLE.read().await
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
        while let Ok(bytes) = net::listen().await.recv().await {
            // 0. parse bytes to packet
            // 1. drop if packet is not valid or TTL = 0
            // 2. if packet is for "me", pass packet to the correct protocol handler
            // 3. if forwarding table has rule for packet, send to the next-hop interface

            match SlicedPacket::from_ip(&bytes) {
                Err(value) => eprintln!("Err {:?}", value),
                Ok(packet) => {
                    eprintln!("ip: {:?}", packet.ip);

                    if packet.ip.is_none() {
                        eprintln!("Packet has no IP fields");
                        continue;
                    }
                    let ip = packet.ip.unwrap();
                    match ip {
                        InternetSlice::Ipv4(headers, _) => {}
                        InternetSlice::Ipv6(_, _) => eprintln!("Unsupported IPV6 packet"),
                    };
                }
            }
        }
    }
}

pub async fn bootstrap(args: &Args) {
    let mut rt = ROUTING_TABLE.write().await;

    for link in &args.links {
        // Add entry to my interface with a cost of 0.
        rt.push(Entry::new(link.interface_ip, link.interface_ip, 0));

        // Add entry to my neighbor with a cost of 1.
        rt.push(Entry::new(link.dest_ip, link.dest_ip, 1));
    }
}
