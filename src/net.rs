use crate::drop_policy::{DropFactor, DropPolicy};
use crate::link::{Ipv4PacketBuilder, VtLinkLayer};
use crate::protocol::rip::RipMessage;
use crate::protocol::Protocol;
use crate::utils::loop_with_interval;
use crate::{Args, Message};
use async_trait::async_trait;
use etherparse::Ipv4HeaderSlice;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use std::{net::Ipv4Addr, time::Instant};
use tokio::task::JoinHandle;

use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Default)]
pub struct ForwardingTable {
    entries: Vec<Entry>,
}

impl ForwardingTable {
    pub fn with_entries(entries: Vec<Entry>) -> Self {
        Self { entries }
    }

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
        for (i, entry) in self.entries().iter().enumerate() {
            if !entry.is_local() && entry.last_updated.elapsed() > max_age {
                log::warn!(
                    "Deleting entry {i}, {:?}, age: {:?}",
                    entry,
                    entry.last_updated.elapsed()
                );
            }
        }

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
    is_local: bool,
    pub last_updated: Instant,
}

impl Entry {
    pub fn new(destination: Ipv4Addr, next_hop: Ipv4Addr, cost: u32) -> Self {
        Self {
            destination,
            next_hop,
            cost,
            is_local: false,
            last_updated: Instant::now(),
        }
    }

