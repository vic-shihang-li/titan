mod args;
mod link;

pub use args::Args;
use link::Link;

use std::net::Ipv4Addr;
use tokio::sync::broadcast::Receiver;

/// Send bytes to a destination.
///
/// The destination is typically the next-hop address for a packet.
pub fn send(bytes: &[u8], dest: Ipv4Addr) {
    todo!()
}

/// Turns on a link interface.
pub fn activate(link_no: u16) {
    todo!()
}

/// Turns off a link interface.
pub fn deactivate(link_no: u16) {
    todo!()
}

/// Subscribe to a stream of packets received by this host.
///
/// The received data is a packet in its binary format.
pub fn listen() -> Receiver<Vec<u8>> {
    todo!()
}

pub fn bootstrap(args: Args) {
    let net = Net::from(args);
    tokio::spawn(async move {
        net.run().await;
    });
}

/// Main struct for orchestrating a router's interfaces.
struct Net {
    links: Vec<Link>,
}

impl From<Args> for Net {
    fn from(args: Args) -> Self {
        Self { links: args.links }
    }
}

impl Net {
    async fn run(&self) {}
}
