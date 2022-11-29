use etherparse::{InternetSlice, Ipv4HeaderSlice, SlicedPacket};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{RwLockReadGuard, RwLockWriteGuard};

use crate::cli::{RecvFileCmd, RecvFileError, SendFileCmd, SendFileError};
use crate::net::{self, LinkIter, LinkRef, Net};
use crate::protocol::tcp::{
    Port, Remote, SocketDescriptor, SocketId, SocketRef, Tcp, TcpCloseError, TcpConn, TcpConnError,
    TcpHandler, TcpListenError, TcpListener, TcpReadError, TcpSendError,
};
use crate::protocol::{Protocol, ProtocolHandler};
use crate::route::{self, ForwardingTable, PacketDecision, Router, RouterConfig};
use crate::Args;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

pub struct NodeBuilder<'a> {
    args: &'a Args,
    built: bool,
    prune_interval: Duration,
    rip_update_interval: Duration,
    drop_factor: usize,
    entry_max_age: Duration,
    protocol_handlers: HashMap<Protocol, Box<dyn ProtocolHandler>>,
}

impl<'a> NodeBuilder<'a> {
    pub fn new(args: &'a Args) -> Self {
        let drop_factor = if args.lossy { 5 } else { 0 };

        Self {
            args,
            built: false,
            prune_interval: Duration::from_secs(1),
            rip_update_interval: Duration::from_secs(5),
            entry_max_age: Duration::from_secs(12),
            drop_factor,
            protocol_handlers: HashMap::new(),
        }
    }

    /// Set the interval of sending up periodic RIP updates.
    pub fn with_rip_interval(&mut self, rip_interval: Duration) -> &mut Self {
        self.rip_update_interval = rip_interval;
        self
    }

    /// Set the maximum time a routing entry can live without receiving an update.
    pub fn with_entry_max_age(&mut self, max_age: Duration) -> &mut Self {
        self.entry_max_age = max_age;
        self
    }

    /// Set how often routing entries are pruned.
    pub fn with_prune_interval(&mut self, prune_interval: Duration) -> &mut Self {
        self.prune_interval = prune_interval;
        self
    }

    /// Set how often packets are dropped.
    ///
    /// With a drop factor of N, 1 packet is dropped every N packets.
    ///
    /// A drop factor of 0 drops no packets.
    pub fn with_drop_factor(&mut self, drop_factor: usize) -> &mut Self {
        self.drop_factor = drop_factor;
        self
    }

    /// Provide a handler for a protocol.
    ///
    /// Replaces any handler that is associated with the protocol.
    pub fn with_protocol_handler<H: ProtocolHandler + 'static>(
        &mut self,
        protocol: Protocol,
        handler: H,
    ) -> &mut Self {
        self.protocol_handlers.insert(protocol, Box::new(handler));
        self
    }

    pub async fn build(&mut self) -> Node {
        if self.built {
            panic!("A NodeBuilder can only be built once.");
        }
        self.built = true;

        let net = Arc::new(Net::new(self.args).await);
        let router = Arc::new(Router::new(
            net.clone(),
            self.args,
            RouterConfig {
                prune_interval: self.prune_interval,
                rip_update_interval: self.rip_update_interval,
                entry_max_age: self.entry_max_age,
                drop_factor: self.drop_factor,
            },
        ));
        let tcp = Arc::new(Tcp::new(router.clone()));

        self.with_protocol_handler(Protocol::Tcp, TcpHandler::new(tcp.clone()));

        let mut protocol_handlers = HashMap::new();
        self.protocol_handlers.drain().for_each(|(proto, handler)| {
            protocol_handlers.insert(proto, handler);
        });

        Node {
            net,
            tcp,
            router,
            protocol_handlers,
        }
    }
}

pub struct Node {
    net: Arc<Net>,
    tcp: Arc<Tcp>,
    router: Arc<Router>,
    protocol_handlers: HashMap<Protocol, Box<dyn ProtocolHandler>>,
}

