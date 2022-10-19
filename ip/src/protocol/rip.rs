use crate::route::ProtocolHandler;

use std::net::Ipv4Addr;

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

impl Into<u8> for Command {
    fn into(self) -> u8 {
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
    BadValue(u8),
}

impl TryFrom<u8> for Command {
    type Error = ParseCommandError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Command::Request),
            2 => Ok(Command::Response),
            _ => Err(ParseCommandError::BadValue(value)),
        }
    }
}

impl Message for RipMessage {
    fn into_bytes(self) -> Vec<u8> {
        let mut v = Vec::new();

        v.push(self.command.into());
        v.push(
            self.entries
                .len()
                .try_into()
                .expect("RIP message has too many entries"),
        );

        for entry in self.entries {
            v.append(&mut entry.into_bytes());
        }

        v
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        assert!(bytes.len() >= 1, "Missing command byte");
        let command = Command::try_from(bytes[0]).expect("Bad command type");

        assert!(bytes.len() >= 2, "Missing num entries byte");
        let num_entries: u8 = bytes[1];

        if command == Command::Request {
            assert!(
                num_entries == 0,
                "request RIP message cannot have any entries"
            );
        }

        assert!(
            bytes.len() >= 2 + num_entries as usize * Entry::serialized_size(),
            "Missing entry bytes"
        );

        let mut entries = Vec::new();
        let mut start = 2;
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

impl ProtocolHandler for RipHandler {
    fn handle_packet(&self, payload: &[u8]) {
        // TODO:
        // 1. update the routing table (what APIs does the routing module need to provide?)
        // 2. send out triggered updates (use `net::iter_links()` and `link.send(rip_message)`)

        let message = RipMessage::from_bytes(payload);
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
