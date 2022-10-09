use std::{net::Ipv4Addr, time::Instant};

struct RoutingTable {}

struct Entry {
    destination: Ipv4Addr,
    next_hop: Ipv4Addr,
    cost: u16,
    last_updated: Instant,
}
