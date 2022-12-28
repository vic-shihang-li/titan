mod args;

pub use args::Args;
use core::fmt;
use std::{
    ops::{Deref, DerefMut},
    usize,
};

use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::{
    net::UdpSocket,
    sync::{
        broadcast::{self, Receiver, Sender},
        Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
};

use crate::utils::net::localhost_with_port;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    LinkNotFound,
    LinkInactive,
}

impl From<SendError> for Error {
    fn from(e: SendError) -> Self {
        match e {
            SendError::LinkInactive => Error::LinkInactive,
        }
    }
}

pub struct LinkIter<'a> {
    inner: RwLockReadGuard<'a, Vec<Link>>,
}

impl<'a> Deref for LinkIter<'a> {
    type Target = Vec<Link>;
    fn deref(&self) -> &Self::Target {
        &self.inner
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

    pub async fn iter_links(&self) -> LinkIter<'_> {
        self.links.iter().await
    }

    pub async fn find_link_to(&self, dest: Ipv4Addr) -> Option<LinkRef<'_>> {
        self.links.find(|link| link.dest() == dest).await
    }

    pub async fn find_link_with_interface_ip(&self, ip: Ipv4Addr) -> Option<LinkRef<'_>> {
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

    async fn get(&self, link_no: u16) -> Option<LinkRef<'_>> {
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

    async fn get_mut(&self, link_no: u16) -> Option<LinkMutRef<'_>> {
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

    async fn find(&self, pred: impl Fn(&Link) -> bool) -> Option<LinkRef<'_>> {
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
    async fn find_mut(&self, pred: impl Fn(&Link) -> bool) -> Option<LinkMutRef<'_>> {
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

    async fn iter(&self) -> LinkIter<'_> {
        LinkIter {
            inner: self.0.read().await,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LinkDefinition {
    /// The port where the connected host runs.
    pub dest_port: u16,
    /// The virtual IP of this host's interface.
    pub interface_ip: Ipv4Addr,
    /// The virtual IP of the connected host's interface.
    pub dest_ip: Ipv4Addr,
}

pub struct Link {
    dest_port: u16,
    dest_virtual_ip: Ipv4Addr,
    src_virtual_ip: Ipv4Addr,
    activated: bool,
    sock: Arc<UdpSocket>,
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

impl LinkDefinition {
    pub fn try_parse(raw_link: &str) -> std::result::Result<Self, ParseLinkError> {
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

        Ok(LinkDefinition {
            dest_port,
            interface_ip,
            dest_ip,
        })
    }

    pub fn into_link(self, udp_socket: Arc<UdpSocket>) -> Link {
        Link {
            dest_port: self.dest_port,
            dest_virtual_ip: self.dest_ip,
            src_virtual_ip: self.interface_ip,
            activated: true,
            sock: udp_socket,
        }
    }
}

#[derive(Debug)]
pub enum SendError {
    LinkInactive,
}

impl Link {
    /// On this link, send a message conforming to one of the supported protocols.
    pub async fn send(&self, payload: &[u8]) -> std::result::Result<(), SendError> {
        if !self.activated {
            return Err(SendError::LinkInactive);
        }

        self.sock
            .send_to(payload, localhost_with_port(self.dest_port))
            .await
            .unwrap();

        Ok(())
    }

    pub async fn activate(&mut self) {
        if !self.activated {
            self.activated = true;
        }
    }

    pub async fn deactivate(&mut self) {
        if self.activated {
            self.activated = false;
        }
    }

    pub fn is_disabled(&self) -> bool {
        !self.activated
    }

    pub fn dest(&self) -> Ipv4Addr {
        self.dest_virtual_ip
    }

    pub fn source(&self) -> Ipv4Addr {
        self.src_virtual_ip
    }

    pub fn clone_socket(&self) -> Arc<UdpSocket> {
        self.sock.clone()
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = if self.activated { "up" } else { "down" };
        write!(
            f,
            "{}\t{}\t{}\t{}",
            state, self.src_virtual_ip, self.dest_virtual_ip, self.dest_port
        )
    }
}
