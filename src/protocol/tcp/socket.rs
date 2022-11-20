use crate::protocol::tcp::buf::{FillError, SetTailError, WriteRangeError};
use crate::protocol::tcp::transport::RetransmissionConfig;
use crate::protocol::tcp::{SocketIdBuilder, TcpAcceptError, TcpReadError, TcpSendError};
use crate::protocol::Protocol;
use crate::route::Router;
use crate::utils::sync::RaceOneShotSender;
use etherparse::{Ipv4HeaderSlice, TcpHeader, TcpHeaderSlice};
use rand::{thread_rng, Rng};
use std::cmp::min;
use std::fmt::Display;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{self, channel};
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::task::JoinHandle;

use super::buf::{RecvBuf, SendBuf};
use super::transport::{transport_single_message, AckHandle, TcpTransport};
use super::{
    Port, Remote, SocketDescriptor, SocketId, TcpCloseError, TcpConnError, TCP_DEFAULT_WINDOW_SZ,
};

#[derive(Clone, Debug)]
pub struct TcpConn {
    inner: Arc<InnerTcpConn<TCP_DEFAULT_WINDOW_SZ>>,
    socket_id: SocketId,
}

impl TcpConn {
    fn new(
        socket_id: SocketId,
        remote: Remote,
        local_port: Port,
        start_seq_no: usize,
        start_ack_no: usize,
        router: Arc<Router>,
    ) -> Self {
        Self {
            socket_id,
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

    /// Read all bytes from the connection until it is closed.
    pub async fn read_till_closed(&self) -> Vec<u8> {
        let mut read_buf = [0; 1024];
        let mut out_buf = Vec::new();

        loop {
            match self.read_all(&mut read_buf).await {
                Ok(_) => {
                    out_buf.extend_from_slice(&read_buf);
                }
                Err(e) => match e {
                    TcpReadError::Closed(read_bytes) => {
                        out_buf.extend_from_slice(&read_buf[..read_bytes]);
                        break;
                    }
                    _ => unreachable!(),
                },
            }
        }

        out_buf
    }

    pub fn socket_id(&self) -> SocketId {
        self.socket_id
    }

    /// Close the write-end of the socket.
    async fn close(&self) {
        self.inner
            .close()
            .await
            .expect("Socket sendbuf should only be closed once");
    }

    async fn close_read(&self) {
        self.inner
            .close_read()
            .await
            .expect("Socket recvbuf should only be closed once");
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

    pub fn remote(&self) -> &Remote {
        &self.inner.remote
    }

    pub fn local_port(&self) -> Port {
        self.inner.local_port
    }

    async fn drain_content_on_close(&self) -> usize {
        self.inner.drain_content_on_close().await
    }
}

const ACK_BATCH_SZ: usize = 10;

#[derive(Debug)]
struct InnerTcpConn<const N: usize> {
    send_buf: SendBuf<N>,
    recv_buf: RecvBuf<N>,
    remote: Remote,
    local_port: Port,
    transport_worker: JoinHandle<()>,
    should_ack: broadcast::Sender<()>,
    ack_policy: AckBatchPolicy<ACK_BATCH_SZ>,
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
        let (should_ack_tx, should_ack_rx) = broadcast::channel(10);

        let sb = send_buf.clone();
        let rb = recv_buf.clone();
        let transport_worker = tokio::spawn(async move {
            TcpTransport::init(sb, rb, remote, local_port, router, should_ack_rx)
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
            should_ack: should_ack_tx,
            ack_policy: AckBatchPolicy::<ACK_BATCH_SZ>::default(),
        }
    }

    async fn close(&self) -> Result<(), TcpCloseError> {
        self.send_buf
            .close()
            .await
            .map_err(|_| TcpCloseError::AlreadyClosed)
    }

    async fn close_read(&self) -> Result<(), TcpCloseError> {
        self.recv_buf
            .close()
            .await
            .map_err(|_| TcpCloseError::AlreadyClosed)
    }

    async fn send_all(&self, bytes: &[u8]) -> Result<(), TcpSendError> {
        self.send_buf
            .write_all(bytes)
            .await
            .map_err(|_| TcpSendError::ConnClosed)
    }

    async fn read_all(&self, out_buffer: &mut [u8]) -> Result<(), TcpReadError> {
        const MAX_READ_SZ: usize = 1024;

        let mut curr = 0;
        while curr < out_buffer.len() {
            let end = min(out_buffer.len(), curr + MAX_READ_SZ);
            match self.recv_buf.fill(&mut out_buffer[curr..end]).await {
                Ok(_) => {
                    curr = end;
                }
                Err(e) => match e {
                    FillError::Closed(filled_bytes) => {
                        curr += filled_bytes;
                        return Err(TcpReadError::Closed(curr));
                    }
                },
            }
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

        self.send_buf.set_window_size(tcp_header.window_size());
        self.update_last_acked_byte(tcp_header.acknowledgment_number())
            .await;

        if !payload.is_empty() {
            self.write_received_bytes(tcp_header.sequence_number(), payload)
                .await;
        }

        if self.ack_policy.should_ack() {
            self.should_ack.send(()).unwrap();
        }
    }
}

impl<const N: usize> InnerTcpConn<N> {
    async fn update_last_acked_byte(&self, ack: u32) {
        if let Err(e) = self.send_buf.set_tail(ack.try_into().unwrap()).await {
            match e {
                SetTailError::LowerThanCurrent => log::error!("Remote responded with a lower ack"),
                SetTailError::TooBig => log::error!(
                    "Remote responded with an ack that's higher than the greatest sent seq no."
                ),
            }
        }
    }

    async fn write_received_bytes(&self, seq_no: u32, payload: &[u8]) {
        if let Err(e) = self
            .recv_buf
            .write(seq_no.try_into().unwrap(), payload)
            .await
        {
            match e {
                WriteRangeError::SeqNoTooSmall(min_seq_no) => log::info!(
                    "Received delayed packet, min seq no: {}, got seq no {}",
                    min_seq_no,
                    seq_no
                ),
                WriteRangeError::ExceedBuffer(_) => {
                    log::error!("Remote did not honor window size")
                }
            };

            self.should_ack.send(()).unwrap();
        }
    }

    async fn drain_content_on_close(&self) -> usize {
        self.send_buf.wait_for_empty().await
    }
}

impl<const N: usize> Drop for InnerTcpConn<N> {
    fn drop(&mut self) {
        // TODO: notify transport worker for shutdown
        // self.transport_worker.abort();
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

/// Utility that that allows a TcpConn to batch up to LAG Ack packets.
#[derive(Default, Debug)]
struct AckBatchPolicy<const LAG: usize> {
    count: AtomicUsize,
}

impl<const LAG: usize> AckBatchPolicy<LAG> {
    fn should_ack(&self) -> bool {
        self.count.fetch_add(1, Ordering::AcqRel) % LAG == 0
    }
}

#[derive(Debug, Copy, Clone)]
pub enum TransportError {
    DestUnreachable(Ipv4Addr),
}

/// Possible socket state types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketStatus {
    Closed,
    SynSent,
    SynReceived,
    Established,
    Listen,
    FinWait1,
    FinWait2,
    Closing,
    TimeWait,
    CloseWait,
    LastAck,
}

impl From<&TcpState> for SocketStatus {
    fn from(s: &TcpState) -> Self {
        match s {
            TcpState::Closed(_) => SocketStatus::Closed,
            TcpState::SynSent(_) => SocketStatus::SynSent,
            TcpState::SynReceived(_) => SocketStatus::SynReceived,
            TcpState::Established(_) => SocketStatus::Established,
            TcpState::Listen(_) => SocketStatus::Listen,
            TcpState::FinWait1(_) => SocketStatus::FinWait1,
            TcpState::FinWait2(_) => SocketStatus::FinWait2,
            TcpState::Closing(_) => SocketStatus::Closing,
            TcpState::TimeWait(_) => SocketStatus::TimeWait,
            TcpState::CloseWait(_) => SocketStatus::CloseWait,
            TcpState::LastAck(_) => SocketStatus::LastAck,
        }
    }
}

enum TcpState {
    Closed(Closed),
    SynSent(SynSent),
    SynReceived(SynReceived),
    Established(Established),
    Listen(Listen),
    FinWait1(FinWait1),
    FinWait2(FinWait2),
    Closing(Closing),
    TimeWait(TimeWait),
    CloseWait(CloseWait),
    LastAck(LastAck),
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

impl From<FinWait1> for TcpState {
    fn from(s: FinWait1) -> Self {
        Self::FinWait1(s)
    }
}

impl From<FinWait2> for TcpState {
    fn from(s: FinWait2) -> Self {
        Self::FinWait2(s)
    }
}

impl From<Closing> for TcpState {
    fn from(s: Closing) -> Self {
        Self::Closing(s)
    }
}

impl From<TimeWait> for TcpState {
    fn from(s: TimeWait) -> Self {
        Self::TimeWait(s)
    }
}

impl From<CloseWait> for TcpState {
    fn from(s: CloseWait) -> Self {
        Self::CloseWait(s)
    }
}

impl From<LastAck> for TcpState {
    fn from(s: LastAck) -> Self {
        Self::LastAck(s)
    }
}

struct Closed {
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
    ) -> Result<(oneshot::Receiver<Result<TcpConn, TcpConnError>>, SynSent), TransportError> {
        let (established_tx, established_rx) = oneshot::channel();
        let (dest_ip, dest_port) = dest;

        let syn_pkt = self.make_syn_packet(src_port, dest_port);

        let established_tx = RaceOneShotSender::from(established_tx);
        let established = established_tx.clone();

        let ack_handle = transport_single_message(
            syn_pkt,
            Remote::new(dest_ip, dest_port),
            self.router.clone(),
            RetransmissionConfig::default(),
            move |_| {
                established.send(Err(TcpConnError::Timeout)).ok();
            },
        );

        let syn_sent = SynSent {
            src_port,
            dest_port,
            dest_ip,
            syn_packet_rtx_handle: ack_handle,
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

struct Listen {
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

        let syn_ack_pkt = self.make_syn_ack_packet(syn_packet);

        let ack_handle = transport_single_message(
            syn_ack_pkt,
            Remote::new(ip_header.source_addr(), syn_packet.source_port().into()),
            self.router.clone(),
            RetransmissionConfig::default(),
            move |_| {
                // TODO: delete SynReceived socket
            },
        );

        let syn_recvd = SynReceived {
            seq_no: self.seq_no + 1,
            local_port: self.port,
            synack_ack_handle: ack_handle,
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

struct SynSent {
    seq_no: u32,
    src_port: Port,
    dest_ip: Ipv4Addr,
    dest_port: Port,
    syn_packet_rtx_handle: AckHandle,
    router: Arc<Router>,
    established_tx: RaceOneShotSender<Result<TcpConn, TcpConnError>>,
}

impl SynSent {
    pub async fn establish<'a>(
        mut self,
        syn_ack_packet: &TcpHeaderSlice<'a>,
    ) -> Result<Established, TransportError> {
        assert!(syn_ack_packet.syn());
        assert!(syn_ack_packet.ack());
        assert_eq!(syn_ack_packet.acknowledgment_number(), self.seq_no);

        self.syn_packet_rtx_handle.acked();

        let ack_pkt = self.make_ack_packet(syn_ack_packet);
        let ack_no = syn_ack_packet.acknowledgment_number() + 1;

        self.router
            .send(&ack_pkt, Protocol::Tcp, self.dest_ip)
            .await
            .map_err(|_| TransportError::DestUnreachable(self.dest_ip))?;

        let send_buf_start = self.seq_no.try_into().unwrap();
        let recv_buf_start = (syn_ack_packet.sequence_number() + 1).try_into().unwrap();

        let sock_id = SocketIdBuilder::default()
            .with_remote_ip(self.dest_ip)
            .with_remote_port(self.dest_port)
            .with_local_port(self.src_port)
            .build()
            .unwrap();

        let conn = TcpConn::new(
            sock_id,
            Remote::new(self.dest_ip, self.dest_port),
            self.src_port,
            send_buf_start,
            recv_buf_start,
            self.router.clone(),
        );
        self.established_tx
            .send(Ok(conn.clone()))
            .expect("Failed to notify new connection established");

        Ok(Established {
            local_port: self.src_port,
            remote_ip: self.dest_ip,
            remote_port: self.dest_port,
            conn,
            router: self.router,
            last_ack: ack_no,
            last_seq: self.seq_no + 1,
        })
    }

    fn make_ack_packet<'a>(&mut self, syn_ack_packet: &TcpHeaderSlice<'a>) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            self.src_port.0,
            self.dest_port.0,
            self.seq_no,
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
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
    synack_ack_handle: AckHandle,
    new_conn_tx: mpsc::Sender<TcpConn>,
}

impl SynReceived {
    async fn establish<'a>(mut self, ack_packet: &TcpHeaderSlice<'a>) -> Established {
        assert!(ack_packet.ack());
        self.synack_ack_handle.acked();

        let send_buf_start = self.seq_no.try_into().unwrap();
        let recv_buf_start = ack_packet.sequence_number().try_into().unwrap();

        let sock_id = SocketIdBuilder::default()
            .with_remote_ip(self.remote_ip)
            .with_remote_port(self.remote_port)
            .with_local_port(self.local_port)
            .build()
            .unwrap();

        let conn = TcpConn::new(
            sock_id,
            Remote::new(self.remote_ip, self.remote_port),
            self.local_port,
            send_buf_start,
            recv_buf_start,
            self.router.clone(),
        );

        self.new_conn_tx.send(conn.clone()).await.ok();

        Established {
            local_port: self.local_port,
            remote_ip: self.remote_ip,
            remote_port: self.remote_port,
            conn,
            router: self.router,
            last_ack: ack_packet.acknowledgment_number(),
            last_seq: self.seq_no,
        }
    }

    pub fn into_socket(self, socket_id: SocketId, descriptor: SocketDescriptor) -> Socket {
        Socket::with_state(socket_id, descriptor, self.into())
    }
}

struct Established {
    local_port: Port,
    remote_ip: Ipv4Addr,
    remote_port: Port,
    conn: TcpConn,
    last_ack: u32,
    last_seq: u32,
    router: Arc<Router>,
}

impl Established {
    async fn handle_packet<'a>(
        self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) -> TcpState {
        if tcp_header.fin() {
            return self.passive_close(tcp_header).await.into();
        }
        self.conn
            .handle_packet(ip_header, tcp_header, payload)
            .await;
        self.into()
    }

    /// Perform transition from Established to CloseWait upon receiving a FIN
    /// packet.
    async fn passive_close<'a>(self, tcp_header: &TcpHeaderSlice<'a>) -> CloseWait {
        let ack_packet = self.make_handshake_ack_packet(tcp_header);
        self.router
            .send(&ack_packet, Protocol::Tcp, self.remote_ip)
            .await
            .map_err(|_| TransportError::DestUnreachable(self.remote_ip))
            .unwrap();

        self.conn.close_read().await;

        CloseWait {
            conn: self.conn,
            last_seq: self.last_seq,
            last_ack: self.last_ack,
            local_port: self.local_port,
            remote_ip: self.remote_ip,
            remote_port: self.remote_port,
            router: self.router,
        }
    }

    /// Perform transition from Established -> FinWait1 in a close() syscall.
    async fn active_close(self) -> FinWait1 {
        // Below are the stages of transition from Established -> FinWait1 -> Finwait2.
        //
        // 1. Mark the write-side of TcpConn as closed. This stops new data from
        // being written into SendBuf.
        //
        // 2. Established transitions to FinWait1 immediately, without blocking.
        //
        // 3. FIN packet is not sent until all data in SendBuf has been acked
        // by the remote. Once SendBuf is empty, transmit the FIN packet. This
        // is done in an asynchronous thread.
        //
        // 4. The FIN packet's sequence number is communicated to the socket,
        // now in FinWait1, via a shared Mutex<Option<usize>>.
        //
        // 5. The FinWait1 socket checks this Mutex<Option<usize>> every time it
        // processes an incoming packet. When the FIN's ACK arrives, FIN's
        // sequence number would've been written already.
        //
        // 6. The socket transitions into FinWait2 until it receives a packet
        // where ACK = FIN.seq_no + 1.
        //
        // 7. Upon transitioning into FinWait2, a oneshot message is sent to the
        // FIN packet transporter. This stops the FIN packet's retransmissions.

        let conn = self.conn.clone();
        conn.close().await;

        let (fin_acked_tx, fin_acked_rx) = oneshot::channel();
        let fin_seq_no: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));

        let router_clone = self.router.clone();
        let local_port = self.local_port;
        let remote = Remote::new(self.remote_ip, self.remote_port);
        let fin_seq_no_clone = fin_seq_no.clone();
        tokio::spawn(async move {
            let fin_seq_no = conn.drain_content_on_close().await;
            let fin_packet = Self::make_shutdown_fin_packet(fin_seq_no, local_port, remote.port());

            let mut ack_handle = transport_single_message(
                fin_packet,
                remote,
                router_clone,
                RetransmissionConfig::default(),
                |_| {},
            );

            {
                let mut write_guard = fin_seq_no_clone.lock().await;
                *write_guard = Some(fin_seq_no);
            }

            // TODO: optimize
            fin_acked_rx.await.unwrap();
            ack_handle.acked();
        });

        FinWait1 {
            local_port: self.local_port,
            remote_ip: self.remote_ip,
            remote_port: self.remote_port,
            conn: self.conn,
            fin_acked_tx,
            fin_seq_no,
            router: self.router,
        }
    }

    fn make_shutdown_fin_packet(fin_seq_no: usize, local_port: Port, remote_port: Port) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            local_port.0,
            remote_port.0,
            fin_seq_no.try_into().unwrap(),
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.fin = true;
        header.write(&mut bytes).unwrap();

        bytes
    }

    fn make_handshake_ack_packet<'a>(&self, fin_ack_header: &TcpHeaderSlice<'a>) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            self.local_port.0,
            self.remote_port.0,
            fin_ack_header.acknowledgment_number(),
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.ack = true;
        header.acknowledgment_number = fin_ack_header.sequence_number() + 1;
        header.write(&mut bytes).unwrap();
        bytes
    }
}

