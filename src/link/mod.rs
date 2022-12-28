mod args;
mod link;
mod utils;

pub use args::Args;
pub use link::{Link, LinkDefinition};
use std::{
    ops::{Deref, DerefMut},
    usize,
};
use utils::localhost_with_port;
pub use utils::Ipv4PacketBuilder;

use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::{
    net::UdpSocket,
    sync::{
        broadcast::{self, Receiver, Sender},
        Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
};

use self::link::SendError;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    LinkNotFound,
    LinkInactive,
}

impl From<link::SendError> for Error {
    fn from(e: link::SendError) -> Self {
        match e {
            link::SendError::LinkInactive => Error::LinkInactive,
        }
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

pub struct LinkRef<'a> {
    guard: RwLockReadGuard<'a, Vec<Link>>,
    idx: usize,
}

impl<'a> Deref for LinkRef<'a> {
    type Target = Link;
    fn deref(&self) -> &Self::Target {
        &self.guard[self.idx]
    }
}

pub struct LinkMutRef<'a> {
    guard: RwLockWriteGuard<'a, Vec<Link>>,
    idx: usize,
}

impl<'a> Deref for LinkMutRef<'a> {
    type Target = Link;
    fn deref(&self) -> &Self::Target {
        &self.guard[self.idx]
    }
}

impl<'a> DerefMut for LinkMutRef<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard[self.idx]
    }
}

pub struct VtLinkLayer {
    links: Links,
    listener_sub: Mutex<Option<Sender<Vec<u8>>>>,
}

impl VtLinkLayer {
    pub async fn new(args: &Args) -> Self {
        let udp_socket = Arc::new(
            UdpSocket::bind(localhost_with_port(args.host_port))
                .await
                .unwrap_or_else(|_| {
                    panic!("Failed to bind to this router's port: {}", args.host_port)
                }),
        );

        let links = Links::new(
            args.links
                .iter()
                .map(|l| l.into_link(udp_socket.clone()))
                .collect(),
        );

        Self {
            links,
            listener_sub: Mutex::new(None),
        }
    }

    /// Send bytes to a destination.
    ///
    /// The destination is typically the next-hop address for a packet.
    pub async fn send(&self, payload: &[u8], next_hop: Ipv4Addr) -> Result<()> {
        self.links
            .find(|link| link.dest() == next_hop)
            .await
            .ok_or(Error::LinkNotFound)?
            .send(payload)
            .await
            .map_err(|e| match e {
                SendError::LinkInactive => Error::LinkInactive,
            })
    }

    pub async fn activate_link(&self, link_no: u16) -> Result<()> {
        self.links
            .get_mut(link_no)
            .await
            .ok_or(Error::LinkNotFound)?
            .activate()
            .await;
        Ok(())
    }

    pub async fn deactivate_link(&self, link_no: u16) -> Result<()> {
        self.links
            .get_mut(link_no)
            .await
            .ok_or(Error::LinkNotFound)?
            .deactivate()
            .await;
        Ok(())
    }

    #[allow(clippy::needless_lifetimes)]
    pub async fn iter_links<'a>(&'a self) -> LinkIter<'a> {
        self.links.iter().await
    }

    #[allow(clippy::needless_lifetimes)]
    pub async fn find_link_to<'a>(&'a self, dest: Ipv4Addr) -> Option<LinkRef<'a>> {
        self.links.find(|link| link.dest() == dest).await
    }

    #[allow(clippy::needless_lifetimes)]
    pub async fn find_link_with_interface_ip<'a>(&'a self, ip: Ipv4Addr) -> Option<LinkRef<'a>> {
        self.links.find(|link| link.source() == ip).await
    }

    /// Subscribe to a stream of packets received by this host.
    ///
    /// The received data is a packet in its binary format.
    pub async fn listen(&self) -> Receiver<Vec<u8>> {
        let mut sub = self.listener_sub.lock().await;
        if let Some(ref sub_handle) = *sub {
            return sub_handle.subscribe();
        }

        let sock = self
            .links
            .get(0)
            .await
            .expect("cannot listen on an uninitialized network")
            .clone_socket();

        let (tx, rx) = broadcast::channel(100);
        let sender = tx.clone();

        tokio::spawn(async move {
            let mut buf = [0; 65536];
            while let Ok(sz) = sock.recv(&mut buf).await {
                // Note: there seems to be a bug here. We should be able to
                // unwrap send() b/c there should always be a listener, and we
                // do assert that the receiver is running (see Router::run()).
                // However, after running the node for long enough,
                // send().unwrap() panics.
                if sender.send(buf[..sz].into()).is_err() {
                    log::error!("Failed to send packet to receiver");
                }
            }
        });

        *sub = Some(tx);
        rx
    }
}

#[derive(Default)]
struct Links(RwLock<Vec<Link>>);

impl Links {
    fn new(links: Vec<Link>) -> Self {
        Links(RwLock::new(links))
    }

    #[allow(clippy::needless_lifetimes)]
    async fn get<'a>(&'a self, link_no: u16) -> Option<LinkRef<'a>> {
        let links = self.0.read().await;
        let link_no = link_no as usize;
        if link_no >= links.len() {
            None
        } else {
            Some(LinkRef {
                guard: links,
                idx: link_no,
            })
        }
    }

    #[allow(clippy::needless_lifetimes)]
    async fn get_mut<'a>(&'a self, link_no: u16) -> Option<LinkMutRef<'a>> {
        let links = self.0.write().await;
        let link_no = link_no as usize;
        if link_no >= links.len() {
            None
        } else {
            Some(LinkMutRef {
                guard: links,
                idx: link_no,
            })
        }
    }

    #[allow(clippy::needless_lifetimes)]
    async fn find<'a>(&'a self, pred: impl Fn(&Link) -> bool) -> Option<LinkRef<'a>> {
        let links = self.0.read().await;
        let mut idx = 0;

        while idx < links.len() {
            if pred(&links[idx]) {
                break;
            }
            idx += 1;
        }

        if idx == links.len() {
            None
        } else {
            Some(LinkRef { guard: links, idx })
        }
    }

    #[allow(unused)]
    #[allow(clippy::needless_lifetimes)]
    async fn find_mut<'a>(&'a self, pred: impl Fn(&Link) -> bool) -> Option<LinkMutRef<'a>> {
        let links = self.0.write().await;
        let mut idx = 0;

        while idx < links.len() {
            if pred(&links[idx]) {
                break;
            }
            idx += 1;
        }

        if idx == links.len() {
            None
        } else {
            Some(LinkMutRef { guard: links, idx })
        }
    }

    #[allow(clippy::needless_lifetimes)]
    async fn iter<'a>(&'a self) -> LinkIter<'a> {
        LinkIter {
            inner: self.0.read().await,
        }
    }
}
