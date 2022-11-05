// TODO: remove this once the rest of TCP is implemented
#[allow(dead_code)]
mod buf;

use std::{net::Ipv4Addr, sync::Arc};

use crate::{net::Net, protocol::ProtocolHandler, route::Router};
use async_trait::async_trait;
use etherparse::Ipv4HeaderSlice;

pub struct TcpConn {}

pub struct TcpListener {}

#[derive(Debug)]
pub struct TcpConnError {}

#[derive(Debug)]
pub struct TcpListenError {}

#[derive(Debug)]
pub struct TcpAcceptError {}

#[derive(Debug)]
pub struct TcpSendError {}

#[derive(Debug)]
pub struct TcpReadError {}

impl TcpConn {
    /// Sends bytes over a connection.
    ///
    /// Blocks until all bytes have been acknowledged by the other end.
    pub async fn send_all(&self, bytes: &[u8]) -> Result<(), TcpSendError> {
        todo!()
    }

    /// Reads N bytes from the connection, where N is `out_buffer`'s size.
    pub async fn read_all(&self, out_buffer: &mut [u8]) -> Result<(), TcpReadError> {
        todo!()
    }
}

impl TcpListener {
    /// Yields new client connections.
    ///
    /// To repeatedly accept new client connections:
    /// ```
    /// while let Ok(conn) = listener.accept().await {
    ///     // handle new conn...
    /// }
    /// ```
    pub async fn accept(&self) -> Result<TcpConn, TcpAcceptError> {
        // TODO: create a new Tcp socket and state machine. (Keep the listener
        // socket, open a new socket to handle this client).
        //
        // 1. The new Tcp state machine should transition to SYN_RECVD after
        // replying syn+ack to client.
        // 2. When Tcp handler receives client's ack packet (3rd step in
        // handshake), the new Tcp state machine should transition to ESTABLISHED.
        todo!()
    }
}

/// A TCP stack.
#[derive(Default)]
pub struct Tcp {
    // a concurrent data structure holding Tcp stack states
}

impl Tcp {
    /// Attempts to connect to a host, establishing the client side of a TCP connection.
    pub async fn connect(&self, dest_ip: Ipv4Addr, port: u16) -> Result<TcpConn, TcpConnError> {
        // TODO: create Tcp state machine. State machine should
        // 1. Send syn packet, transition to SYN_SENT.
        // 2. When TCP handler receives syn+ack packet, send a syn packet and
        //    transition to ESTABLISHED.
        //
        // Tcp state machine should provide some function that blocks until
        // state becomes ESTABLISHED.

        todo!()
    }

    /// Starts listening for incoming connections at a port. Opens a listener socket.
    pub async fn listen(&self, port: u16) -> Result<TcpListener, TcpListenError> {
        // TODO: create Tcp machine that starts with LISTEN state. Open listen socket.

        todo!()
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
        _header: &Ipv4HeaderSlice<'a>,
        _payload: &[u8],
        _router: &Router,
        _net: &Net,
    ) {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use tokio::sync::Barrier;

    use crate::{node::NodeBuilder, protocol::Protocol};

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
            let tcp_stack = Arc::new(Tcp::default());
            let node = NodeBuilder::new(&sender_cfg, tcp_stack.clone())
                .with_protocol_handler(Protocol::Tcp, TcpHandler::new(tcp_stack))
                .build()
                .await;

            let dest_ip = {
                let recv_ips = receiver_cfg.get_my_interface_ips();
                recv_ips[0]
            };

            listen_barr.wait().await;

            let conn = node.connect(dest_ip, recv_listen_port).await.unwrap();
            conn.send_all(payload.to_string().as_bytes()).await.unwrap();
        });

        let receiver = tokio::spawn(async move {
            let tcp_stack = Arc::new(Tcp::default());
            let node = NodeBuilder::new(&recv_cfg, tcp_stack.clone())
                .with_protocol_handler(Protocol::Tcp, TcpHandler::new(tcp_stack))
                .build()
                .await;

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
}
