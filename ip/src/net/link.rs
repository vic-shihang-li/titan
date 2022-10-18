use std::net::Ipv4Addr;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The port where the connected host runs.
    pub dest_port: u16,
    /// The virtual IP of this host's interface.
    pub interface_ip: Ipv4Addr,
    /// The virtual IP of the connected host's interface.
    pub dest_ip: Ipv4Addr,
}

#[derive(Debug)]
pub enum ParseLinkError {
    NoIp,
    NoPort,
    NoSrcVirtualIp,
    NoDstVirtualIp,
    MalformedPort,
    MalformedIp,
}

impl Link {
    pub fn try_parse(raw_link: &str) -> Result<Self, ParseLinkError> {
        let mut split = raw_link.split_whitespace();

        split.next().ok_or(ParseLinkError::NoIp)?;

        let dest_port = split
            .next()
            .ok_or(ParseLinkError::NoPort)?
            .parse::<u16>()
            .map_err(|_| ParseLinkError::MalformedPort)?;

        let interface_ip = split
            .next()
            .ok_or(ParseLinkError::NoSrcVirtualIp)?
            .parse()
            .map_err(|_| ParseLinkError::MalformedIp)?;

        let dest_ip = split
            .next()
            .ok_or(ParseLinkError::NoDstVirtualIp)?
            .parse()
            .map_err(|_| ParseLinkError::MalformedIp)?;

        Ok(Link {
            dest_port,
            interface_ip,
            dest_ip,
        })
    }
}

/// Iterate all links (both active and inactive) for this host.
///
/// This is useful for sending out periodic RIP messages to all links.
pub fn iter_links() -> LinkIter {
    todo!()
}

pub struct LinkIter {}

impl Iterator for LinkIter {
    type Item = Link;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}
