use crate::protocol::tcp::{TcpAcceptError, TcpReadError, TcpSendError};
use crate::protocol::Protocol;
use crate::route::Router;
use etherparse::{Ipv4HeaderSlice, TcpHeader, TcpHeaderSlice};
use rand::{thread_rng, Rng};
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::mpsc::{self, channel};
use tokio::sync::oneshot;

use super::buf::{RecvBuf, SendBuf};
use super::{Port, SocketId, TCP_DEFAULT_WINDOW_SZ};

#[derive(Debug)]
struct InnerTcpConn<const N: usize> {
    send_buf: SendBuf<N>,
    recv_buf: RecvBuf<N>,
}

#[derive(Clone, Debug)]
pub struct TcpConn {
    inner: Arc<InnerTcpConn<TCP_DEFAULT_WINDOW_SZ>>,
}

impl TcpConn {
    fn new(start_seq_no: usize, start_ack_no: usize) -> Self {
        Self {
            inner: Arc::new(InnerTcpConn::new(start_seq_no, start_ack_no)),
        }
    }

    /// Sends bytes over a connection.
    ///
    /// Blocks until all bytes have been acknowledged by the other end.
    pub async fn send_all(&self, bytes: &[u8]) -> Result<(), TcpSendError> {
        self.inner.send_all(bytes).await
    }

    /// Reads N bytes from the connection, where N is `out_buffer`'s size.
    pub async fn read_all(&self, out_buffer: &mut [u8]) -> Result<(), TcpReadError> {
        self.inner.read_all(out_buffer).await
    }
}

impl<const N: usize> InnerTcpConn<N> {
    fn new(start_seq_no: usize, start_ack_no: usize) -> Self {
        Self {
            send_buf: SendBuf::new( /* TODO: start_seq_no */ ),
            recv_buf: RecvBuf::new(start_ack_no),
        }
    }

    async fn send_all(&self, bytes: &[u8]) -> Result<(), TcpSendError> {
        todo!()
    }

    async fn read_all(&self, out_buffer: &mut [u8]) -> Result<(), TcpReadError> {
        todo!()
    }
}

pub struct TcpListener {
    receiver: mpsc::Receiver<TcpConn>,
}

impl TcpListener {
    /// Creates a new TcpListener.
    ///
    /// The listener can be used to accept incoming connections
    pub fn new(receiver: mpsc::Receiver<TcpConn>) -> Self {
        Self { receiver }
    }
    /// Yields new client connections.
    ///
    /// To repeatedly accept new client connections:
    /// ```ignore
    /// let mut listener = node.listen(5353).unwrap();
    /// while let Ok(conn) = listener.accept().await {
    ///     // handle new conn...
    /// }
    /// ```
    pub async fn accept(&mut self) -> Result<TcpConn, TcpAcceptError> {
        self.receiver
            .recv()
            .await
            .ok_or(TcpAcceptError::ListenSocketClosed)
    }
}

#[derive(Debug)]
pub enum TransportError {
    DestUnreachable(Ipv4Addr),
}

pub enum TcpState {
    Closed(Closed),
    SynSent(SynSent),
    SynReceived(SynReceived),
    Established(Established),
    Listen(Listen),
}

impl TcpState {
    fn new(router: Arc<Router>) -> Self {
        Self::Closed(Closed::new(router))
    }
}

impl From<Closed> for TcpState {
    fn from(s: Closed) -> Self {
        Self::Closed(s)
    }
}

impl From<SynSent> for TcpState {
    fn from(s: SynSent) -> Self {
        Self::SynSent(s)
    }
}

impl From<SynReceived> for TcpState {
    fn from(s: SynReceived) -> Self {
        Self::SynReceived(s)
    }
}

impl From<Established> for TcpState {
    fn from(s: Established) -> Self {
        Self::Established(s)
    }
}

impl From<Listen> for TcpState {
    fn from(s: Listen) -> Self {
        Self::Listen(s)
    }
}

pub struct Closed {
    seq_no: u32,
    router: Arc<Router>,
}

impl Closed {
    pub fn new(router: Arc<Router>) -> Self {
        Self {
            router,
            seq_no: Self::gen_rand_seq_no(),
        }
    }

    pub async fn connect(
        self,
        src_port: Port,
        dest: (Ipv4Addr, Port),
    ) -> Result<(oneshot::Receiver<TcpConn>, SynSent), TransportError> {
        let (established_tx, established_rx) = oneshot::channel();
        let (dest_ip, dest_port) = dest;

        let syn_pkt = self.make_syn_packet(src_port, dest_port);
        self.router
            .send(&syn_pkt, Protocol::Tcp, dest_ip)
            .await
            .map_err(|_| TransportError::DestUnreachable(dest_ip))?;

        let syn_sent = SynSent {
            src_port,
            dest_port,
            dest_ip,
            established_tx,
            router: self.router,
            seq_no: self.seq_no,
        };
        Ok((established_rx, syn_sent))
    }