struct FinWait1 {
    local_port: Port,
    remote_ip: Ipv4Addr,
    remote_port: Port,
    conn: TcpConn,
    router: Arc<Router>,
    fin_seq_no: Arc<Mutex<Option<usize>>>,
    fin_acked_tx: oneshot::Sender<()>,
}

impl FinWait1 {
    async fn handle_packet<'a>(
        self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) -> TcpState {
        let ack = tcp_header.ack();
        let fin = tcp_header.fin();

        let fin_seq_no = *self.fin_seq_no.lock().await;

        if fin && ack {
            // Remote closed too
            let ack_packet = self.make_ack_packet(tcp_header);
            self.router
                .send(&ack_packet, Protocol::Tcp, self.remote_ip)
                .await
                .map_err(|_| TransportError::DestUnreachable(self.remote_ip))
                .unwrap();

            if let Some(fin_seq_no) = fin_seq_no {
                if tcp_header.acknowledgment_number() == fin_seq_no as u32 + 1 {
                    self.fin_acked_tx.send(()).unwrap();
                }
            }

            let state = TimeWait {};
            return state.into();
        }

        if fin {
            // Simultaneous close

            let ack_packet: Vec<u8> = self.make_ack_packet(tcp_header);
            self.router
                .send(ack_packet.as_slice(), Protocol::Tcp, self.remote_ip)
                .await
                .map_err(|_| TransportError::DestUnreachable(self.remote_ip))
                .unwrap();

            let state = Closing {
                fin_acked_tx: self.fin_acked_tx,
            };
            return state.into();
        }

        if ack {
            if let Some(fin_seq_no) = fin_seq_no {
                self.fin_acked_tx.send(()).unwrap();

                let state = FinWait2 {
                    conn: self.conn,
                    local_port: self.local_port,
                    remote_ip: self.remote_ip,
                    remote_port: self.remote_port,
                    router: self.router,
                };
                return state.into();
            }
        }

        // Handle normal traffic
        self.conn
            .handle_packet(ip_header, tcp_header, payload)
            .await;
        self.into()
    }

    fn make_ack_packet<'a>(&self, tcp_header: &TcpHeaderSlice<'a>) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            self.local_port.0,
            self.remote_port.0,
            tcp_header.acknowledgment_number(),
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.ack = true;
        header.acknowledgment_number = tcp_header.sequence_number() + 1;
        header.write(&mut bytes).unwrap();
        bytes
    }
}

