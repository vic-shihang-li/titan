use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cli::{RecvFileError, SendFileError};
use crate::net::vtlink::{self, LinkIter, LinkRef, VtLinkLayer, VtLinkNet, VtLinkNetConfig};
use crate::net::Net;
use crate::protocol::tcp::prelude::{Port, Remote, SocketDescriptor, SocketId};
use crate::protocol::tcp::{
    SocketRef, Tcp, TcpCloseError, TcpConn, TcpConnError, TcpHandler, TcpListenError, TcpListener,
    TcpReadError, TcpSendError,
};
use crate::protocol::{Protocol, ProtocolHandler};
use crate::{net, Args};
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

        let links = Arc::new(VtLinkLayer::new(self.args).await);
        let net = Arc::new(VtLinkNet::new(
            links,
            self.args,
            VtLinkNetConfig {
                prune_interval: self.prune_interval,
                rip_update_interval: self.rip_update_interval,
                entry_max_age: self.entry_max_age,
                drop_factor: self.drop_factor,
            },
        ));
        let tcp = Arc::new(Tcp::new(net.clone()));

        self.with_protocol_handler(Protocol::Tcp, TcpHandler::new(tcp.clone()));

        let mut protocol_handlers = HashMap::new();
        self.protocol_handlers.drain().for_each(|(proto, handler)| {
            protocol_handlers.insert(proto, handler);
        });

        Node {
            tcp,
            net,
            protocol_handlers,
        }
    }
}

pub struct Node {
    tcp: Arc<Tcp<VtLinkNet>>,
    net: Arc<VtLinkNet>,
    protocol_handlers: HashMap<Protocol, Box<dyn ProtocolHandler>>,
}

impl Node {
    pub async fn find_link_to(&self, next_hop: Ipv4Addr) -> Option<LinkRef<'_>> {
        self.net.links().find_link_to(next_hop).await
    }

    pub async fn find_link_with_interface_ip(&self, ip: Ipv4Addr) -> Option<LinkRef<'_>> {
        self.net.links().find_link_with_interface_ip(ip).await
    }

    /// Turns on a link interface.
    pub async fn activate(&self, link_no: u16) -> Result<(), vtlink::Error> {
        self.net.links().activate_link(link_no).await
    }

    /// Turns off a link interface.
    pub async fn deactivate(&self, link_no: u16) -> Result<(), vtlink::Error> {
        self.net.links().deactivate_link(link_no).await
    }

    pub async fn is_my_addr(&self, addr: Ipv4Addr) -> bool {
        self.net.is_my_addr(addr)
    }

    /// Iterate all links (both active and inactive) for this host.
    ///
    /// This is useful for sending out periodic RIP messages to all links.
    pub async fn iter_links(&self) -> LinkIter<'_> {
        self.net.links().iter_links().await
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
    pub async fn send<P: Into<u8> + Send>(
        &self,
        payload: &[u8],
        protocol: P,
        dest_vip: Ipv4Addr,
    ) -> Result<(), net::SendError> {
        self.net.send(payload, protocol, dest_vip).await
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

    pub async fn get_socket(&self, socket_id: SocketId) -> Option<SocketRef<'_, VtLinkNet>> {
        self.tcp.get_socket(socket_id).await
    }

    pub async fn get_socket_by_descriptor(
        &self,
        socket_descriptor: SocketDescriptor,
    ) -> Option<SocketRef<'_, VtLinkNet>> {
        self.tcp.get_socket_by_descriptor(socket_descriptor).await
    }

    pub async fn get_socket_descriptor(&self, socket_id: SocketId) -> Option<SocketDescriptor> {
        self.tcp.get_socket_descriptor(socket_id).await
    }

    pub async fn run(&self) {
        self.net.run(&self.protocol_handlers).await;
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

    pub async fn send_file(&self, path: &str, remote: Remote) -> Result<(), SendFileError> {
        let mut f = File::open(path).await.map_err(SendFileError::OpenFile)?;

        let mut input = Vec::new();
        f.read_to_end(&mut input)
            .await
            .map_err(SendFileError::ReadFile)?;

        self.connect_and_send_bytes(remote, &input).await
    }

    pub async fn recv_file(&self, out_path: &str, port: Port) -> Result<(), RecvFileError> {
        let received = self.listen_and_recv_bytes(port).await?;

        let mut out_file = File::create(out_path).await?;
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
