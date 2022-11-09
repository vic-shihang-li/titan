// TODO: remove this once the rest of TCP is implemented
#[allow(dead_code)]
mod buf;
mod socket;

use std::collections::HashMap;
use std::hash::Hash;
use std::usize;
use std::{net::Ipv4Addr, sync::Arc};

use crate::{net::Net, protocol::ProtocolHandler, route::Router};
use async_trait::async_trait;
use etherparse::{Ipv4HeaderSlice, TcpHeaderSlice};
use socket::Socket;
pub use socket::{TcpConn, TcpListener};
use tokio::sync::RwLock;

pub const TCP_DEFAULT_WINDOW_SZ: usize = 1 << 16;

#[derive(Debug)]
pub enum TcpConnError {
    PortOccupied(Port),
}

#[derive(Debug)]
pub struct TcpListenError {}

#[derive(Debug)]
pub struct TcpAcceptError {}

#[derive(Debug)]
pub struct TcpSendError {}

#[derive(Debug)]
pub struct TcpReadError {}

/// A TCP stack.
pub struct Tcp {
    sockets: RwLock<SocketTable>,
    router: Arc<Router>,
    // a concurrent data structure holding Tcp stack states
}

impl Tcp {
    pub fn new(router: Arc<Router>) -> Self {
        let sockets = RwLock::new(SocketTable::new(router.clone()));
        Tcp { router, sockets }
    }

    /// Attempts to connect to a host, establishing the client side of a TCP connection.
    pub async fn connect(&self, dest_ip: Ipv4Addr, port: Port) -> Result<TcpConn, TcpConnError> {
        let mut sockets = self.sockets.write().await;
        let socket = sockets.add_new_socket(port).map_err(|e| match e {
            AddSocketError::PortOccupied => TcpConnError::PortOccupied(port),
        })?;

        let on_connected = socket
            .initiate_connection(dest_ip, port)
            .await
            .expect("Failed to send SYN packet");
        drop(sockets);

        let tcp_conn = on_connected.await.unwrap();

        Ok(tcp_conn)
    }

    /// Starts listening for incoming connections at a port. Opens a listener socket.
    pub async fn listen(&self, port: u16) -> Result<TcpListener, TcpListenError> {
        // TODO: create Tcp machine that starts with LISTEN state. Open listen socket.

        todo!()
    }
}

#[derive(Hash, PartialEq, Eq, Debug, Copy, Clone)]
pub struct SocketId(u16);

#[derive(Hash, PartialEq, Eq, Debug, Copy, Clone)]
pub struct Port(u16);

enum AddSocketError {
    PortOccupied,
}

struct SocketTable {
    socket_id_map: HashMap<SocketId, Port>,
    socket_map: HashMap<Port, Socket>,
    socket_builder: SocketBuilder,
}

impl SocketTable {
    pub fn new(router: Arc<Router>) -> Self {
        Self {
            socket_builder: SocketBuilder::new(router),
            socket_id_map: HashMap::new(),
            socket_map: HashMap::new(),
        }
    }
    pub fn add_new_socket(&mut self, port: Port) -> Result<&mut Socket, AddSocketError> {
        let socket = self.socket_builder.make_with_port(port);
        let socket_id = socket.id();

        let sock_ref = self
            .socket_map
            .try_insert(port, socket)
            .map_err(|_| AddSocketError::PortOccupied)?;

        self.socket_id_map
            .try_insert(socket_id, port)
            .expect("Found duplicate socket ID");

        Ok(sock_ref)
    }

    pub fn get_socket_by_port(&self, port: Port) -> Option<&Socket> {
        self.socket_map.get(&port)
    }

    pub fn get_socket_by_id(&self, id: SocketId) -> Option<&Socket> {
        self.socket_id_map
            .get(&id)
            .and_then(|port| self.socket_map.get(port))
    }

    pub fn get_mut_socket_by_port(&mut self, port: Port) -> Option<&mut Socket> {
        self.socket_map.get_mut(&port)
    }

