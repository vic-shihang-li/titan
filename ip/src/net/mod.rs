mod args;
mod link;
mod utils;

pub use args::Args;
use link::{Link, LinkDefinition};
use std::time::Duration;
use utils::localhost_with_port;

use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::{
    net::UdpSocket,
    sync::{broadcast::Receiver, RwLock},
};

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
    tokio::spawn(async move {
        let net = Net::bootstrap(&args).await;
        net.run_till_exit().await;
    });
}

/// Main struct for orchestrating a router's interfaces.
struct Net {
    sock: Arc<UdpSocket>,
    port: u16,
    links: Arc<RwLock<Vec<Link>>>,
}

impl Net {
    async fn bootstrap(args: &Args) -> Self {
        let udp_socket = Arc::new(
            UdpSocket::bind(localhost_with_port(args.host_port))
                .await
                .expect("Failed to bind to this router's port"),
        );

        let mut links = Vec::new();
        for link_def in &args.links {
            let sock = udp_socket.clone();
            links.push(link_def.into_link(sock));
        }

        Self {
            port: args.host_port,
            sock: udp_socket,
            links: Arc::new(RwLock::new(links)),
        }
    }
}

impl Net {
    async fn run_till_exit(&self) {
        let links = self.links.clone();
        tokio::spawn(async move {
            send_periodic_updates(links).await;
        });
    }
}

async fn send_periodic_updates(links: Arc<RwLock<Vec<Link>>>) {
    let interval = Duration::from_secs(5);

    loop {
        let ls = links.read().await;
        for l in ls.iter() {
            // TODO: send periodic update payload
            l.send(&[1, 2, 3, 4]).await;
        }
        drop(ls);
        tokio::time::sleep(interval).await;
    }
}
