use crate::net::{iter_links, Ipv4PacketBuilder, LinkRef};
use crate::protocol::rip::RipMessage;
use crate::protocol::Protocol;
use crate::utils::loop_with_interval;
use crate::{net, Args, Message};
use async_trait::async_trait;
use etherparse::{InternetSlice, Ipv4HeaderSlice, SlicedPacket};
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;
use std::{net::Ipv4Addr, time::Instant};

use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

lazy_static! {
    static ref FORWARDING_TABLE: RwLock<ForwardingTable> = RwLock::new(ForwardingTable::default());
}

pub async fn get_forwarding_table() -> RwLockReadGuard<'static, ForwardingTable> {
    FORWARDING_TABLE.read().await
}

pub async fn get_forwarding_table_mut() -> RwLockWriteGuard<'static, ForwardingTable> {
    FORWARDING_TABLE.write().await
}

#[derive(Default)]
pub struct ForwardingTable {
    entries: Vec<Entry>,
}

impl ForwardingTable {
    pub fn has_entry_for(&self, addr: Ipv4Addr) -> bool {
        self.entries.iter().any(|e| e.destination == addr)
    }

    pub fn find_mut_entry_for(&mut self, addr: Ipv4Addr) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.destination == addr)
    }

    pub fn find_entry_for(&self, addr: Ipv4Addr) -> Option<&Entry> {
        self.entries.iter().find(|e| e.destination == addr)
    }

    pub fn delete_mut_entry_for(&mut self, addr: Ipv4Addr) {
        self.entries.retain(|e| e.destination != addr)
    }

    pub fn add_entry(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[Entry] {
        self.entries.as_slice()
    }

    pub fn prune(&mut self, max_age: Duration) {
        let num_deleted = {
            let len_before = self.entries.len();
            self.entries
                .retain(|e| e.is_local() || e.last_updated.elapsed() < max_age);
            let len_after = self.entries().len();
            len_before - len_after
        };
        if num_deleted > 0 {
            log::info!("Table pruned, {num_deleted} entries deleted");
        }
    }
}

#[derive(Copy, Clone, Debug)]
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

    pub fn is_unreachable(&self) -> bool {
        self.cost >= Entry::max_cost()
    }

    pub fn next_hop(&self) -> Ipv4Addr {
        self.next_hop
    }

    /// Whether this is an entry for one of the router's own IPs
    pub fn is_local(&self) -> bool {
        self.destination == self.next_hop
    }

    pub async fn get_inner_link<'a>(&self) -> LinkRef<'a> {
        let r = if self.is_local() {
            net::find_link_with_interface_ip(self.destination).await
        } else {
            net::find_link_to(self.next_hop).await
        };

        if r.is_none() {
            panic!("Failed to find link for entry {:?}", self);
        }

        r.unwrap()
    }

    pub fn update(&mut self, next_hop: Ipv4Addr, cost: u32) {
        log::info!(
            "Update routing entry: old: {}, new next hop: {}, new cost: {}",
            self,
            next_hop,
            cost
        );
        self.next_hop = next_hop;
        self.cost = cost;
        self.restart_delete_timer();
    }

    pub fn mark_unreachable(&mut self) {
        self.update(self.next_hop, Entry::max_cost());
    }

    pub fn update_cost(&mut self, cost: u32) {
        self.update(self.next_hop, cost);
    }

    pub fn restart_delete_timer(&mut self) {
        self.last_updated = Instant::now();
    }

    pub fn max_cost() -> u32 {
        16
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}\t{}\t{}", self.destination, self.next_hop, self.cost)
    }
}

async fn prune_routing_table(prune_interval: Duration, max_age: Duration) {
    loop_with_interval(prune_interval, || async {
        let mut table = FORWARDING_TABLE.write().await;
        table.prune(max_age);
    })
    .await;
}

async fn periodic_rip_update(interval: Duration) {
    loop_with_interval(interval, || async {
        log::info!("Sending periodic update");
        let table = FORWARDING_TABLE.read().await;

        for link in &*iter_links().await {
            let rip_msg_bytes =
                RipMessage::from_entries_with_poisoned_reverse(table.entries(), link.dest())
                    .into_bytes();
            let packet = Ipv4PacketBuilder::default()
                .with_payload(&rip_msg_bytes)
                .with_protocol(Protocol::Rip)
                .with_src(link.source())
                .with_dst(link.dest())
                .build()
                .unwrap();
            link.send(&packet).await.ok();
        }
    })
    .await;
}

#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    async fn handle_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, payload: &[u8]);
}

#[derive(PartialEq, Eq, Debug)]
enum PacketDecision {
    Drop,
    Forward,
    Consume,
}

