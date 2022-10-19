mod args;
mod link;
mod utils;

pub use args::Args;
use link::{Link, LinkDefinition};
use std::ops::Deref;
use std::time::Duration;
use utils::localhost_with_port;

use lazy_static::{__Deref, lazy_static};
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::{
    net::UdpSocket,
    sync::{broadcast::Receiver, RwLock, RwLockReadGuard},
};

lazy_static! {
    static ref LINKS: RwLock<Vec<Link>> = RwLock::new(Vec::new());
}

pub type Result<T> = core::result::Result<T, Error>;

pub enum Error {
    LinkNotFound,
}

/// Send bytes to a destination.
///
/// The destination is typically the next-hop address for a packet.
pub fn send(bytes: &[u8], dest: Ipv4Addr) {
    todo!()
}

/// Turns on a link interface.
pub async fn activate(link_no: u16) -> Result<()> {
    let mut links = LINKS.write().await;
    let link_no = link_no as usize;
    if link_no >= links.len() {
        Err(Error::LinkNotFound)
    } else {
        links[link_no].deactivate();
        Ok(())
    }
}

/// Turns off a link interface.
pub async fn deactivate(link_no: u16) -> Result<()> {
    let mut links = LINKS.write().await;
    let link_no = link_no as usize;
    if link_no >= links.len() {
        Err(Error::LinkNotFound)
    } else {
        links[link_no].activate();
        Ok(())
    }
}

/// Iterate all links (both active and inactive) for this host.
///
/// This is useful for sending out periodic RIP messages to all links.
pub async fn iter_links<'a>() -> LinkIter<'a> {
    LinkIter {
        inner: LINKS.read().await,
    }
}

pub struct LinkIter<'a> {
    inner: RwLockReadGuard<'a, Vec<Link>>,
}

impl<'a> Deref for LinkIter<'a> {
    type Target = Vec<Link>;
    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
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
        });
    });
}

async fn send_periodic_updates() {
    let interval = Duration::from_secs(5);

    loop {
        for link in &*iter_links().await {
            // TODO: send periodic update payload
        }
        tokio::time::sleep(interval).await;
    }
}
