mod args;
mod link;
mod utils;

pub use args::Args;
use link::{Link, LinkDefinition};
use std::time::Duration;
use utils::localhost_with_port;

use lazy_static::lazy_static;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::{
    net::UdpSocket,
    sync::{broadcast::Receiver, RwLock},
};

lazy_static! {
    static ref LINKS: RwLock<Vec<Link>> = RwLock::new(Vec::new());
}

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

async fn bootstrap_net(args: &Args) {
    let udp_socket = Arc::new(
        UdpSocket::bind(localhost_with_port(args.host_port))
            .await
            .expect("Failed to bind to this router's port"),
    );

    let mut links = LINKS.write().await;
    for link_def in &args.links {
        let sock = udp_socket.clone();
        links.push(link_def.into_link(sock));
    }
}

pub fn bootstrap(args: Args) {
    tokio::spawn(async move {
        bootstrap_net(&args).await;
        tokio::spawn(async {
            send_periodic_updates().await;
        })
    });
}

async fn send_periodic_updates() {
    let interval = Duration::from_secs(5);

    loop {
        let ls = LINKS.read().await;
        for l in ls.iter() {
            // TODO: send periodic update payload
            l.send(&[1, 2, 3, 4]).await;
        }
        drop(ls);
        tokio::time::sleep(interval).await;
    }
}