impl Node {
    #[allow(clippy::needless_lifetimes)]
    pub async fn find_link_to<'a>(&'a self, next_hop: Ipv4Addr) -> Option<LinkRef<'a>> {
        self.net.find_link_to(next_hop).await
    }

    #[allow(clippy::needless_lifetimes)]
    pub async fn find_link_with_interface_ip<'a>(&'a self, ip: Ipv4Addr) -> Option<LinkRef<'a>> {
        self.net.find_link_with_interface_ip(ip).await
    }

    /// Turns on a link interface.
    pub async fn activate(&self, link_no: u16) -> Result<(), net::Error> {
        self.net.activate_link(link_no).await
    }

    /// Turns off a link interface.
    pub async fn deactivate(&self, link_no: u16) -> Result<(), net::Error> {
        self.net.deactivate_link(link_no).await
    }

    pub async fn is_my_addr(&self, addr: Ipv4Addr) -> bool {
        self.router.is_my_addr(addr)
    }

    /// Iterate all links (both active and inactive) for this host.
    ///
    /// This is useful for sending out periodic RIP messages to all links.
    #[allow(clippy::needless_lifetimes)]
    pub async fn iter_links<'a>(&'a self) -> LinkIter<'a> {
        self.net.iter_links().await
    }

    pub async fn close_socket(&self, socket_id: SocketId) -> Result<(), TcpCloseError> {
        self.tcp.close(socket_id).await
    }

    pub async fn close_socket_by_descriptor(
        &self,
        socket_descriptor: SocketDescriptor,
    ) -> Result<(), TcpCloseError> {
        self.tcp.close_by_descriptor(socket_descriptor).await
    }

    /// Send bytes to a destination.
    ///
    /// The destination is typically the next-hop address for a packet.
    pub async fn send<P: Into<u8>>(
        &self,
        payload: &[u8],
        protocol: P,
        dest_vip: Ipv4Addr,
    ) -> Result<(), route::SendError> {
        self.router.send(payload, protocol, dest_vip).await
    }

    /// Send bytes over a TCP connection.
    pub async fn tcp_send(
        &self,
        socket_descriptor: SocketDescriptor,
        payload: &[u8],
    ) -> Result<(), TcpSendError> {
        self.tcp
            .send_on_socket_descriptor(socket_descriptor, payload)
            .await
    }

    /// Read some bytes over a TCP connection.
    pub async fn tcp_read(
        &self,
        socket_descriptor: SocketDescriptor,
        n_bytes: usize,
    ) -> Result<Vec<u8>, TcpReadError> {
        self.tcp
            .read_on_socket_descriptor(socket_descriptor, n_bytes)
            .await
    }

    pub async fn get_socket(&self, socket_id: SocketId) -> Option<SocketRef<'_>> {
        self.tcp.get_socket(socket_id).await
    }

    pub async fn get_socket_by_descriptor(
        &self,
        socket_descriptor: SocketDescriptor,
    ) -> Option<SocketRef<'_>> {
        self.tcp.get_socket_by_descriptor(socket_descriptor).await
    }

    pub async fn get_socket_descriptor(&self, socket_id: SocketId) -> Option<SocketDescriptor> {
        self.tcp.get_socket_descriptor(socket_id).await
    }

    pub async fn run(&self) {
        let mut listener = self.net.listen().await;
        while let Ok(bytes) = listener.recv().await {
            // 0. parse bytes to packet
            // 1. drop if packet is not valid or TTL = 0
            // 2. if packet is for "me", pass packet to the correct protocol handler
            // 3. if forwarding table has rule for packet, send to the next-hop interface

            self.handle_packet_bytes(&bytes).await;
        }
    }

    #[allow(clippy::needless_lifetimes)]
    pub async fn get_forwarding_table_mut<'a>(&'a self) -> RwLockWriteGuard<'a, ForwardingTable> {
        self.router.get_forwarding_table_mut().await
    }

    #[allow(clippy::needless_lifetimes)]
    pub async fn get_forwarding_table<'a>(&'a self) -> RwLockReadGuard<'a, ForwardingTable> {
        self.router.get_forwarding_table().await
    }

    pub async fn connect(
        &self,
        dest_ip: Ipv4Addr,
        dest_port: Port,
    ) -> Result<TcpConn, TcpConnError> {
        self.tcp.connect(Remote::new(dest_ip, dest_port)).await
    }

    pub async fn listen(&self, port: Port) -> Result<TcpListener, TcpListenError> {
        self.tcp.listen(port).await
    }

    pub async fn send_file(&self, cmd: SendFileCmd) -> Result<(), SendFileError> {
        let mut f = File::open(&cmd.path)
            .await
            .map_err(SendFileError::OpenFile)?;

        let mut input = Vec::new();
        f.read_to_end(&mut input)
            .await
            .map_err(SendFileError::ReadFile)?;

        self.connect_and_send_bytes(Remote::new(cmd.dest_ip, cmd.port), &input)
            .await
    }

    pub async fn recv_file(&self, cmd: RecvFileCmd) -> Result<(), RecvFileError> {
        let received = self.listen_and_recv_bytes(cmd.port).await?;

        let mut out_file = File::create(cmd.out_path).await?;
        out_file.write_all(&received).await?;

        Ok(())
    }

    pub async fn connect_and_send_bytes(
        &self,
        remote: Remote,
        bytes: &[u8],
    ) -> Result<(), SendFileError> {
        let conn = self
            .connect(remote.ip(), remote.port())
            .await
            .map_err(SendFileError::Connect)?;

        conn.send_all(bytes).await.map_err(SendFileError::Send)?;

        self.close_socket(conn.socket_id())
            .await
            .expect("Socket should be open");

        Ok(())
    }

    pub async fn listen_and_recv_bytes(&self, port: Port) -> Result<Vec<u8>, RecvFileError> {
        let mut listener = self.tcp.listen(port).await.map_err(RecvFileError::Listen)?;
        let socket = listener.accept().await.map_err(RecvFileError::Accept)?;
        Ok(socket.read_till_closed().await)
    }

    pub async fn print_sockets(&self, file: Option<String>) {
        self.tcp.print_sockets(file).await
    }
}

