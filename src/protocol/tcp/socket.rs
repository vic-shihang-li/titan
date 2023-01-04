use crate::net::Net;
use crate::protocol::tcp::buf::{FillError, SetTailError, WriteRangeError};
use crate::protocol::tcp::prelude::SocketIdBuilder;
use crate::protocol::tcp::transport::RetransmissionConfig;
use crate::protocol::tcp::{TcpAcceptError, TcpReadError, TcpSendError};
use crate::protocol::Protocol;
use crate::utils::sync::RaceOneShotSender;
use etherparse::{Ipv4HeaderSlice, TcpHeader, TcpHeaderSlice};
use rand::{thread_rng, Rng};
use std::cmp::min;
use std::net::Ipv4Addr;
use std::sync::Arc;

use tokio::sync::mpsc::{self, channel};
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::task::JoinHandle;

use super::ack_policy::{self, AckPolicy};
use super::buf::{RecvBuf, SendBuf};
use super::transport::{transport_single_message, AckHandle, TcpTransport};
use super::{
    Port, Remote, SocketDescriptor, SocketId, TcpCloseError, TcpConnError, TCP_DEFAULT_WINDOW_SZ,
};

#[derive(Clone, Debug)]
pub struct TcpConn {
    inner: Arc<InnerTcpConn<TCP_DEFAULT_WINDOW_SZ, ack_policy::AlwaysAck>>,
    socket_id: SocketId,
}

