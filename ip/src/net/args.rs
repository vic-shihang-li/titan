use std::{
    fmt::Display,
    fs::File,
    io::{BufRead, BufReader},
    net::Ipv4Addr,
};

use crate::net::link::{LinkDefinition, ParseLinkError};

/// Input to a router; used to establish a router's interfaces.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Args {
    /// The port where this host runs.
    pub host_port: u16,
    /// A list of the router's interfaces, ordered by their interface ID number.
    pub links: Vec<LinkDefinition>,
}

#[derive(Debug)]
pub enum ParseArgsError {
    MissingFirstLine,
    NoHost,
    NoPort,
    NoLinks,
    MalformedPort,
    MalformedLink(ParseLinkError),
    ReadLineError(std::io::Error),
    OpenLinkFileError(std::io::Error),
    MissingLinkFileArg,
}

impl Args {
    pub fn try_parse<B>(reader: B) -> Result<Args, ParseArgsError>
    where
        B: BufRead,
    {
        let mut lines = reader.lines();
        let host_ip_port = lines
            .next()
            .ok_or(ParseArgsError::MissingFirstLine)?
            .map_err(ParseArgsError::ReadLineError)?;

        let mut ip_port = host_ip_port.split_whitespace();
        // ignored: assume localhost
        let _ip = ip_port.next().ok_or(ParseArgsError::NoHost)?;
        let port = ip_port
            .next()
            .ok_or(ParseArgsError::NoPort)?
            .parse::<u16>()
            .map_err(|_| ParseArgsError::MalformedPort)?;

        let mut links = Vec::new();
        for line in lines {
            let raw_link = line.map_err(ParseArgsError::ReadLineError)?;
            links.push(
                LinkDefinition::try_parse(raw_link.as_str())
                    .map_err(ParseArgsError::MalformedLink)?,
            );
        }

        if links.is_empty() {
            return Err(ParseArgsError::NoLinks);
        }

        Ok(Args {
            host_port: port,
            links,
        })
    }

    pub fn get_my_interface_ips(&self) -> Vec<Ipv4Addr> {
        self.links.iter().map(|l| l.interface_ip).collect()
    }
}

impl TryFrom<std::env::Args> for Args {
    type Error = ParseArgsError;

    fn try_from(mut args: std::env::Args) -> Result<Self, Self::Error> {
        if args.len() < 2 {
            return Err(ParseArgsError::MissingLinkFileArg);
        }

        let link_file_path = args.nth(1).unwrap();
        let br =
            BufReader::new(File::open(link_file_path).map_err(ParseArgsError::OpenLinkFileError)?);

        Args::try_parse(br)
    }
}

impl Display for Args {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Running on port {}", self.host_port)?;
        for (lnk_no, lnk) in self.links.iter().enumerate() {
            write!(f, "\n{}:{}", lnk_no, lnk.dest_ip)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn parse_link_file() {
        let link_file_path = "../net_links/abc/B.lnx";
        let br = BufReader::new(File::open(link_file_path).unwrap());
        let args = Args::try_parse(br).unwrap();

        assert_eq!(
            args,
            Args {
                host_port: 5001,
                links: vec![
                    LinkDefinition {
                        dest_port: 5000,
                        interface_ip: Ipv4Addr::new(192, 168, 0, 2),
                        dest_ip: Ipv4Addr::new(192, 168, 0, 1)
                    },
                    LinkDefinition {
                        dest_port: 5002,
                        interface_ip: Ipv4Addr::new(192, 168, 0, 3),
                        dest_ip: Ipv4Addr::new(192, 168, 0, 4)
                    }
                ]
            }
        )
    }
}
