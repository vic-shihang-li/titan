use crate::protocol::tcp::buf::{SetTailError, SliceError, WriteRangeError};
use crate::protocol::tcp::{TcpAcceptError, TcpReadError, TcpSendError};
use crate::protocol::Protocol;
use crate::route::{Router, SendError};
use etherparse::{Ipv4HeaderSlice, TcpHeader, TcpHeaderSlice};
use rand::{thread_rng, Rng};
use std::cmp::min;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, channel};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::buf::{RecvBuf, SendBuf};
use super::{Port, Remote, SocketId, MAX_SEGMENT_SZ, TCP_DEFAULT_WINDOW_SZ};

#[derive(Debug)]
struct InnerTcpConn<const N: usize> {
    send_buf: SendBuf<N>,
    recv_buf: RecvBuf<N>,
    remote: Remote,
    local_port: Port,
    transport_worker: JoinHandle<()>,
}

#[derive(Clone, Debug)]
pub struct TcpConn {
    inner: Arc<InnerTcpConn<TCP_DEFAULT_WINDOW_SZ>>,
}

impl TcpConn {
    fn new(
        remote: Remote,
        local_port: Port,
        start_seq_no: usize,
        start_ack_no: usize,
        router: Arc<Router>,
    ) -> Self {
        Self {
            inner: Arc::new(InnerTcpConn::new(
                remote,
                local_port,
                start_seq_no,
                start_ack_no,
                router,
            )),
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

    async fn handle_packet<'a>(
        &self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) {
        self.inner
            .handle_packet(ip_header, tcp_header, payload)
            .await
    }
}

impl<const N: usize> InnerTcpConn<N> {
    fn new(
        remote: Remote,
        local_port: Port,
        start_seq_no: usize,
        start_ack_no: usize,
        router: Arc<Router>,
    ) -> Self {
        let send_buf = SendBuf::new(start_seq_no);
        let recv_buf = RecvBuf::new(start_ack_no);

        let sb = send_buf.clone();
        let rb = recv_buf.clone();
        let transport_worker = tokio::spawn(async move {
            TcpTransport::init(sb, rb, remote, local_port, router)
                .await
                .run()
                .await;
        });

        Self {
            send_buf,
            recv_buf,
            remote,
            local_port,
            transport_worker,
        }
    }

    async fn send_all(&self, bytes: &[u8]) -> Result<(), TcpSendError> {
        self.send_buf.write_all(bytes).await;
        Ok(())
    }

    async fn read_all(&self, out_buffer: &mut [u8]) -> Result<(), TcpReadError> {
        const MAX_READ_SZ: usize = 1024;

        let mut curr = 0;
        while curr < out_buffer.len() {
            let end = min(out_buffer.len(), curr + MAX_READ_SZ);
            self.recv_buf.fill(&mut out_buffer[curr..end]).await;
            curr = end;
        }

        Ok(())
    }

    async fn handle_packet<'a>(
        &self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) {
        assert!(tcp_header.ack());

        if let Err(e) = self
            .send_buf
            .set_tail(tcp_header.acknowledgment_number().try_into().unwrap())
            .await
        {
            match e {
                SetTailError::LowerThanCurrent => log::error!("Remote responded with a lower ack"),
                SetTailError::TooBig => log::error!(
                    "Remote responded with an ack that's higher than the greatest sent seq no."
                ),
            }
        }

        self.send_buf.set_window_size(tcp_header.window_size());

        if !payload.is_empty() {
            if let Err(e) = self
                .recv_buf
                .write(tcp_header.sequence_number().try_into().unwrap(), payload)
                .await
            {
                match e {
                    WriteRangeError::SeqNoTooSmall(_) => log::info!("Received delayed packet"),
                    WriteRangeError::ExceedBuffer(_) => {
                        log::error!("Remote did not honor window size")
                    }
                }
            }
        }
    }
}

impl<const N: usize> Drop for InnerTcpConn<N> {
    fn drop(&mut self) {
        self.transport_worker.abort();
    }
}

struct TcpTransport<const N: usize> {
    send_buf: SendBuf<N>,
    recv_buf: RecvBuf<N>,
    remote: Remote,
    local_port: Port,
    router: Arc<Router>,
    seq_no: usize,
    last_transmitted: Instant,
    ack_batch_timeout: Duration,
    retrans_interval: Duration,
}

impl<const N: usize> TcpTransport<N> {
    async fn init(
        send_buf: SendBuf<N>,
        recv_buf: RecvBuf<N>,
        remote: Remote,
        local_port: Port,
        router: Arc<Router>,
    ) -> Self {
        let seq_no = send_buf.tail().await;
        Self {
            send_buf,
            recv_buf,
            remote,
            local_port,
            router,
            seq_no,
            last_transmitted: Instant::now(),
            ack_batch_timeout: Duration::from_millis(1),
            retrans_interval: Duration::from_millis(5),
        }
    }

    async fn run(mut self) {
        let mut segment = [0; MAX_SEGMENT_SZ];
        let mut segment_sz = MAX_SEGMENT_SZ;

        // Set upper bound on how long before acks are sent back to sender.
        let mut transmit_ack_interval = tokio::time::interval(self.ack_batch_timeout);

        let mut retrans_interval = tokio::time::interval(self.retrans_interval);

        loop {
            tokio::select! {
                _ = self.send_buf.wait_for_new_data(self.seq_no) => {
                    if segment_sz == 0 {
                        segment_sz = MAX_SEGMENT_SZ;
                    }
                    segment_sz = self.try_consume_and_send(&mut segment[..segment_sz]).await;
                }
                _ = transmit_ack_interval.tick() => {
                    self.check_and_retransmit_ack().await;
                }
                _ = retrans_interval.tick() => {
                    self.check_retransmission(&mut segment).await;
                }
            }
        }
    }