struct FinWait2 {
    local_port: Port,
    remote_ip: Ipv4Addr,
    remote_port: Port,
    conn: TcpConn,
    router: Arc<Router>,
}

impl FinWait2 {
    async fn handle_packet<'a>(
        self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) -> TcpState {
        if tcp_header.fin() {
            self.handle_fin(tcp_header).await.into()
        } else {
            self.conn
                .handle_packet(ip_header, tcp_header, payload)
                .await;
            self.into()
        }
    }

    async fn handle_fin<'a>(&self, tcp_header: &TcpHeaderSlice<'a>) -> TimeWait {
        let ack_packet = self.make_ack_packet(tcp_header);
        self.router
            .send(&ack_packet, Protocol::Tcp, self.remote_ip)
            .await
            .unwrap();
        TimeWait {}
    }

    fn make_ack_packet<'a>(&self, tcp_header: &TcpHeaderSlice<'a>) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut header = TcpHeader::new(
            self.local_port.0,
            self.remote_port.0,
            tcp_header.acknowledgment_number(),
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.ack = true;
        header.acknowledgment_number = tcp_header.sequence_number() + 1;
        header.write(&mut bytes).unwrap();
        bytes
    }
}

struct Closing {
    fin_acked_tx: oneshot::Sender<()>,
}

