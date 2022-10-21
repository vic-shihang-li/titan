use crate::net::iter_links;
use crate::protocol::rip::RipMessage;
use crate::protocol::Protocol;
use crate::{net, Args};
use async_trait::async_trait;
use etherparse::{InternetSlice, Ipv4HeaderSlice, SlicedPacket};
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::time::Duration;
use std::{net::Ipv4Addr, time::Instant};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

lazy_static! {
    static ref ROUTING_TABLE: RwLock<RoutingTable> = {
        tokio::spawn(async {
            prune_routing_table().await;
        });
        tokio::spawn(async {
            periodic_rip_update().await;
        });

        RwLock::new(RoutingTable::default())
    };
}

pub async fn get_routing_table() -> RwLockReadGuard<'static, RoutingTable> {
    ROUTING_TABLE.read().await
}

pub async fn get_routing_table_mut() -> RwLockWriteGuard<'static, RoutingTable> {
    ROUTING_TABLE.write().await
}

#[derive(Default)]
pub struct RoutingTable {
    entries: Vec<Entry>,
}

impl RoutingTable {
    pub fn has_entry_for(&self, addr: Ipv4Addr) -> bool {
        self.entries.iter().any(|e| e.destination == addr)
    }

    pub fn find_mut_entry_for(&mut self, addr: Ipv4Addr) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.destination == addr)
    }

    pub fn add_entry(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[Entry] {
        self.entries.as_slice()
    }

    pub fn prune(&mut self) {
        let max_age = Duration::from_secs(12);
        self.entries.retain(|e| e.last_updated.elapsed() < max_age);
    }
}

#[derive(Copy, Clone)]
pub struct Entry {
    destination: Ipv4Addr,
    next_hop: Ipv4Addr,
    cost: u32,
    last_updated: Instant,
}

impl Entry {
    pub fn new(destination: Ipv4Addr, next_hop: Ipv4Addr, cost: u32) -> Self {
        Self {
            destination,
            next_hop,
            cost,
            last_updated: Instant::now(),
        }
    }

    pub fn destination(&self) -> Ipv4Addr {
        self.destination
    }

    pub fn cost(&self) -> u32 {
        self.cost
    }

    pub fn next_hop(&self) -> Ipv4Addr {
        self.next_hop
    }

    pub fn update(&mut self, next_hop: Ipv4Addr, cost: u32) {
        self.next_hop = next_hop;
        self.cost = cost;
        self.last_updated = Instant::now();
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}\t{}\t{}", self.destination, self.next_hop, self.cost)
    }
}

async fn prune_routing_table() {
    loop_with_interval(Duration::from_secs(1), || async {
        let mut table = ROUTING_TABLE.write().await;
        table.prune();
    })
    .await;
}

async fn periodic_rip_update() {
    loop_with_interval(Duration::from_secs(5), || async {
        let table = ROUTING_TABLE.read().await;
        let msg = RipMessage::from_entries(table.entries());
        for link in &*iter_links().await {
            link.send(msg.clone().into()).await.ok();
        }
    })
    .await;
}

#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    async fn handle_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, payload: &[u8]);
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

            self.handle_packet_bytes(&bytes).await;
        }
    }

    async fn handle_packet_bytes(&self, bytes: &[u8]) {
        match SlicedPacket::from_ip(bytes) {
            Err(value) => eprintln!("Err {:?}", value),
            Ok(packet) => {
                if packet.ip.is_none() {
                    eprintln!("Packet has no IP fields");
                    return;
                }

                let ip = packet.ip.unwrap();
                let payload = packet.payload;

                match ip {
                    InternetSlice::Ipv4(header, _) => {
                        self.handle_packet(&header, payload, bytes).await
                    }
                    InternetSlice::Ipv6(_, _) => eprintln!("Unsupported IPV6 packet"),
                };
            }
        }
    }

    async fn handle_packet<'a>(
        &self,
        header: &Ipv4HeaderSlice<'a>,
        payload: &[u8],
        packet_bytes: &[u8],
    ) {
        match self.decide_packet(header) {
            PacketDecision::Drop => {}
            PacketDecision::Consume => self.consume_packet(header, payload).await,
            PacketDecision::Forward => self.forward_packet(header, packet_bytes).await,
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

    async fn consume_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, payload: &[u8]) {
        match header.protocol().try_into() {
            Ok(protocol) => match self.protocol_handlers.get(&protocol) {
                Some(handler) => {
                    handler.handle_packet(header, payload).await;
                }
                None => eprintln!("Warning: no protocol handler for protocol {:?}", protocol),
            },
            Err(_) => eprintln!("Unrecognized protocol {}", header.protocol()),
        }
    }

    async fn forward_packet<'a>(&self, _header: &Ipv4HeaderSlice<'a>, _packet_bytes: &[u8]) {
        todo!()
    }
}

pub async fn bootstrap(args: &Args) {
    let mut rt = ROUTING_TABLE.write().await;

    for link in &args.links {
        // Add entry to my interface with a cost of 0.
        rt.add_entry(Entry::new(link.interface_ip, link.interface_ip, 0));

        // Add entry to my neighbor with a cost of 1.
        rt.add_entry(Entry::new(link.dest_ip, link.dest_ip, 1));
    }
}

async fn loop_with_interval<Fut: Future<Output = ()>>(interval: Duration, f: impl Fn() -> Fut) {
    loop {
        f().await;
        tokio::time::sleep(interval).await;
    }
}
