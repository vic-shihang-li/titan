use crate::protocol::tcp::{TcpAcceptError, TcpListenError, TcpReadError, TcpSendError};
use etherparse::TcpHeader;

pub struct TcpConn {
    // sendBuf: SendBuf<n>,
    // recvBuf: RecvBuf<n>,
}

pub struct TcpListener {
    port: u16,
}

pub struct TcpMessage {
    header: TcpHeader,
    payload: Vec<u8>,
}

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
    /// Creates a new TcpListener.
    ///
    /// The listener can be used to accept incoming connections
    pub fn new(port: u16) -> Self {
        Self { port }
    }
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