impl TcpConn {
    fn new<N: Net + Send + Sync>(
        socket_id: SocketId,
        remote: Remote,
        local_port: Port,
        start_seq_no: usize,
        start_ack_no: usize,
        net: Arc<N>,
    ) -> Self {
        Self {
            socket_id,
            inner: Arc::new(InnerTcpConn::new(
                remote,
                local_port,
                start_seq_no,
                start_ack_no,
                net,
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

    pub fn remote(&self) -> &Remote {
        &self.inner.remote
    }

    pub fn local_port(&self) -> Port {
        self.inner.local_port
    }

    /// Close the write-end of the socket.
    async fn close(&self) {
        self.inner.close().await.ok();
    }

    async fn close_read(&self) {
        self.inner.close_read().await.ok();
    }

    pub fn is_read_closed(&self) -> bool {
        self.inner.is_read_closed()
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

    async fn drain_content_on_close(&self) -> usize {
        self.inner.drain_content_on_close().await
    }

    async fn local_window_sz(&self) -> usize {
        self.inner.local_window_sz().await
    }

    async fn remote_window_sz(&self) -> usize {
        self.inner.remote_window_sz().await
    }
}

#[derive(Debug)]
struct InnerTcpConn<const BUF_SZ: usize, A: AckPolicy> {
    send_buf: SendBuf<BUF_SZ>,
    recv_buf: RecvBuf<BUF_SZ>,
    remote: Remote,
    local_port: Port,
    transport_worker: JoinHandle<()>,
    should_ack: broadcast::Sender<()>,
    ack_policy: A,
}

impl<const BUF_SZ: usize, A: AckPolicy + Default> InnerTcpConn<BUF_SZ, A> {
    fn new<N: Net + Send + Sync>(
        remote: Remote,
        local_port: Port,
        start_seq_no: usize,
        start_ack_no: usize,
        net: Arc<N>,
    ) -> Self {
        let send_buf = SendBuf::new(start_seq_no);
        let recv_buf = RecvBuf::new(start_ack_no);
        let (should_ack_tx, should_ack_rx) = broadcast::channel(10);

        let sb = send_buf.clone();
        let rb = recv_buf.clone();
        let transport_worker = tokio::spawn(async move {
            TcpTransport::init(sb, rb, remote, local_port, net, should_ack_rx)
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
            ack_policy: A::default(),
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

    fn is_read_closed(&self) -> bool {
        self.recv_buf.closed()
    }

    async fn handle_packet<'a>(
        &self,
        _ip_header: &Ipv4HeaderSlice<'a>,
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

        if self.ack_policy.should_ack(tcp_header) {
            self.should_ack.send(()).unwrap();
        }
    }
}

impl<const N: usize, A: AckPolicy> InnerTcpConn<N, A> {
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

    async fn local_window_sz(&self) -> usize {
        self.recv_buf.window_size().await
    }

    async fn remote_window_sz(&self) -> usize {
        self.send_buf.window_size().into()
    }
}

impl<const N: usize, A: AckPolicy> Drop for InnerTcpConn<N, A> {
    fn drop(&mut self) {
        self.transport_worker.abort();
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

impl<N: Net> From<&TcpState<N>> for SocketStatus {
    fn from(s: &TcpState<N>) -> Self {
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

enum TcpState<N> {
    Closed(Closed<N>),
    SynSent(SynSent<N>),
    SynReceived(SynReceived<N>),
    Established(Established<N>),
    Listen(Listen<N>),
    FinWait1(FinWait1<N>),
    FinWait2(FinWait2<N>),
    Closing(Closing),
    TimeWait(TimeWait),
    CloseWait(CloseWait<N>),
    LastAck(LastAck<N>),
}

impl<N: Net> TcpState<N> {
    fn new(net: Arc<N>) -> Self {
        Self::Closed(Closed::new(net))
    }
}

impl<N: Net> From<Closed<N>> for TcpState<N> {
    fn from(s: Closed<N>) -> Self {
        Self::Closed(s)
    }
}

impl<N: Net> From<SynSent<N>> for TcpState<N> {
    fn from(s: SynSent<N>) -> Self {
        Self::SynSent(s)
    }
}

impl<N: Net> From<SynReceived<N>> for TcpState<N> {
    fn from(s: SynReceived<N>) -> Self {
        Self::SynReceived(s)
    }
}

impl<N: Net> From<Established<N>> for TcpState<N> {
    fn from(s: Established<N>) -> Self {
        Self::Established(s)
    }
}

impl<N: Net> From<Listen<N>> for TcpState<N> {
    fn from(s: Listen<N>) -> Self {
        Self::Listen(s)
    }
}

impl<N: Net> From<FinWait1<N>> for TcpState<N> {
    fn from(s: FinWait1<N>) -> Self {
        Self::FinWait1(s)
    }
}

impl<N: Net> From<FinWait2<N>> for TcpState<N> {
    fn from(s: FinWait2<N>) -> Self {
        Self::FinWait2(s)
    }
}

impl<N: Net> From<Closing> for TcpState<N> {
    fn from(s: Closing) -> Self {
        Self::Closing(s)
    }
}

impl<N: Net> From<TimeWait> for TcpState<N> {
    fn from(s: TimeWait) -> Self {
        Self::TimeWait(s)
    }
}

impl<N: Net> From<CloseWait<N>> for TcpState<N> {
    fn from(s: CloseWait<N>) -> Self {
        Self::CloseWait(s)
    }
}

impl<N: Net> From<LastAck<N>> for TcpState<N> {
    fn from(s: LastAck<N>) -> Self {
        Self::LastAck(s)
    }
}

impl<N: Net> TcpState<N> {
    async fn local_window_sz(&self) -> usize {
        match self {
            TcpState::Closed(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::SynSent(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::SynReceived(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::Established(s) => s.conn.local_window_sz().await,
            TcpState::Listen(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::FinWait1(s) => s.conn.local_window_sz().await,
            TcpState::FinWait2(s) => s.conn.local_window_sz().await,
            TcpState::Closing(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::TimeWait(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::CloseWait(s) => s.conn.local_window_sz().await,
            TcpState::LastAck(_) => TCP_DEFAULT_WINDOW_SZ,
        }
    }

    async fn remote_window_sz(&self) -> usize {
        match self {
            TcpState::Closed(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::SynSent(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::SynReceived(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::Established(s) => s.conn.remote_window_sz().await,
            TcpState::Listen(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::FinWait1(s) => s.conn.remote_window_sz().await,
            TcpState::FinWait2(s) => s.conn.remote_window_sz().await,
            TcpState::Closing(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::TimeWait(_) => TCP_DEFAULT_WINDOW_SZ,
            TcpState::CloseWait(s) => s.conn.remote_window_sz().await,
            TcpState::LastAck(_) => TCP_DEFAULT_WINDOW_SZ,
        }
    }

    fn is_read_closed(&self) -> Option<bool> {
        match self {
            TcpState::Closed(_) => None,
            TcpState::SynSent(_) => None,
            TcpState::SynReceived(_) => None,
            TcpState::Established(s) => s.conn.is_read_closed().into(),
            TcpState::Listen(_) => Some(true),
            TcpState::FinWait1(s) => s.conn.is_read_closed().into(),
            TcpState::FinWait2(s) => s.conn.is_read_closed().into(),
            TcpState::Closing(_) => Some(true),
            TcpState::TimeWait(_) => Some(true),
            TcpState::CloseWait(s) => s.conn.is_read_closed().into(),
            TcpState::LastAck(_) => None,
        }
    }
}

struct Closed<N> {
    seq_no: u32,
    net: Arc<N>,
}

impl<N: Net> Closed<N> {
    pub fn new(net: Arc<N>) -> Self {
        Self {
            net,
            seq_no: Self::gen_rand_seq_no(),
        }
    }

    pub async fn connect(
        self,
        src_port: Port,
        dest: (Ipv4Addr, Port),
    ) -> Result<(oneshot::Receiver<Result<TcpConn, TcpConnError>>, SynSent<N>), TransportError>
    {
        let (established_tx, established_rx) = oneshot::channel();
        let (dest_ip, dest_port) = dest;

        let syn_pkt = self.make_syn_packet(src_port, dest_port, dest_ip).await;

        let established_tx = RaceOneShotSender::from(established_tx);
        let established = established_tx.clone();

        let ack_handle = transport_single_message(
            syn_pkt,
            Remote::new(dest_ip, dest_port),
            self.net.clone(),
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
            net: self.net,
            seq_no: self.seq_no + 1,
        };
        Ok((established_rx, syn_sent))
    }

    pub fn listen(self, port: Port, tx: mpsc::Sender<TcpConn>) -> Listen<N> {
        Listen {
            port,
            seq_no: self.seq_no,
            net: self.net,
            new_conn_tx: tx,
        }
    }

    async fn make_syn_packet(&self, src_port: Port, dst_port: Port, dst_ip: Ipv4Addr) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            src_port.0,
            dst_port.0,
            self.seq_no,
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.syn = true;
        let payload: &[u8] = &[];
        let src_ip = self.net.get_outbound_ip(dst_ip).await.unwrap();
        let checksum = header
            .calc_checksum_ipv4_raw(src_ip, dst_ip.octets(), payload)
            .unwrap();
        header.checksum = checksum;
        header.write(&mut bytes).unwrap();
        bytes
    }

    fn gen_rand_seq_no() -> u32 {
        thread_rng().gen_range(0..u16::MAX).into()
    }
}

struct Listen<N> {
    port: Port,
    seq_no: u32,
    net: Arc<N>,
    // Notifies when new connections are established with a new TcpConn.
    // The TcpListener has the receiving end of this channel.
    new_conn_tx: mpsc::Sender<TcpConn>,
}

impl<N: Net> Listen<N> {
    pub async fn syn_received<'a>(
        &self,
        ip_header: &Ipv4HeaderSlice<'a>,
        syn_packet: &TcpHeaderSlice<'a>,
    ) -> Result<SynReceived<N>, TransportError> {
        assert!(syn_packet.syn());

        let src_ip = ip_header.destination_addr();
        let dst_ip = ip_header.source_addr();
        let syn_ack_pkt = self.make_syn_ack_packet(syn_packet, src_ip, dst_ip);

        let ack_handle = transport_single_message(
            syn_ack_pkt,
            Remote::new(ip_header.source_addr(), syn_packet.source_port().into()),
            self.net.clone(),
            RetransmissionConfig::default(),
            move |_| {
                // TODO: delete SynReceived socketx
            },
        );

        let syn_recvd = SynReceived {
            seq_no: self.seq_no + 1,
            local_port: self.port,
            synack_ack_handle: ack_handle,
            remote_ip: ip_header.source_addr(),
            remote_port: Port(syn_packet.source_port()),
            net: self.net.clone(),
            new_conn_tx: self.new_conn_tx.clone(),
        };

        Ok(syn_recvd)
    }

    fn make_syn_ack_packet(
        &self,
        syn_packet: &TcpHeaderSlice<'_>,
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
    ) -> Vec<u8> {
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
        let payload: &[u8] = &[];
        let checksum = header
            .calc_checksum_ipv4_raw(src_ip.octets(), dst_ip.octets(), payload)
            .unwrap();
        header.checksum = checksum;
        header.write(&mut bytes).unwrap();
        bytes
    }
}

struct SynSent<N> {
    seq_no: u32,
    src_port: Port,
    dest_ip: Ipv4Addr,
    dest_port: Port,
    syn_packet_rtx_handle: AckHandle,
    net: Arc<N>,
    established_tx: RaceOneShotSender<Result<TcpConn, TcpConnError>>,
}

impl<N: Net> SynSent<N> {
    pub async fn establish<'a>(
        mut self,
        syn_ack_packet: &TcpHeaderSlice<'a>,
    ) -> Result<Established<N>, TransportError> {
        assert!(syn_ack_packet.syn());
        assert!(syn_ack_packet.ack());
        assert_eq!(syn_ack_packet.acknowledgment_number(), self.seq_no);

        self.syn_packet_rtx_handle.acked();

        let ack_pkt = self.make_ack_packet(syn_ack_packet, self.dest_ip).await;

        self.net
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
            self.net.clone(),
        );
        self.established_tx
            .send(Ok(conn.clone()))
            .expect("Failed to notify new connection established");

        Ok(Established {
            local_port: self.src_port,
            remote_ip: self.dest_ip,
            remote_port: self.dest_port,
            conn,
            net: self.net,
            last_seq: self.seq_no + 1,
        })
    }

    async fn make_ack_packet<'a>(
        &mut self,
        syn_ack_packet: &TcpHeaderSlice<'a>,
        dst_ip: Ipv4Addr,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            self.src_port.0,
            self.dest_port.0,
            self.seq_no,
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.ack = true;
        header.acknowledgment_number = syn_ack_packet.sequence_number() + 1;
        let payload: &[u8] = &[];
        let src_ip = self.net.get_outbound_ip(dst_ip).await.unwrap();
        let checksum = header
            .calc_checksum_ipv4_raw(src_ip, dst_ip.octets(), payload)
            .unwrap();
        header.checksum = checksum;
        header.write(&mut bytes).unwrap();

        bytes
    }
}

pub struct SynReceived<N> {
    seq_no: u32,
    local_port: Port,
    remote_ip: Ipv4Addr,
    remote_port: Port,
    net: Arc<N>,
    synack_ack_handle: AckHandle,
    new_conn_tx: mpsc::Sender<TcpConn>,
}

impl<N: Net> SynReceived<N> {
    async fn establish<'a>(mut self, ack_packet: &TcpHeaderSlice<'a>) -> Established<N> {
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
            self.net.clone(),
        );

        self.new_conn_tx.send(conn.clone()).await.ok();

        Established {
            local_port: self.local_port,
            remote_ip: self.remote_ip,
            remote_port: self.remote_port,
            conn,
            net: self.net,
            last_seq: self.seq_no,
        }
    }

    pub fn into_socket(self, socket_id: SocketId, descriptor: SocketDescriptor) -> Socket<N> {
        Socket::with_state(socket_id, descriptor, self.into())
    }
}

struct Established<N> {
    local_port: Port,
    remote_ip: Ipv4Addr,
    remote_port: Port,
    conn: TcpConn,
    last_seq: u32,
    net: Arc<N>,
}

impl<N: Net> Established<N> {
    async fn handle_packet<'a>(
        self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) -> TcpState<N> {
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
    async fn passive_close<'a>(self, tcp_header: &TcpHeaderSlice<'a>) -> CloseWait<N> {
        let ack_packet = self
            .make_handshake_ack_packet(tcp_header, self.remote_ip)
            .await;
        self.net
            .send(&ack_packet, Protocol::Tcp, self.remote_ip)
            .await
            .map_err(|_| TransportError::DestUnreachable(self.remote_ip))
            .unwrap();

        self.conn.close_read().await;

        CloseWait {
            conn: self.conn,
            last_seq: self.last_seq,
            net: self.net,
        }
    }

    /// Perform transition from Established -> FinWait1 in a close() syscall.
    async fn active_close(self) -> FinWait1<N> {
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

        let net = self.net.clone();
        let src_ip = self.net.get_outbound_ip(self.remote_ip).await.unwrap();
        let local_port = self.local_port;
        let remote = Remote::new(self.remote_ip, self.remote_port);
        let fin_seq_no_clone = fin_seq_no.clone();
        tokio::spawn(async move {
            let fin_seq_no = conn.drain_content_on_close().await;
            let fin_packet = Self::make_shutdown_fin_packet(
                fin_seq_no,
                local_port,
                remote.port(),
                src_ip,
                remote.ip().octets(),
            );

            let mut ack_handle = transport_single_message(
                fin_packet,
                remote,
                net,
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
            net: self.net,
        }
    }

    async fn close_read(&self) {
        self.conn.close_read().await;
    }

    fn make_shutdown_fin_packet(
        fin_seq_no: usize,
        local_port: Port,
        remote_port: Port,
        src_ip: [u8; 4],
        dst_ip: [u8; 4],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            local_port.0,
            remote_port.0,
            fin_seq_no.try_into().unwrap(),
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.fin = true;
        let payload: &[u8] = &[];
        let checksum = header
            .calc_checksum_ipv4_raw(src_ip, dst_ip, payload)
            .unwrap();
        header.checksum = checksum;
        header.write(&mut bytes).unwrap();

        bytes
    }

    async fn make_handshake_ack_packet<'a>(
        &self,
        fin_ack_header: &TcpHeaderSlice<'a>,
        dst_ip: Ipv4Addr,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            self.local_port.0,
            self.remote_port.0,
            fin_ack_header.acknowledgment_number(),
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.ack = true;
        header.acknowledgment_number = fin_ack_header.sequence_number() + 1;
        let payload: &[u8] = &[];
        let src_ip = self.net.get_outbound_ip(dst_ip).await.unwrap();
        let checksum = header
            .calc_checksum_ipv4_raw(src_ip, dst_ip.octets(), payload)
            .unwrap();
        header.checksum = checksum;
        header.write(&mut bytes).unwrap();
        bytes
    }
}

struct FinWait1<N> {
    local_port: Port,
    remote_ip: Ipv4Addr,
    remote_port: Port,
    conn: TcpConn,
    net: Arc<N>,
    fin_seq_no: Arc<Mutex<Option<usize>>>,
    fin_acked_tx: oneshot::Sender<()>,
}

impl<N: Net> FinWait1<N> {
    async fn handle_packet<'a>(
        self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) -> TcpState<N> {
        let ack = tcp_header.ack();
        let fin = tcp_header.fin();

        let fin_seq_no = *self.fin_seq_no.lock().await;

        if fin && ack {
            // Remote closed too
            let ack_packet = self.make_ack_packet(
                tcp_header,
                ip_header.destination_addr(),
                ip_header.destination_addr(),
            );
            self.net
                .send(&ack_packet, Protocol::Tcp, self.remote_ip)
                .await
                .map_err(|_| TransportError::DestUnreachable(self.remote_ip))
                .unwrap();

            if let Some(fin_seq_no) = fin_seq_no {
                if tcp_header.acknowledgment_number() > fin_seq_no as u32 {
                    self.fin_acked_tx.send(()).unwrap();
                }
            }

            let state = TimeWait {};
            return state.into();
        }

        if fin {
            // Simultaneous close

            let ack_packet = self.make_ack_packet(
                tcp_header,
                ip_header.destination_addr(),
                ip_header.destination_addr(),
            );
            self.net
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
                if tcp_header.acknowledgment_number() > fin_seq_no as u32 {
                    self.fin_acked_tx.send(()).unwrap();

                    let state = FinWait2 {
                        conn: self.conn,
                        local_port: self.local_port,
                        remote_ip: self.remote_ip,
                        remote_port: self.remote_port,
                        net: self.net,
                    };
                    return state.into();
                }
            }
        }

        // Handle normal traffic
        self.conn
            .handle_packet(ip_header, tcp_header, payload)
            .await;
        self.into()
    }

    fn make_ack_packet(
        &self,
        tcp_header: &TcpHeaderSlice<'_>,
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            self.local_port.0,
            self.remote_port.0,
            tcp_header.acknowledgment_number(),
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.ack = true;
        header.acknowledgment_number = tcp_header.sequence_number() + 1;
        let payload: &[u8] = &[];
        let checksum = header
            .calc_checksum_ipv4_raw(src_ip.octets(), dst_ip.octets(), payload)
            .unwrap();
        header.checksum = checksum;
        header.write(&mut bytes).unwrap();
        bytes
    }
}

struct FinWait2<N> {
    local_port: Port,
    remote_ip: Ipv4Addr,
    remote_port: Port,
    conn: TcpConn,
    net: Arc<N>,
}

impl<N: Net> FinWait2<N> {
    async fn handle_packet<'a>(
        self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) -> TcpState<N> {
        if tcp_header.fin() {
            self.handle_fin(ip_header, tcp_header).await.into()
        } else {
            self.conn
                .handle_packet(ip_header, tcp_header, payload)
                .await;
            self.into()
        }
    }

    async fn handle_fin<'a>(
        &self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
    ) -> TimeWait {
        let ack_packet = self.make_ack_packet(
            tcp_header,
            ip_header.destination_addr(),
            ip_header.source_addr(),
        );
        self.net
            .send(&ack_packet, Protocol::Tcp, self.remote_ip)
            .await
            .unwrap();
        TimeWait {}
    }

    fn make_ack_packet(
        &self,
        tcp_header: &TcpHeaderSlice<'_>,
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut header = TcpHeader::new(
            self.local_port.0,
            self.remote_port.0,
            tcp_header.acknowledgment_number(),
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.ack = true;
        header.acknowledgment_number = tcp_header.sequence_number() + 1;
        let payload: &[u8] = &[];
        let checksum = header
            .calc_checksum_ipv4_raw(src_ip.octets(), dst_ip.octets(), payload)
            .unwrap();
        header.checksum = checksum;
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

impl TimeWait {}

struct CloseWait<N> {
    conn: TcpConn,
    last_seq: u32,
    net: Arc<N>,
}

impl<N: Net> CloseWait<N> {
    async fn close(&self, id: SocketId, local_port: Port) -> LastAck<N> {
        // FIXME: send this packet?
        let _fin_packet = self
            .make_fin_packet(local_port, id.remote_port(), id.remote_ip())
            .await;
        self.conn.close().await;
        LastAck {
            net: self.net.clone(),
        }
    }

    async fn handle_packet<'a>(
        self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) -> TcpState<N> {
        if !payload.is_empty() {
            log::warn!(
                "Got payload when the remote has already closed their end of the connection"
            );
        }
        self.conn.handle_packet(ip_header, tcp_header, &[]).await;
        self.into()
    }

    async fn make_fin_packet(&self, src_port: Port, dst_port: Port, dst_ip: Ipv4Addr) -> Vec<u8> {
        let mut bytes = Vec::new();

        let mut header = TcpHeader::new(
            src_port.0,
            dst_port.0,
            self.last_seq + 1,
            TCP_DEFAULT_WINDOW_SZ.try_into().unwrap(),
        );
        header.ack = false;
        header.fin = true;

        let payload: &[u8] = &[];
        let src_ip = self.net.get_outbound_ip(dst_ip).await.unwrap();
        let checksum = header
            .calc_checksum_ipv4_raw(src_ip, dst_ip.octets(), payload)
            .unwrap();
        header.checksum = checksum;
        header.write(&mut bytes).unwrap();
        bytes
    }
}

struct LastAck<N> {
    net: Arc<N>,
}

impl<N: Net> LastAck<N> {
    fn handle_ack(self) -> Closed<N> {
        Closed::new(self.net)
    }
}

#[derive(Debug)]
pub enum ListenTransitionError {
    // Errs when attempting to transition into Listen state from a state that's
    // not Closed.
    NotFromClosed,
}

pub enum UpdateAction<N: Net> {
    NewSynReceivedSocket(SynReceived<N>),
    CloseSocket(SocketId),
}

pub struct Socket<N: Net> {
    id: SocketId,
    descriptor: SocketDescriptor,
    state: Mutex<Option<TcpState<N>>>,
}

impl<N: Net> Socket<N> {
    pub fn new(id: SocketId, descriptor: SocketDescriptor, net: Arc<N>) -> Self {
        Self {
            id,
            descriptor,
            state: Mutex::new(Some(TcpState::new(net))),
        }
    }

    fn with_state(id: SocketId, descriptor: SocketDescriptor, state: TcpState<N>) -> Self {
        Self {
            id,
            descriptor,
            state: Mutex::new(Some(state)),
        }
    }

    pub async fn listen(&self, port: Port) -> Result<TcpListener, ListenTransitionError> {
        let mut state_guard = self.state.lock().await;
        let state = state_guard.take().expect("State should exist");

        match state {
            TcpState::Closed(s) => {
                let (new_conn_tx, new_conn_rx) = channel(1024);
                let listener = TcpListener::new(new_conn_rx);
                let new_state: TcpState<N> = s.listen(port, new_conn_tx).into();
                *state_guard = Some(new_state);
                Ok(listener)
            }
            _ => {
                *state_guard = Some(state);
                Err(ListenTransitionError::NotFromClosed)
            }
        }
    }

    pub fn id(&self) -> SocketId {
        self.id
    }

    pub fn descriptor(&self) -> SocketDescriptor {
        self.descriptor
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

    pub async fn status(&self) -> SocketStatus {
        let state_guard = self.state.lock().await;
        SocketStatus::from((*state_guard).as_ref().unwrap())
    }

    pub async fn send_all(&self, payload: &[u8]) -> Result<(), TcpSendError> {
        let conn = {
            let state_guard = self.state.lock().await;
            match (*state_guard).as_ref().unwrap() {
                TcpState::Established(s) => s.conn.clone(),
                TcpState::Listen(_) | TcpState::SynSent(_) | TcpState::SynReceived(_) => {
                    return Err(TcpSendError::ConnNotEstablished)
                }
                _ => return Err(TcpSendError::ConnClosed),
            }
        };
        conn.send_all(payload).await
    }

    /// Reads N bytes from the connection, where N is `out_buffer`'s size.
    pub async fn read_all(&self, out_buffer: &mut [u8]) -> Result<(), TcpReadError> {
        let conn = {
            let state_guard = self.state.lock().await;
            match (*state_guard).as_ref().unwrap() {
                TcpState::Established(s) => s.conn.clone(),
                TcpState::Listen(_) | TcpState::SynSent(_) | TcpState::SynReceived(_) => {
                    return Err(TcpReadError::ConnNotEstablished)
                }
                _ => return Err(TcpReadError::Closed(0)),
            }
        };
        conn.read_all(out_buffer).await
    }

    pub async fn initiate_connection(
        &self,
    ) -> Result<oneshot::Receiver<Result<TcpConn, TcpConnError>>, TcpConnError> {
        let mut state_guard = self.state.lock().await;
        let state = state_guard.take().expect("State should exist");
        match state {
            TcpState::Closed(s) => {
                let (established_rx, syn_sent) = s
                    .connect(self.local_port(), self.remote_ip_port())
                    .await
                    .map_err(TcpConnError::Transport)?;
                *state_guard = Some(syn_sent.into());
                Ok(established_rx)
            }
            _ => {
                *state_guard = Some(state);
                Err(TcpConnError::ConnectionExists(self.id.remote()))
            }
        }
    }

    pub async fn handle_packet<'a>(
        &self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) -> Option<UpdateAction<N>> {
        let mut state_guard = self.state.lock().await;
        let state = state_guard.take().expect("State should exist");

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
            TcpState::FinWait2(s) => {
                let new_state = s.handle_packet(ip_header, tcp_header, payload).await;
                match new_state {
                    TcpState::TimeWait(state) => {
                        (state.into(), Some(UpdateAction::CloseSocket(self.id)))
                    }
                    TcpState::FinWait2(state) => (state.into(), None),
                    _ => (new_state, None),
                }
            }
            TcpState::Closing(s) => {
                if tcp_header.ack() {
                    (s.handle_ack().into(), None)
                } else {
                    (s.into(), None)
                }
            }
            TcpState::TimeWait(s) => (s.into(), None),
            TcpState::CloseWait(s) => (s.handle_packet(ip_header, tcp_header, payload).await, None),
            TcpState::LastAck(s) => (s.handle_ack().into(), None), //TODO return action that triggers socket table pruning
        };

        *state_guard = Some(next_state);
        action
    }

    pub async fn close(&self) {
        let mut state_guard = self.state.lock().await;
        let state = state_guard.take().expect("State should exist");
        match state {
            TcpState::Established(s) => {
                let state = s.active_close().await;
                *state_guard = Some(state.into());
            }
            TcpState::CloseWait(s) => {
                let state = s.close(self.id, self.local_port()).await;
                *state_guard = Some(state.into());
            }
            _ => {
                *state_guard = Some(state);
                eprintln!("Should not be able to close a connection that's not established");
            }
        }
    }

    pub async fn close_read(&self) {
        let mut state_guard = self.state.lock().await;
        let state = state_guard.take().expect("State should exist");
        match state {
            TcpState::Established(s) => {
                s.close_read().await;
                *state_guard = Some(s.into());
            }
            _ => {
                *state_guard = Some(state);
                eprintln!("Should not be able to close a connection that's not established");
            }
        }
    }

    pub async fn close_rw(&self) {
        let mut state_guard = self.state.lock().await;
        let state = state_guard.take().expect("State should exist");
        match state {
            TcpState::Established(s) => {
                s.close_read().await;
                let state = s.active_close().await;
                *state_guard = Some(state.into());
            }
            _ => {
                *state_guard = Some(state);
                eprintln!("Should not be able to close a connection that's not established");
            }
        }
    }

    pub async fn is_read_closed(&self) -> Option<bool> {
        self.state
            .lock()
            .await
            .as_ref()
            .expect("State should exist")
            .is_read_closed()
    }

    pub async fn as_table_entry_string(&self) -> String {
        let id = self.descriptor.0;
        let state = self.status().await;

        let local_window_sz = self.local_window_sz().await;
        let remote_window_sz = self.remote_window_sz().await;

        format!("{id}\t{state:?}\t\t{local_window_sz}\t\t\t{remote_window_sz}")
    }
}

impl<N: Net> Socket<N> {
    async fn local_window_sz(&self) -> usize {
        self.state
            .lock()
            .await
            .as_ref()
            .expect("State should exist")
            .local_window_sz()
            .await
    }

    async fn remote_window_sz(&self) -> usize {
        self.state
            .lock()
            .await
            .as_ref()
            .expect("State should exist")
            .remote_window_sz()
            .await
    }
}