    pub fn get_mut_socket_by_id(&mut self, id: SocketId) -> Option<&mut Socket> {
        self.socket_id_map
            .get(&id)
            .and_then(|port| self.socket_map.get_mut(port))
    }
}

struct SocketBuilder {
    next_socket_id: usize,
    router: Arc<Router>,
}

impl SocketBuilder {
    fn new(router: Arc<Router>) -> Self {
        Self {
            router,
            next_socket_id: 0,
        }
    }

    fn make_with_port(&mut self, port: Port) -> Socket {
        let sid = SocketId(self.next_socket_id.try_into().expect("Socket ID overflow"));
        let sock = Socket::new(sid, port, self.router.clone());
        self.next_socket_id += 1;
        sock
    }
}

pub struct TcpHandler {
    tcp: Arc<Tcp>,
}

impl TcpHandler {
    pub fn new(tcp: Arc<Tcp>) -> Self {
        Self { tcp }
    }
}

#[async_trait]
impl ProtocolHandler for TcpHandler {
    async fn handle_packet<'a>(
        &self,
        header: &Ipv4HeaderSlice<'a>,
        payload: &[u8],
        _router: &Router,
        _net: &Net,
    ) {
        // Step 1: validate checksum
        let h = TcpHeaderSlice::from_slice(payload).unwrap();
        let dst_port = Port(h.destination_port());
        let checksum = h.checksum();
        if checksum != h.calc_checksum_ipv4(header, payload).unwrap() {
            eprintln!("TCP checksum failed");
        }

        // Step 2: find the corresponding TCP socket
        let mut sockets = self.tcp.sockets.write().await;
        let socket = sockets.get_mut_socket_by_port(dst_port).unwrap();
        let tcp_payload = &payload[h.slice().len()..];

        // Step 3: let socket handle TCP packet
        socket.handle_packet(header, &h, tcp_payload).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use tokio::sync::Barrier;

    use crate::{
        node::{Node, NodeBuilder},
        protocol::Protocol,
        Args,
    };

    use super::TcpHandler;

    #[tokio::test]
    async fn hello_world() {
        // A minimal test case that establishes TCP connection and sends some bytes.

        let send_cfg = crate::fixture::netlinks::abc::A.clone();
        let recv_cfg = crate::fixture::netlinks::abc::B.clone();
        let payload = "hello world!";
        let recv_listen_port = 5656;
        let barr = Arc::new(Barrier::new(2));

        let listen_barr = barr.clone();
        let sender_cfg = send_cfg.clone();
        let receiver_cfg = recv_cfg.clone();
        let sender = tokio::spawn(async move {
            let node = create_and_start_node(sender_cfg).await;
            listen_barr.wait().await;

            let dest_ip = {
                let recv_ips = receiver_cfg.get_my_interface_ips();
                recv_ips[0]
            };
            let conn = node.connect(dest_ip, recv_listen_port).await.unwrap();
            conn.send_all(payload.to_string().as_bytes()).await.unwrap();
        });

        let receiver = tokio::spawn(async move {
            let node = create_and_start_node(recv_cfg).await;

            let listener = node.listen(recv_listen_port).await.unwrap();
            let conn = listener.accept().await.unwrap();

            barr.wait().await;

            let mut buf = [0; 12];
            conn.read_all(&mut buf).await.unwrap();
            assert_eq!(String::from_utf8(buf.into()).unwrap(), payload.to_string());
        });

        sender.await.unwrap();
        receiver.await.unwrap();
    }

    async fn create_and_start_node(cfg: Args) -> Arc<Node> {
        let tcp_stack = Arc::new(Tcp::default());
        let node = Arc::new(
            NodeBuilder::new(&cfg, tcp_stack.clone())
                .with_protocol_handler(Protocol::Tcp, TcpHandler::new(tcp_stack))
                .build()
                .await,
        );
        let node_runner = node.clone();
        tokio::spawn(async move {
            node_runner.run().await;
        });
        node
    }
}
