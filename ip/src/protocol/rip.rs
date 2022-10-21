use async_trait::async_trait;
use etherparse::Ipv4HeaderSlice;

use crate::{
    net::iter_links,
    route::{get_routing_table_mut, Entry as RoutingEntry, ProtocolHandler},
};

use std::{cmp, cmp::Ordering, net::Ipv4Addr};

use crate::Message;

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub struct Entry {
    cost: u32,
    address: Ipv4Addr,
    mask: Ipv4Addr,
}

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum Command {
    Request,
    Response,
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct RipMessage {
    command: Command,
    entries: Vec<Entry>,
}

#[allow(clippy::from_over_into)]
impl Into<u16> for Command {
    fn into(self) -> u16 {
        match self {
            Command::Request => 1,
            Command::Response => 2,
        }
    }
}

impl Entry {
    /// Constructs a RIP message entry with a mask of 255.255.255.255.
    pub fn with_default_mask(cost: u32, address: Ipv4Addr) -> Self {
        Self {
            cost,
            address,
            mask: Ipv4Addr::new(255, 255, 255, 255),
        }
    }
}

#[derive(Debug)]
pub enum ParseCommandError {
    BadValue(u16),
}

impl TryFrom<u16> for Command {
    type Error = ParseCommandError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Command::Request),
            2 => Ok(Command::Response),
            _ => Err(ParseCommandError::BadValue(value)),
        }
    }
}

impl RipMessage {
    pub fn from_entries(entries: &[RoutingEntry]) -> Self {
        let cmd = Command::Response;
        let entries = entries
            .iter()
            .map(|e| Entry::with_default_mask(e.cost(), e.destination()))
            .collect();

        Self {
            command: cmd,
            entries,
        }
    }
}

impl Message for RipMessage {
    fn into_bytes(self) -> Vec<u8> {
        let cmd: u16 = self.command.into();
        let num_entries: u16 = self
            .entries
            .len()
            .try_into()
            .expect("RIP message has too many entries");

        let mut v = Vec::new();

        v.extend_from_slice(&cmd.to_be_bytes());
        v.extend_from_slice(&num_entries.to_be_bytes());

        for entry in self.entries {
            v.append(&mut entry.into_bytes());
        }

        v
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        assert!(bytes.len() >= 2, "Missing command byte");

        let cmd: u16 = u16::from_be_bytes(bytes[0..2].try_into().unwrap());
        let command = Command::try_from(cmd).expect("Bad command type");

        assert!(bytes.len() >= 4, "Missing num entries byte");
        let num_entries: u16 = u16::from_be_bytes(bytes[2..4].try_into().unwrap());

        if command == Command::Request {
            assert!(
                num_entries == 0,
                "request RIP message cannot have any entries"
            );
        }

        assert!(
            bytes.len() >= 4 + num_entries as usize * Entry::serialized_size(),
            "Missing entry bytes"
        );

        let mut entries = Vec::new();
        let mut start = 4;
        let mut remaining_entries = num_entries;
        while remaining_entries > 0 {
            let entry = Entry::from_bytes(&bytes[start..start + Entry::serialized_size()]);
            entries.push(entry);
            remaining_entries -= 1;
            start += Entry::serialized_size();
        }

        Self { command, entries }
    }
}

impl Message for Entry {
    fn into_bytes(self) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&self.cost.to_be_bytes());
        v.extend_from_slice(&self.address.octets());
        v.extend_from_slice(&self.mask.octets());
        v
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        assert!(
            bytes.len() >= Entry::serialized_size(),
            "Not enough bytes for Entry"
        );

        let cost = u32::from_be_bytes(bytes[..4].try_into().unwrap());
        let address = Ipv4Addr::from(u32::from_be_bytes(bytes[4..8].try_into().unwrap()));
        let mask = Ipv4Addr::from(u32::from_be_bytes(bytes[8..12].try_into().unwrap()));

        Self {
            cost,
            address,
            mask,
        }
    }
}

impl Entry {
    fn serialized_size() -> usize {
        12
    }
}

#[derive(Default)]
pub struct RipHandler {}

#[async_trait]
impl ProtocolHandler for RipHandler {
    async fn handle_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, payload: &[u8]) {
        let message = RipMessage::from_bytes(payload);
        let sender = header.source_addr();

        let mut rt = get_routing_table_mut().await;

        let mut updates = Vec::new();

        // RIP protocol implementation.
        // Reference: http://intronetworks.cs.luc.edu/current2/html/routing.html#distance-vector-update-rules
        for entry in &message.entries {
            match rt.find_mut_entry_for(entry.address) {
                Some(found) => {
                    match entry.cost.cmp(&found.cost()) {
                        Ordering::Less => {
                            found
                                .update(sender, cmp::min(entry.cost + 1, RoutingEntry::max_cost()));
                            updates.push(*found);
                        }
                        Ordering::Greater => {
                            if found.next_hop() == sender {
                                found.update(found.next_hop(), entry.cost);
                                // poisoned reverse
                                updates.push(RoutingEntry::new(
                                    entry.address,
                                    header.destination_addr(),
                                    RoutingEntry::max_cost(),
                                ));
                            }
                        }
                        Ordering::Equal => {
                            // If new cost == old cost, we ignore the new report.
                            // Accepting the new report could destabilize the network (see Ex. 8
                            // in the Chapter 13 of Dordal).
                        }
                    }
                }
                None => {
                    let dest = entry.address;
                    let cost = cmp::min(entry.cost + 1, RoutingEntry::max_cost());

                    let entry = RoutingEntry::new(dest, sender, cost);
                    rt.add_entry(entry);
                    updates.push(entry);
                }
            }
        }

        if !updates.is_empty() {
            let update_msg = RipMessage::from_entries(&updates);
            for link in &*iter_links().await {
                link.send(update_msg.clone().into()).await.ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rip_message_serde() {
        let msg = RipMessage {
            command: Command::Response,
            entries: vec![
                Entry::with_default_mask(1, Ipv4Addr::new(127, 0, 1, 2)),
                Entry::with_default_mask(8, Ipv4Addr::new(3, 4, 5, 6)),
            ],
        };
        let m = msg.clone();

        let bytes = msg.into_bytes();
        let parsed = RipMessage::from_bytes(&bytes);

        assert_eq!(m, parsed);
    }
}
