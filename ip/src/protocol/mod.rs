pub mod rip;
pub mod test;

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

#[allow(clippy::from_over_into)]
impl Into<u8> for Protocol {
    fn into(self) -> u8 {
        match self {
            Protocol::Rip => 200,
            Protocol::Test => 0,
        }
    }
}