    pub fn listen(self, port: Port, tx: mpsc::Sender<TcpConn>) -> Listen {
        Listen {
            port,
            seq_no: self.seq_no,
            router: self.router,
            new_conn_tx: tx,
        }
    }

    fn make_syn_packet(&self, src_port: Port, dest_port: Port) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            src_port.0,
            dest_port.0,
            self.seq_no,
            (TCP_DEFAULT_WINDOW_SZ - 1).try_into().unwrap(),
        );
        header.syn = true;
        header.write(&mut bytes).unwrap();

        bytes
    }

    fn gen_rand_seq_no() -> u32 {
        thread_rng().gen_range(0..u16::MAX).into()
    }
}

pub struct Listen {
    port: Port,
    seq_no: u32,
    router: Arc<Router>,
    // Notifies when new connections are established with a new TcpConn.
    // The TcpListener has the receiving end of this channel.
    new_conn_tx: mpsc::Sender<TcpConn>,
}

impl Listen {
    pub async fn syn_received<'a>(
        &self,
        ip_header: &Ipv4HeaderSlice<'a>,
        syn_packet: &TcpHeaderSlice<'a>,
    ) -> Result<SynReceived, TransportError> {
        assert!(syn_packet.syn());

        let reply_ip = ip_header.source_addr();

        let ack_pkt = self.make_syn_ack_packet(syn_packet);

        self.router
            .send(&ack_pkt, Protocol::Tcp, reply_ip)
            .await
            .map_err(|_| TransportError::DestUnreachable(reply_ip))?;

        let syn_recvd = SynReceived {
            seq_no: self.seq_no,
            src_port: self.port,
            dest_ip: ip_header.source_addr(),
            dest_port: Port(syn_packet.source_port()),
            router: self.router.clone(),
            new_conn_tx: self.new_conn_tx.clone(),
        };

        Ok(syn_recvd)
    }

    fn make_syn_ack_packet<'a>(&self, syn_packet: &TcpHeaderSlice<'a>) -> Vec<u8> {
        let mut bytes = Vec::new();

        let src_port = self.port.0;
        let dst_port = syn_packet.source_port();

        let mut header = TcpHeader::new(
            src_port,
            dst_port,
            self.seq_no,
            (TCP_DEFAULT_WINDOW_SZ - 1).try_into().unwrap(),
        );
        header.syn = true;
        header.ack = true;
        header.acknowledgment_number = syn_packet.acknowledgment_number() + 1;

        header.write(&mut bytes).unwrap();

        bytes
    }
}

pub struct SynSent {
    seq_no: u32,
    src_port: Port,
    dest_ip: Ipv4Addr,
    dest_port: Port,
    router: Arc<Router>,
    established_tx: oneshot::Sender<TcpConn>,
}

impl SynSent {
    pub async fn establish<'a>(
        mut self,
        syn_ack_packet: &TcpHeaderSlice<'a>,
    ) -> Result<Established, TransportError> {
        assert!(syn_ack_packet.syn());
        assert!(syn_ack_packet.ack());
        let ack_pkt = self.make_ack_packet(syn_ack_packet);

        self.router
            .send(&ack_pkt, Protocol::Tcp, self.dest_ip)
            .await
            .map_err(|_| TransportError::DestUnreachable(self.dest_ip))?;

        let last_ack_no = syn_ack_packet.acknowledgment_number();

        let conn = TcpConn::new(
            self.seq_no.try_into().unwrap(),
            last_ack_no.try_into().unwrap(),
        );

        self.established_tx
            .send(conn.clone())
            .expect("Failed to notify new connection established");

        Ok(Established {
            seq_no: self.seq_no,
            src_port: self.src_port,
            dest_ip: self.dest_ip,
            dest_port: self.dest_port,
            router: self.router,
            last_ack_no,
            conn,
        })
    }

    fn make_ack_packet<'a>(&mut self, syn_ack_packet: &TcpHeaderSlice<'a>) -> Vec<u8> {
        let mut bytes = Vec::new();

        let seq_no = {
            self.seq_no += 1;
            self.seq_no
        };

        let mut header = TcpHeader::new(
            self.src_port.0,
            self.dest_port.0,
            seq_no,
            (TCP_DEFAULT_WINDOW_SZ - 1).try_into().unwrap(),
        );
        header.syn = true;
        header.ack = true;
        header.acknowledgment_number = syn_ack_packet.acknowledgment_number() + 1;

        header.write(&mut bytes).unwrap();

        bytes
    }
}

