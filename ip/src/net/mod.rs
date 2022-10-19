mod args;
mod link;
mod utils;

pub use args::Args;
use link::ProtocolPayload;
pub use link::{Link, LinkDefinition};
use std::ops::Deref;
use std::time::Duration;
use utils::localhost_with_port;

use lazy_static::lazy_static;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::{
    net::UdpSocket,
    sync::{
        broadcast::{self, Receiver, Sender},
        Mutex, RwLock, RwLockReadGuard,
    },
};

use self::link::SendError;

lazy_static! {
    static ref NET: Net = Net::new();
}

pub type Result<T> = core::result::Result<T, Error>;

pub enum Error {
    LinkNotFound,
    LinkInactive,
}

/// Send bytes to a destination.
///
/// The destination is typically the next-hop address for a packet.
pub async fn send(message: ProtocolPayload, dest: Ipv4Addr) -> Result<()> {
    NET.send(message, dest).await
}

/// Turns on a link interface.
pub async fn activate(link_no: u16) -> Result<()> {
    NET.activate_link(link_no).await
}

/// Turns off a link interface.
pub async fn deactivate(link_no: u16) -> Result<()> {
    NET.deactivate_link(link_no).await
}

/// Iterate all links (both active and inactive) for this host.
///
/// This is useful for sending out periodic RIP messages to all links.
pub async fn iter_links<'a>() -> LinkIter<'a> {
    NET.iter_links().await
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
pub async fn listen() -> Receiver<Vec<u8>> {
    let (tx, rx) = broadcast::channel(100);
    rx
}

struct Net {
    links: RwLock<Vec<Link>>,
    listener_sub: Mutex<Option<Sender<Vec<u8>>>>,
}

impl Net {
    fn new() -> Self {
        Self {
            links: RwLock::new(Vec::new()),
            listener_sub: Mutex::new(None),
        }
    }

    async fn init(&self, args: &Args) {
        let udp_socket = Arc::new(
            UdpSocket::bind(localhost_with_port(args.host_port))
                .await
                .expect("Failed to bind to this router's port"),
        );

        let mut links = self.links.write().await;
        for link_def in &args.links {
            links.push(link_def.into_link(udp_socket.clone()));
        }
    }

    async fn send(&self, message: ProtocolPayload, dest: Ipv4Addr) -> Result<()> {
        let links = self.links.read().await;
        match links.iter().find(|l| l.dest() == dest) {
            None => Err(Error::LinkNotFound),
            Some(link) => link.send(message).await.map_err(|e| match e {
                SendError::LinkInactive => Error::LinkInactive,
            }),
        }
    }

    async fn activate_link(&self, link_no: u16) -> Result<()> {
        let mut links = self.links.write().await;
        let link_no = link_no as usize;
        if link_no >= links.len() {
            Err(Error::LinkNotFound)
        } else {
            links[link_no].activate();
            Ok(())
        }
    }

    async fn deactivate_link(&self, link_no: u16) -> Result<()> {
        let mut links = self.links.write().await;
        let link_no = link_no as usize;
        if link_no >= links.len() {
            Err(Error::LinkNotFound)
        } else {
            links[link_no].deactivate();
            Ok(())
        }
    }

    async fn iter_links<'a>(&'a self) -> LinkIter<'a> {
        LinkIter {
            inner: self.links.read().await,
        }
    }

    async fn listen(&self) -> Receiver<Vec<u8>> {
        let mut sub = self.listener_sub.lock().await;
        let links = self.links.read().await;
        if links.is_empty() {
            panic!("cannot listen on an uninitialized network");
        }

        if let Some(ref sub_handle) = *sub {
            return sub_handle.subscribe();
        }

        let (tx, rx) = broadcast::channel(100);
        let sender = tx.clone();
        let sock = links[0].clone_socket();

        tokio::spawn(async move {
            let mut buf = [0; 1024];
            while let Ok(sz) = sock.recv(&mut buf).await {
                sender.send(buf[..sz].into()).unwrap();
            }
        });

        *sub = Some(tx);
        rx
    }
}

pub async fn bootstrap(args: &Args) {
    NET.init(args).await;
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