    async fn try_consume_and_send(&mut self, buf: &mut [u8]) -> usize {
        #[allow(unused_assignments)]
        let mut next_segment_sz = MAX_SEGMENT_SZ;

        let window_sz = self.send_buf.window_size().into();
        match self.send_buf.try_slice(self.seq_no, buf).await {
            Ok(bytes_readable) => {
                // TODO: handle send failure
                if self.send(self.seq_no, buf).await.is_ok() {
                    self.seq_no += buf.len();
                    next_segment_sz = min(MAX_SEGMENT_SZ, min(window_sz, bytes_readable));
                }
            }
            Err(e) => match e {
                SliceError::OutOfRange(unconsumed_sz) => {
                    next_segment_sz = min(unconsumed_sz, window_sz);
                }
            },
        }

        next_segment_sz
    }

    async fn check_and_retransmit_ack(&mut self) {
        if self.last_transmitted.elapsed() > self.ack_batch_timeout {
            // The empty-payload packet's main purpose is to update the remote
            // about our latest ACK sequence number.
            self.send(self.seq_no, &[]).await.ok();
        }
    }

    async fn check_retransmission(&mut self, segment_buf: &mut [u8]) {
        let (tail, last_ack_update_time) = self.send_buf.get_tail_and_age().await;

        #[allow(clippy::collapsible_if)]
        if last_ack_update_time > self.retrans_interval {
            if self.send_buf.try_slice(tail, segment_buf).await.is_ok() {
                self.send(tail, segment_buf).await.ok();
            }
        }
    }

    async fn send(&mut self, seq_no: usize, payload: &[u8]) -> Result<(), SendError> {
        let tcp_packet_bytes = self.prepare_tcp_packet(seq_no, payload).await;
        self.router
            .send(&tcp_packet_bytes, Protocol::Tcp, self.remote.ip())
            .await
            .map(|_| {
                self.last_transmitted = Instant::now();
            })
    }

    async fn prepare_tcp_packet(&self, seq_no: usize, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();

        let src_port = self.local_port.0;
        let dst_port = self.remote.port().0;
        let seq_no = seq_no.try_into().expect("seq no overflow");
        let window_sz = self.send_buf.advertised_window_size().await;

        let mut header = TcpHeader::new(src_port, dst_port, seq_no, window_sz);
        header.syn = true;
        header.ack = true;
        header.acknowledgment_number = self.recv_buf.head().await.try_into().unwrap();

        header.write(&mut bytes).unwrap();
        bytes.extend_from_slice(payload);

        bytes
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
            seq_no: self.seq_no + 1,
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
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
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
            seq_no: self.seq_no + 1,
            local_port: self.port,
            remote_ip: ip_header.source_addr(),
            remote_port: Port(syn_packet.source_port()),
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
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.syn = true;
        header.ack = true;
        header.acknowledgment_number = syn_packet.sequence_number() + 1;

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

        let send_buf_start = (self.seq_no + 1).try_into().unwrap();
        let recv_buf_start = (syn_ack_packet.sequence_number() + 1).try_into().unwrap();

        let conn = TcpConn::new(
            Remote::new(self.dest_ip, self.dest_port),
            self.src_port,
            send_buf_start,
            recv_buf_start,
            self.router,
        );

        self.established_tx
            .send(conn.clone())
            .expect("Failed to notify new connection established");

        Ok(Established { conn })
    }

    fn make_ack_packet<'a>(&mut self, syn_ack_packet: &TcpHeaderSlice<'a>) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            self.src_port.0,
            self.dest_port.0,
            self.seq_no,
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.syn = true;
        header.ack = true;
        header.acknowledgment_number = syn_ack_packet.sequence_number() + 1;

        header.write(&mut bytes).unwrap();

        bytes
    }
}

pub struct SynReceived {
    seq_no: u32,
    local_port: Port,
    remote_ip: Ipv4Addr,
    remote_port: Port,
    router: Arc<Router>,
    new_conn_tx: mpsc::Sender<TcpConn>,
}

impl SynReceived {
    pub async fn establish<'a>(self, ack_packet: &TcpHeaderSlice<'a>) -> Established {
        assert!(ack_packet.ack());

        let send_buf_start = self.seq_no.try_into().unwrap();
        let recv_buf_start = (ack_packet.sequence_number() + 1).try_into().unwrap();

        let conn = TcpConn::new(
            Remote::new(self.remote_ip, self.remote_port),
            self.local_port,
            send_buf_start,
            recv_buf_start,
            self.router,
        );

        self.new_conn_tx
            .send(conn.clone())
            .await
            .expect("TcpListener not notified");

        Established { conn }
    }
}

pub struct Established {
    conn: TcpConn,
}

impl Established {
    async fn handle_packet<'a>(
        &self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) {
        self.conn
            .handle_packet(ip_header, tcp_header, payload)
            .await
    }
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
            TcpState::Established(s) => {
                s.handle_packet(ip_header, tcp_header, payload).await;
                (s.into(), None)
            }
        };
        self.state = Some(next_state);

        action
    }
}