pub struct SynReceived {
    seq_no: u32,
    src_port: Port,
    dest_ip: Ipv4Addr,
    dest_port: Port,
    router: Arc<Router>,
    new_conn_tx: mpsc::Sender<TcpConn>,
}

impl SynReceived {
    pub async fn establish<'a>(self, ack_packet: &TcpHeaderSlice<'a>) -> Established {
        assert!(ack_packet.ack());

        let last_ack_no = ack_packet.acknowledgment_number();
        let conn = TcpConn::new(
            self.seq_no.try_into().unwrap(),
            last_ack_no.try_into().unwrap(),
        );

        self.new_conn_tx
            .send(conn.clone())
            .await
            .expect("TcpListener not notified");

        Established {
            seq_no: self.seq_no,
            src_port: self.src_port,
            dest_ip: self.dest_ip,
            dest_port: self.dest_port,
            router: self.router,
            last_ack_no,
            conn,
        }
    }
}

pub struct Established {
    seq_no: u32,
    src_port: Port,
    dest_ip: Ipv4Addr,
    dest_port: Port,
    router: Arc<Router>,
    last_ack_no: u32,
    conn: TcpConn,
}

#[derive(Debug)]
pub enum TcpConnectError {
    Transport(TransportError),
    AlreadyConnected,
}

#[derive(Debug)]
pub enum ListenTransitionError {
    // Errs when attempting to transition into Listen state from a state that's
    // not Closed.
    NotFromClosed,
}

pub enum UpdateAction {
    NewSynReceivedSocket(SynReceived),
}

pub struct Socket {
    id: SocketId,
    state: Option<TcpState>,
}

impl Socket {
    pub fn new(id: SocketId, router: Arc<Router>) -> Self {
        Self {
            id,
            state: Some(TcpState::new(router)),
        }
    }

    pub fn with_state(id: SocketId, state: TcpState) -> Self {
        Self {
            id,
            state: Some(state),
        }
    }

    pub fn listen(&mut self, port: Port) -> Result<TcpListener, ListenTransitionError> {
        let state = self.state.take().unwrap();
        match state {
            TcpState::Closed(s) => {
                let (new_conn_tx, new_conn_rx) = channel(1024);
                let listener = TcpListener::new(new_conn_rx);
                self.state = Some(s.listen(port, new_conn_tx).into());
                Ok(listener)
            }
            _ => {
                self.state = Some(state);
                Err(ListenTransitionError::NotFromClosed)
            }
        }
    }

    pub fn id(&self) -> SocketId {
        self.id
    }

    pub fn local_port(&self) -> Port {
        self.id.local_port()
    }

    pub fn remote_ip(&self) -> Ipv4Addr {
        self.id.remote_ip()
    }

    pub fn remote_port(&self) -> Port {
        self.id.remote_port()
    }

    pub fn remote_ip_port(&self) -> (Ipv4Addr, Port) {
        (self.remote_ip(), self.remote_port())
    }

    pub async fn initiate_connection(
        &mut self,
    ) -> Result<oneshot::Receiver<TcpConn>, TcpConnectError> {
        let state = self.state.take().unwrap();
        match state {
            TcpState::Closed(s) => {
                let (established_rx, syn_sent) = s
                    .connect(self.local_port(), self.remote_ip_port())
                    .await
                    .map_err(TcpConnectError::Transport)?;
                self.state = Some(syn_sent.into());
                Ok(established_rx)
            }
            _ => {
                self.state = Some(state);
                Err(TcpConnectError::AlreadyConnected)
            }
        }
    }

    pub async fn handle_packet<'a>(
        &mut self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) -> Option<UpdateAction> {
        let state = self
            .state
            .take()
            .expect("A socket should not handle packets concurrently");

        let (next_state, action) = match state {
            TcpState::Closed(s) => {
                panic!("Should not receive packet under closed state");
            }
            TcpState::Listen(s) => {
                if tcp_header.syn() {
                    let syn_recvd_state = s.syn_received(ip_header, tcp_header).await.unwrap();
                    (
                        s.into(),
                        Some(UpdateAction::NewSynReceivedSocket(syn_recvd_state)),
                    )
                } else {
                    eprintln!("Should ignore receive non-syn packet under listen state");
                    (TcpState::Listen(s), None)
                }
            }
            TcpState::SynSent(s) => (s.establish(tcp_header).await.unwrap().into(), None),
            TcpState::SynReceived(s) => (s.establish(tcp_header).await.into(), None),
            TcpState::Established(_) => {
                todo!()
            }
        };
        self.state = Some(next_state);

        action
    }
}