pub struct Router {
    my_addrs: Vec<Ipv4Addr>,
    protocol_handlers: HashMap<Protocol, Box<dyn ProtocolHandler>>,
}

impl Router {
    pub fn new(my_addrs: &[Ipv4Addr]) -> Self {
        Self {
            my_addrs: my_addrs.into(),
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

    pub fn is_my_addr(&self, addr: Ipv4Addr) -> bool {
        self.my_addrs.iter().any(|a| *a == addr)
    }

    pub async fn run(&self) {
        while let Ok(bytes) = net::listen().await.recv().await {
            // 0. parse bytes to packet
            // 1. drop if packet is not valid or TTL = 0
            // 2. if packet is for "me", pass packet to the correct protocol handler
            // 3. if forwarding table has rule for packet, send to the next-hop interface

            log::info!("Receiving packet");
            self.handle_packet_bytes(&bytes).await;
        }

        panic!("Premature run loop exit");
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
                    InternetSlice::Ipv4(header, _) => self.handle_packet(&header, payload).await,
                    InternetSlice::Ipv6(_, _) => eprintln!("Unsupported IPV6 packet"),
                };
            }
        }
    }

    async fn handle_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, payload: &[u8]) {
        match self.decide_packet(header).await {
            PacketDecision::Drop => {}
            PacketDecision::Consume => self.consume_packet(header, payload).await,
            PacketDecision::Forward => self.forward_packet(header, payload).await,
        }
    }

    async fn decide_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>) -> PacketDecision {
        if header.ttl() == 0 {
            log::debug!("packet TTL = 0; dropping packet");
            return PacketDecision::Drop;
        }

        if !verify_header_checksum(header) {
            log::debug!("packet header checksum invalid; dropping packet");
            return PacketDecision::Drop;
        }

        if self.is_my_addr(header.destination_addr()) {
            return PacketDecision::Consume;
        }

        PacketDecision::Forward
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

    async fn forward_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, payload: &[u8]) {
        let dest = header.destination_addr();
        let rt = FORWARDING_TABLE.read().await;

        if let Some(entry) = rt.find_entry_for(dest) {
            match net::find_link_to(entry.next_hop).await {
                Some(link) => {
                    let packet = Ipv4PacketBuilder::default()
                        .with_src(header.source_addr())
                        .with_dst(header.destination_addr())
                        .with_payload(payload)
                        .with_protocol(header.protocol())
                        .with_ttl(header.ttl() - 1)
                        .build()
                        .unwrap();

                    if let Err(e) = link.send(&packet).await {
                        log::warn!("Error forwarding packet, {:?}", e);
                    }
                }
                None => {
                    log::warn!("No link to next hop {}, dropping packet", entry.next_hop);
                }
            }
        } else {
            log::warn!("No route to {}, dropping packet", dest);
        }
    }
}

#[derive(Debug, Clone)]
pub struct BootstrapArgs<'a> {
    program_args: &'a Args,
    prune_interval: Duration,
    rip_update_interval: Duration,
    entry_max_age: Duration,
}

impl<'a> BootstrapArgs<'a> {
    pub fn new(args: &'a Args) -> Self {
        Self {
            program_args: args,
            prune_interval: Duration::from_secs(1),
            rip_update_interval: Duration::from_secs(5),
            entry_max_age: Duration::from_secs(12),
        }
    }

    /// Set the interval of sending up periodic RIP updates.
    pub fn with_rip_interval(&mut self, rip_interval: Duration) -> &mut Self {
        self.rip_update_interval = rip_interval;
        self
    }

    /// Set the maximum time a routing entry can live without receiving an update.
    pub fn with_entry_max_age(&mut self, max_age: Duration) -> &mut Self {
        self.entry_max_age = max_age;
        self
    }
}

pub async fn bootstrap<'a>(args: &'a BootstrapArgs<'a>) {
    let mut rt = FORWARDING_TABLE.write().await;

    for link in &args.program_args.links {
        // Add entry to my interface with a cost of 0.
        rt.add_entry(Entry::new(link.interface_ip, link.interface_ip, 0));
    }

    let prune_interval = args.prune_interval;
    let entry_max_age = args.entry_max_age;
    let rip_update_interval = args.rip_update_interval;
    tokio::spawn(async move {
        prune_routing_table(prune_interval, entry_max_age).await;
    });
    tokio::spawn(async move {
        periodic_rip_update(rip_update_interval).await;
    });
}

#[derive(Debug)]
pub enum SendError {
    NoForwardingEntry,
    Unreachable,
    NoLink,
    Transport(crate::net::Error),
}