impl Closing {
    fn handle_ack(self) -> TimeWait {
        self.fin_acked_tx.send(()).unwrap();
        TimeWait {}
    }
}

struct TimeWait {}

struct CloseWait {
    local_port: Port,
    remote_ip: Ipv4Addr,
    remote_port: Port,
    conn: TcpConn,
    last_ack: u32,
    last_seq: u32,
    router: Arc<Router>,
}

impl CloseWait {
    async fn close(&mut self, id: SocketId, local_port: Port) -> LastAck {
        let fin_packet = self.make_fin_packet(local_port, id.remote_port());
        self.conn.close().await;
        LastAck {
            router: self.router.clone(),
        }
    }

    fn make_fin_packet(&self, src_port: Port, dst_port: Port) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            src_port.0,
            dst_port.0,
            self.last_seq + 1,
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.fin = true;
        header.write(&mut bytes).unwrap();
        bytes
    }
}

struct LastAck {
    router: Arc<Router>,
}

impl LastAck {
    fn handle_ack(self) -> Closed {
        Closed::new(self.router)
    }
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
    descriptor: SocketDescriptor,
    state: Option<TcpState>,
}

impl Socket {
    pub fn new(id: SocketId, descriptor: SocketDescriptor, router: Arc<Router>) -> Self {
        Self {
            id,
            descriptor,
            state: Some(TcpState::new(router)),
        }
    }

