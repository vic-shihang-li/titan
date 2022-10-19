use crate::protocol::Protocol;
use crate::{net, net::LinkDefinition, Args};
use etherparse::{InternetSlice, Ipv4HeaderSlice, PacketHeaders, SlicedPacket};
use lazy_static::lazy_static;
use std::collections::HashMap;
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

pub trait ProtocolHandler: Send + Sync {
    fn handle_packet(&self, payload: &[u8]);
}

enum PacketDecision {
    Drop,
    Forward,
    Consume,
}

pub struct Router {
    addrs: Vec<Ipv4Addr>,
    protocol_handlers: HashMap<Protocol, Box<dyn ProtocolHandler>>,
}

impl Router {
    pub fn new(addrs: &[Ipv4Addr]) -> Self {
        // TODO: spawn thread for cleaning up old routing table entries.

        Self {
            addrs: addrs.into(),
            protocol_handlers: HashMap::new(),
        }
    }

    /// Provide a handler for a protocol.
    ///
    /// Replaces any handler that is associated with the protocol.
    pub fn register_handler<H: ProtocolHandler + 'static>(
        &mut self,
        protocol: Protocol,
        handler: H,
    ) {
        self.protocol_handlers.insert(protocol, Box::new(handler));
    }

    pub async fn run(&self) {
        while let Ok(bytes) = net::listen().await.recv().await {
            // 0. parse bytes to packet
            // 1. drop if packet is not valid or TTL = 0
            // 2. if packet is for "me", pass packet to the correct protocol handler
            // 3. if forwarding table has rule for packet, send to the next-hop interface

            self.handle_packet_bytes(&bytes);
        }
    }

    fn handle_packet_bytes(&self, bytes: &[u8]) {
        match SlicedPacket::from_ip(bytes) {
            Err(value) => eprintln!("Err {:?}", value),
            Ok(packet) => {
                eprintln!("ip: {:?}", packet.ip);

                if packet.ip.is_none() {
                    eprintln!("Packet has no IP fields");
                    return;
                }

                let ip = packet.ip.unwrap();
                match ip {
                    InternetSlice::Ipv4(header, _) => self.handle_packet(&header, bytes),
                    InternetSlice::Ipv6(_, _) => eprintln!("Unsupported IPV6 packet"),
                };
            }
        }
    }

    fn handle_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, packet_bytes: &[u8]) {
        match self.decide_packet(&header) {
            PacketDecision::Drop => {}
            PacketDecision::Consume => self.consume_packet(header, packet_bytes),
            PacketDecision::Forward => self.forward_packet(),
        }
    }

    fn decide_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>) -> PacketDecision {
        if header.ttl() == 0 {
            return PacketDecision::Drop;
        }

        // TODO: if checksum is incorrect, drop.

        if self.is_my_addr(&header.destination_addr()) {
            return PacketDecision::Consume;
        }

        PacketDecision::Forward
    }

    fn is_my_addr(&self, addr: &Ipv4Addr) -> bool {
        self.addrs.iter().any(|a| a == addr)
    }

    fn consume_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, packet_bytes: &[u8]) {
        match header.protocol().try_into() {
            Ok(protocol) => match self.protocol_handlers.get(&protocol) {
                Some(handler) => {
                    handler.handle_packet(&packet_bytes);
                }
                None => eprintln!("Warning: no protocol handler for protocol {:?}", protocol),
            },
            Err(_) => eprintln!("Unrecognized protocol {}", header.protocol()),
        }
    }

    fn forward_packet(&self) {
        todo!()
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