pub async fn send<P: Into<u8>>(
    payload: &[u8],
    protocol: P,
    dest_vip: Ipv4Addr,
) -> Result<(), SendError> {
    let table = FORWARDING_TABLE.read().await;

    let entry = table
        .find_entry_for(dest_vip)
        .ok_or(SendError::NoForwardingEntry)?;

    if entry.is_unreachable() {
        return Err(SendError::Unreachable);
    }

    let link = net::find_link_to(entry.next_hop).await.ok_or_else(|| {
        log::warn!("No link found for next hop {}", entry.next_hop);
        SendError::NoLink
    })?;

    let packet = Ipv4PacketBuilder::default()
        .with_src(link.source())
        .with_dst(dest_vip)
        .with_payload(payload)
        .with_protocol(protocol)
        .build()
        .unwrap();

    link.send(&packet)
        .await
        .map_err(|e| SendError::Transport(e.into()))
}

fn verify_header_checksum<'a>(header: &Ipv4HeaderSlice<'a>) -> bool {
    let owned_header = header.to_header();
    match owned_header.calc_header_checksum() {
        Ok(expected_checksum) => expected_checksum == header.header_checksum(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use etherparse::{Ipv4Header, Ipv4HeaderSlice};

    use super::*;

    #[tokio::test]
    async fn drop_packet_with_invalid_checksum() {
        let r = Router::new(&[]);

        let valid_packet = make_random_packet();
        let decision = r
            .decide_packet(&Ipv4HeaderSlice::from_slice(&valid_packet).unwrap())
            .await;

        assert_eq!(decision, PacketDecision::Forward);

        let invalid_packet = make_random_packet_with_incorrect_checksum();
        let decision = r
            .decide_packet(&Ipv4HeaderSlice::from_slice(&invalid_packet).unwrap())
            .await;

        assert_eq!(decision, PacketDecision::Drop);
    }

    #[tokio::test]
    async fn drop_packet_with_zero_ttl() {
        let r = Router::new(&[]);

        let packet = make_packet_with_zero_ttl();
        let decision = r
            .decide_packet(&Ipv4HeaderSlice::from_slice(&packet).unwrap())
            .await;

        assert_eq!(decision, PacketDecision::Drop);
    }

    #[tokio::test]
    async fn consume_packet_on_ip_match() {
        let my_ip = Ipv4Addr::new(1, 2, 3, 4);
        let r = Router::new(&[my_ip]);

        let pkt = Ipv4PacketBuilder::default()
            .with_dst(my_ip)
            .with_src(Ipv4Addr::new(255, 255, 255, 255))
            .with_protocol(Protocol::Test)
            .with_payload(&[1, 2, 3, 4])
            .build()
            .unwrap();

        let decision = r
            .decide_packet(&Ipv4HeaderSlice::from_slice(&pkt).unwrap())
            .await;
        assert_eq!(decision, PacketDecision::Consume);

        // Drop packets with zero TTL even if destination IP matches
        let pkt_zero_ttl = Ipv4PacketBuilder::default()
            .with_ttl(0)
            .with_dst(my_ip)
            .with_src(Ipv4Addr::new(255, 255, 255, 255))
            .with_protocol(Protocol::Test)
            .with_payload(&[1, 2, 3, 4])
            .build()
            .unwrap();

        let decision = r
            .decide_packet(&Ipv4HeaderSlice::from_slice(&pkt_zero_ttl).unwrap())
            .await;
        assert_eq!(decision, PacketDecision::Drop);
    }

    fn make_random_packet() -> Vec<u8> {
        let (header, mut payload) = make_random_packet_internal();
        let mut v = Vec::new();
        header.write(&mut v).unwrap();
        v.append(&mut payload);
        v
    }

    fn make_packet_with_zero_ttl() -> Vec<u8> {
        let (mut header, mut payload) = make_random_packet_internal();
        header.time_to_live = 0;

        let mut v = Vec::new();
        header.write(&mut v).unwrap();
        v.append(&mut payload);
        v
    }

    fn make_random_packet_with_incorrect_checksum() -> Vec<u8> {
        let (mut header, mut payload) = make_random_packet_internal();

        // set a bogus checksum value
        header.header_checksum = 128;

        let mut v = Vec::new();
        header.write_raw(&mut v).unwrap();
        v.append(&mut payload);
        v
    }

    fn make_random_packet_internal() -> (Ipv4Header, Vec<u8>) {
        let payload = vec![1; 8];

        let header = Ipv4Header::new(
            payload.len().try_into().unwrap(),
            8,
            0,
            [1, 2, 3, 4],
            [5, 6, 7, 8],
        );

        (header, payload)
    }
}