    fn with_state(id: SocketId, descriptor: SocketDescriptor, state: TcpState) -> Self {
        Self {
            id,
            descriptor,
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

    pub fn status(&self) -> SocketStatus {
        SocketStatus::from(self.state.as_ref().unwrap())
    }

    pub async fn send_all(&self, payload: &[u8]) -> Result<(), TcpSendError> {
        match self.state.as_ref().unwrap() {
            TcpState::Established(s) => s.conn.send_all(payload).await,
            TcpState::Listen(_) | TcpState::SynSent(_) | TcpState::SynReceived(_) => {
                Err(TcpSendError::ConnNotEstablished)
            }
            _ => Err(TcpSendError::ConnClosed),
        }
    }

    /// Reads N bytes from the connection, where N is `out_buffer`'s size.
    pub async fn read_all(&self, out_buffer: &mut [u8]) -> Result<(), TcpReadError> {
        match self.state.as_ref().unwrap() {
            TcpState::Established(s) => s.conn.read_all(out_buffer).await,
            TcpState::Listen(_) | TcpState::SynSent(_) | TcpState::SynReceived(_) => {
                Err(TcpReadError::ConnNotEstablished)
            }
            _ => Err(TcpReadError::Closed(0)),
        }
    }

    pub async fn initiate_connection(
        &mut self,
    ) -> Result<oneshot::Receiver<Result<TcpConn, TcpConnError>>, TcpConnError> {
        let state = self.state.take().unwrap();
        match state {
            TcpState::Closed(s) => {
                let (established_rx, syn_sent) = s
                    .connect(self.local_port(), self.remote_ip_port())
                    .await
                    .map_err(TcpConnError::Transport)?;
                self.state = Some(syn_sent.into());
                Ok(established_rx)
            }
            _ => {
                self.state = Some(state);
                Err(TcpConnError::ConnectionExists(self.id.remote()))
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
            TcpState::Closed(_) => {
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
            TcpState::SynReceived(s) => {
                if tcp_header.acknowledgment_number() == s.seq_no {
                    (s.establish(tcp_header).await.into(), None)
                } else {
                    (s.into(), None)
                }
            }
            TcpState::Established(s) => {
                (s.handle_packet(ip_header, tcp_header, payload).await, None)
            }
            TcpState::FinWait1(s) => (s.handle_packet(ip_header, tcp_header, payload).await, None),
            TcpState::FinWait2(s) => (s.handle_packet(ip_header, tcp_header, payload).await, None),
            TcpState::Closing(s) => {
                if tcp_header.ack() {
                    (s.handle_ack().into(), None)
                } else {
                    (s.into(), None)
                }
            }
            TcpState::TimeWait(s) => todo!(),
            TcpState::CloseWait(s) => todo!(),
            TcpState::LastAck(s) => (s.handle_ack().into(), None), //TODO return action that triggers socket table pruning
        };
        self.state = Some(next_state);
        action
    }

    pub async fn close(&mut self) {
        let state = self.state.take().unwrap();
        match state {
            TcpState::Established(s) => {
                let state = s.active_close().await;
                self.state = Some(state.into());
            }
            _ => {
                self.state = Some(state);
                eprintln!("Should not be able to close a connection that's not established");
            }
        }
    }
}

impl Display for Socket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let id = self.descriptor.0;
        let state = SocketStatus::from(self.state.as_ref().unwrap());

        // TODO: print actual window sizes
        let local_window_sz = 0;
        let remote_window_sz = 0;

        write!(
            f,
            "{}\t{:?}\t{}\t{}",
            id, state, local_window_sz, remote_window_sz
        )
    }
}
