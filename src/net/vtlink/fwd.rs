use core::fmt;
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};

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
        log::debug!("resetting timer for entry: {}", self);
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
