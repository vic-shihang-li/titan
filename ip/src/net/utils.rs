use std::net::{Ipv4Addr, SocketAddr};

pub fn localhost_with_port(port: u16) -> SocketAddr {
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port)
}
