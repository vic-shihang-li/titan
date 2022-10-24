use async_trait::async_trait;
use etherparse::Ipv4HeaderSlice;

use crate::{
    net::{self, iter_links, Ipv4PacketBuilder},
    protocol::Protocol,
    route::{get_forwarding_table_mut, Entry as RoutingEntry, ProtocolHandler},
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
    pub fn from_entries_with_poisoned_reverse(
        entries: &[RoutingEntry],
        receiver: Ipv4Addr,
    ) -> Self {
        let cmd = Command::Response;
        let entries: Vec<_> = entries
            .iter()
            .map(|e| {
                let cost = {
                    if e.next_hop() == receiver {
                        RoutingEntry::max_cost()
                    } else {
                        e.cost()
                    }
                };

                Entry::with_default_mask(cost, e.destination())
            })
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
            assert_eq!(
                num_entries, 0,
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

        log::info!("Received RIP packet");

        let sender = header.source_addr();

        if net::find_link_to(sender)
            .await
            .expect("Failed to find the link where RIP packet was sent")
            .is_disabled()
        {
            log::info!("Ignoring RIP packet from {}, link disabled", sender);
            return;
        }

        let mut rt = get_forwarding_table_mut().await;
        let mut updates = Vec::new();

        // RIP protocol implementation.
        // Reference: http://intronetworks.cs.luc.edu/current2/html/routing.html#distance-vector-update-rules
        for entry in &message.entries {
            if net::is_my_addr(entry.address).await {
                log::warn!("Ignoring advertised entry for my address: {:?}", entry);
                continue;
            }

            let entry_cost = cmp::min(entry.cost + 1, RoutingEntry::max_cost());
            match rt.find_mut_entry_for(entry.address) {
                Some(local_entry) => {
                    match entry_cost.cmp(&local_entry.cost()) {
                        Ordering::Less => {
                            log::info!(
                                "Found a cheaper entry; old: {:?}, new: {:?}",
                                local_entry,
                                entry
                            );
                            if local_entry.get_inner_link().await.is_disabled() {
                                log::info!("Update skipped; neighbor is advertising cost for disabled link");
                            } else {
                                local_entry.update(sender, entry_cost);
                                updates.push(*local_entry);
                            }
                        }
                        Ordering::Greater => {
                            if local_entry.next_hop() == sender {
                                log::info!(
                                    "Updating entry cost; old: {:?}, new: {:?}",
                                    local_entry,
                                    entry
                                );
                                local_entry.update_cost(entry_cost);
                                updates.push(*local_entry);
                            } else {
                                log::warn!(
                                    "Ignoring RIP entry with greater cost {:?}, sender: {:?}, local: {:?}",
                                    entry,
                                    sender,
                                    local_entry
                                );
                            }
                        }
                        Ordering::Equal => {
                            // If new cost == old cost, we ignore the new report.
                            // Accepting the new report could destabilize the network (see Ex. 8
                            // in the Chapter 13 of Dordal).
                            if local_entry.next_hop() == sender {
                                log::info!("Restarting timer without update");
                                local_entry.restart_delete_timer();
                            }
                        }
                    }
                }
                None => {
                    log::info!("Adding new entry: {:?}", entry);

                    let dest = entry.address;
                    let entry = RoutingEntry::new(dest, sender, entry_cost);
                    rt.add_entry(entry);
                    updates.push(entry);
                }
            }
        }

        if !updates.is_empty() {
            for link in &*iter_links().await {
                log::info!("Sending triggered update to {}", link.dest());
                let rip_msg_bytes =
                    RipMessage::from_entries_with_poisoned_reverse(&updates, link.dest())
                        .into_bytes();
                let packet = Ipv4PacketBuilder::default()
                    .with_src(link.source())
                    .with_dst(link.dest())
                    .with_payload(&rip_msg_bytes)
                    .with_protocol(Protocol::Rip)
                    .build()
                    .unwrap();
                link.send(&packet).await.ok();
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
