pub mod rip;
pub mod test;

use crate::rip::RipMessage;
use crate::Message;

#[derive(PartialEq, Eq, Hash, Debug)]
pub enum Protocol {
    Rip,
    Test,
}

pub enum ParseProtocolError {
    Unsupported,
}

impl TryFrom<u8> for Protocol {
    type Error = ParseProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Protocol::Test),
            200 => Ok(Protocol::Rip),
            _ => Err(ParseProtocolError::Unsupported),
        }
    }
}

impl Into<u8> for Protocol {
    fn into(self) -> u8 {
        match self {
            Protocol::Rip => 200,
            Protocol::Test => 0,
        }
    }
}

pub enum ProtocolPayload {
    Rip(RipMessage),
    Test(String),
}

impl ProtocolPayload {
    pub fn into_bytes(self) -> (u8, Vec<u8>) {
        match self {
            ProtocolPayload::Rip(msg) => (Protocol::Rip.into(), msg.into_bytes()),
            ProtocolPayload::Test(s) => (Protocol::Test.into(), s.into_bytes()),
        }
    }
}