impl Node {
    async fn handle_packet_bytes(&self, bytes: &[u8]) {
        match SlicedPacket::from_ip(bytes) {
            Err(value) => eprintln!("Err {:?}", value),
            Ok(packet) => {
                if packet.ip.is_none() {
                    eprintln!("Packet has no IP fields");
                    return;
                }

                let ip = packet.ip.unwrap();

                match ip {
                    InternetSlice::Ipv4(header, _) => {
                        let ipv4_header_len = 20;
                        let payload = &bytes[ipv4_header_len..];
                        self.handle_packet(&header, payload).await;
                    }
                    InternetSlice::Ipv6(_, _) => eprintln!("Unsupported IPV6 packet"),
                };
            }
        }
    }

    async fn handle_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, payload: &[u8]) {
        match self.router.decide_packet(header).await {
            PacketDecision::Drop => {}
            PacketDecision::Consume => self.consume_packet(header, payload).await,
            PacketDecision::Forward => self.router.forward_packet(header, payload).await,
        }
    }

    async fn consume_packet<'a>(&self, header: &Ipv4HeaderSlice<'a>, payload: &[u8]) {
        match header.protocol().try_into() {
            Ok(protocol) => match self.protocol_handlers.get(&protocol) {
                Some(handler) => {
                    handler
                        .handle_packet(header, payload, &self.router, &self.net)
                        .await;
                }
                None => eprintln!("Warning: no protocol handler for protocol {:?}", protocol),
            },
            Err(_) => eprintln!("Unrecognized protocol {}", header.protocol()),
        }
    }
}