    pub fn new_local(destination: Ipv4Addr, next_hop: Ipv4Addr, cost: u32) -> Self {
        Self {
            destination,
            next_hop,
            cost,
            is_local: true,
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
        self.is_local
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
        log::info!("resetting timer for entry: {}", self);
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

async fn prune_routing_table(
    table: Arc<RwLock<ForwardingTable>>,
    prune_interval: Duration,
    max_age: Duration,
) {
    loop_with_interval(prune_interval, || async {
        log::debug!("Pruning table");
        let mut table = table.write().await;
        table.prune(max_age);
    })
    .await;
}

async fn periodic_rip_update(
    table: Arc<RwLock<ForwardingTable>>,
    links: Arc<VtLinkLayer>,
    interval: Duration,
) {
    loop_with_interval(interval, || async {
        for link in &*links.iter_links().await {
            log::info!("Sending periodic update to {}", link.dest());
            let rip_msg = RipMessage::from_entries_with_poisoned_reverse(
                table.read().await.entries(),
                link.dest(),
            );
            let rip_msg_bytes = rip_msg.into_bytes();
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

#[derive(PartialEq, Eq, Debug)]
pub enum PacketDecision {
    Drop,
    Forward,
    Consume,
}

#[derive(Debug)]
pub enum SendError {
    NoForwardingEntry,
    Unreachable,
    NoLink,
    Transport(crate::link::Error),
}

pub struct RouterConfig {
    pub prune_interval: Duration,
    pub rip_update_interval: Duration,
    pub entry_max_age: Duration,
    pub drop_factor: usize,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            prune_interval: Duration::from_secs(1),
            rip_update_interval: Duration::from_secs(5),
            entry_max_age: Duration::from_secs(12),
            drop_factor: 0,
        }
    }
}

#[async_trait]
pub trait Net: 'static + Send + Sync {
    async fn get_outbound_ip(&self, dest: Ipv4Addr) -> Option<[u8; 4]>;

    async fn send<P: Into<u8> + Send>(
        &self,
        payload: &[u8],
        protocol: P,
        dest: Ipv4Addr,
    ) -> Result<(), SendError>;
}

pub struct VtLinkNet {
    links: Arc<VtLinkLayer>,
    my_addrs: Vec<Ipv4Addr>,
    routes: Arc<RwLock<ForwardingTable>>,
    pruner: JoinHandle<()>,
    rip_updater: JoinHandle<()>,
    drop_policy: DropFactor,
}

#[async_trait]
impl Net for VtLinkNet {
    async fn get_outbound_ip(&self, dest: Ipv4Addr) -> Option<[u8; 4]> {
        let rt = self.routes.read().await;
        if let Some(forward_rule) = rt.find_entry_for(dest) {
            self.links
                .find_link_to(forward_rule.next_hop)
                .await
                .map(|link| link.source().octets())
        } else {
            None
        }
    }

    async fn send<P: Into<u8> + Send>(
        &self,
        payload: &[u8],
        protocol: P,
        dest_vip: Ipv4Addr,
    ) -> Result<(), SendError> {
        let table = self.routes.read().await;

        let entry = table
            .find_entry_for(dest_vip)
            .ok_or(SendError::NoForwardingEntry)?;

        if entry.is_unreachable() {
            return Err(SendError::Unreachable);
        }

        let link = self
            .links
            .find_link_to(entry.next_hop())
            .await
            .ok_or_else(|| {
                log::warn!("No link found for next hop {}", entry.next_hop());
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
}

impl VtLinkNet {
    pub fn new(links: Arc<VtLinkLayer>, program_args: &Args, config: RouterConfig) -> Self {
        let my_addrs = program_args.get_my_interface_ips();

        let entries = program_args
            .links
            .iter()
            .map(|l| Entry::new_local(l.interface_ip, l.interface_ip, 0 /* cost */))
            .collect();
        let routes = Arc::new(RwLock::new(ForwardingTable::with_entries(entries)));

        let prune_interval = config.prune_interval;
        let entry_max_age = config.entry_max_age;
        let rip_update_interval = config.rip_update_interval;

        let pruner_routes = routes.clone();
        let pruner = tokio::spawn(async move {
            prune_routing_table(pruner_routes, prune_interval, entry_max_age).await;
        });

        let rip_updater_routes = routes.clone();
        let rip_updater_links = links.clone();
        let rip_updater = tokio::spawn(async move {
            periodic_rip_update(rip_updater_routes, rip_updater_links, rip_update_interval).await;
        });

        Self {
            links,
            my_addrs,
            routes,
            pruner,
            rip_updater,
            drop_policy: DropFactor::new(config.drop_factor),
        }
    }

    #[allow(clippy::needless_lifetimes)]
    pub async fn get_forwarding_table_mut<'a>(&'a self) -> RwLockWriteGuard<'a, ForwardingTable> {
        self.routes.write().await
    }

    #[allow(clippy::needless_lifetimes)]
    pub async fn get_forwarding_table<'a>(&'a self) -> RwLockReadGuard<'a, ForwardingTable> {
        self.routes.read().await
    }

    pub fn is_my_addr(&self, addr: Ipv4Addr) -> bool {
        self.my_addrs.iter().any(|a| *a == addr)
    }

    pub async fn decide_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>) -> PacketDecision {
        if !verify_header_checksum(header) {
            log::debug!("packet header checksum invalid; dropping packet");
            return PacketDecision::Drop;
        }

        let sender = header.source_addr();
        match self.links.find_link_to(sender).await {
            Some(link) => {
                if link.is_disabled() {
                    log::info!("Ignoring RIP packet from {}, link disabled", sender);
                    return PacketDecision::Drop;
                }
            }
            None => {
                log::debug!("Could not obtain the link where a packet is sent; is this in a test?");
            }
        };

        if self.drop_policy.should_drop(header) {
            return PacketDecision::Drop;
        }

        if self.is_my_addr(header.destination_addr()) {
            return PacketDecision::Consume;
        }

        if header.ttl() == 0 {
            log::debug!("packet TTL = 0; dropping packet");
            return PacketDecision::Drop;
        }

        PacketDecision::Forward
    }

    pub async fn forward_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, payload: &[u8]) {
        let dest = header.destination_addr();
        let rt = self.routes.read().await;

        if let Some(entry) = rt.find_entry_for(dest) {
            match self.links.find_link_to(entry.next_hop).await {
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

impl Drop for VtLinkNet {
    fn drop(&mut self) {
        self.pruner.abort();
        self.rip_updater.abort();
    }
}

#[allow(clippy::needless_lifetimes)]
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
        let r = make_mock_router().await;

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
        let r = make_mock_router().await;

        let packet = make_packet_with_zero_ttl();
        let decision = r
            .decide_packet(&Ipv4HeaderSlice::from_slice(&packet).unwrap())
            .await;

        assert_eq!(decision, PacketDecision::Drop);
    }

    #[tokio::test]
    async fn consume_packet_on_ip_match() {
        let abc_net = crate::fixture::netlinks::abc::gen_unique();
        let args = abc_net.a;
        let my_ips = args.get_my_interface_ips();
        let r = make_mock_router_with_args(args).await;

        let pkt = Ipv4PacketBuilder::default()
            .with_dst(my_ips[0])
            .with_src(Ipv4Addr::new(255, 255, 255, 255))
            .with_protocol(Protocol::Test)
            .with_payload(&[1, 2, 3, 4])
            .build()
            .unwrap();

        let decision = r
            .decide_packet(&Ipv4HeaderSlice::from_slice(&pkt).unwrap())
            .await;
        assert_eq!(decision, PacketDecision::Consume);

        // Even if a packet arrives with TTL=0, it should be processed if it
        // matches our IP.
        let pkt_zero_ttl = Ipv4PacketBuilder::default()
            .with_ttl(0)
            .with_dst(my_ips[0])
            .with_src(Ipv4Addr::new(255, 255, 255, 255))
            .with_protocol(Protocol::Test)
            .with_payload(&[1, 2, 3, 4])
            .build()
            .unwrap();

        let decision = r
            .decide_packet(&Ipv4HeaderSlice::from_slice(&pkt_zero_ttl).unwrap())
            .await;
        assert_eq!(decision, PacketDecision::Consume);
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

    async fn make_mock_router() -> VtLinkNet {
        let abc_net = crate::fixture::netlinks::abc::gen_unique();
        make_mock_router_with_args(abc_net.a).await
    }

    async fn make_mock_router_with_args(args: Args) -> VtLinkNet {
        let links = Arc::new(VtLinkLayer::new(&args).await);
        let router = VtLinkNet::new(links, &args, RouterConfig::default());
        router
    }
}
