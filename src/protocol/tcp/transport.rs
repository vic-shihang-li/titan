use std::{
    cmp::min,
    sync::Arc,
    time::{Duration, Instant},
};

use etherparse::TcpHeader;
use tokio::sync::{
    broadcast,
    broadcast::error::RecvError::{Closed, Lagged},
    oneshot,
};

use crate::{
    net::{Net, SendError},
    protocol::Protocol,
};

use super::{
    buf::{RecvBuf, SendBuf, SliceError},
    Port, Remote, MAX_SEGMENT_SZ, TCP_DEFAULT_WINDOW_SZ,
};

const TCP_DEFAULT_RTX_TICK_INTERVAL: Duration = Duration::from_millis(10);

#[allow(unused)]
const TCP_DEFAULT_INITIAL_RTO: Duration = Duration::from_millis(10);

#[allow(unused)]
struct RtxRequest {
    seq_no: usize,
    tx_time: Instant,
}

#[allow(unused)]
impl RtxRequest {
    fn new(seq_no: usize, tx_time: Instant) -> Self {
        Self { seq_no, tx_time }
    }

    async fn tx_time(&self) -> Instant {
        self.tx_time
    }
}

impl PartialEq for RtxRequest {
    fn eq(&self, other: &Self) -> bool {
        self.seq_no == other.seq_no
    }
}

impl Eq for RtxRequest {}

impl PartialOrd for RtxRequest {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.seq_no.partial_cmp(&other.seq_no)
    }
}

impl Ord for RtxRequest {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.seq_no.cmp(&other.seq_no)
    }
}

/// Dynamically updated retransmission timeout, based on RFC6298:
///
/// https://www.rfc-editor.org/rfc/rfc6298.html.
#[allow(unused)]
struct DynamicRto {
    /// smoothed round-trip time
    srtt: Option<Duration>,
    /// round-trip time variation
    rtt_var: Option<Duration>,
    /// retransmission timeout
    rto: Duration,
    /// timer tick interval
    tick: Duration,

    // parameters for smoothening
    alpha: f64,
    beta: f64,
}

#[allow(unused)]
impl DynamicRto {
    fn new(tick_interval: Duration, initial_rto: Duration) -> Self {
        Self {
            srtt: None,
            rtt_var: None,
            rto: initial_rto,
            tick: tick_interval,
            alpha: 0.125,
            beta: 0.24,
        }
    }

    fn update(&mut self, rtt: Duration) {
        const K: u32 = 4;

        match (self.srtt, self.rtt_var) {
            (None, None) => {
                self.srtt = Some(rtt);
                self.rtt_var = Some(rtt / 2);
                self.rto = std::cmp::max(self.tick, K * self.rtt_var.unwrap());
            }
            (Some(srtt), Some(rtt_var)) => {
                let (alpha, beta) = (self.alpha, self.beta);

                // RTTVAR <- (1 - beta) * RTTVAR + beta * |SRTT - R'|
                // SRTT <- (1 - alpha) * SRTT + alpha * R'

                let srtt_rtt_diff = {
                    if srtt > rtt {
                        srtt - rtt
                    } else {
                        rtt - srtt
                    }
                };

                let rtt_var = rtt_var.mul_f64(1f64 - beta) + srtt_rtt_diff.mul_f64(beta);
                let srtt = srtt.mul_f64(1f64 - alpha) + rtt.mul_f64(alpha);

                self.srtt = Some(srtt);
                self.rtt_var = Some(rtt_var);

                // RTO <- SRTT + max (G, K*RTTVAR)
                self.rto = srtt + std::cmp::max(self.tick, K * rtt_var);
            }
            _ => unreachable!(),
        }
    }

    fn rto(&self) -> Instant {
        Instant::now() + self.rto
    }
}

pub struct TcpTransport<const BUF_SZ: usize, N: Net> {
    send_buf: SendBuf<BUF_SZ>,
    recv_buf: RecvBuf<BUF_SZ>,
    remote: Remote,
    local_port: Port,
    net: Arc<N>,
    seq_no: usize,
    last_transmitted: Instant,
    ack_batch_timeout: Duration,
    last_acked: usize,
    zero_window_probe_interval: Duration,
    send_ack_request: broadcast::Receiver<()>,
    last_ack_transmitted: usize,
    remaining_window_sz: usize,
}

enum NextSendDecision {
    NextSegmentSize(usize),
    SendBufClosed,
}

impl<const BUF_SZ: usize, N: Net> TcpTransport<BUF_SZ, N> {
    pub async fn init(
        send_buf: SendBuf<BUF_SZ>,
        recv_buf: RecvBuf<BUF_SZ>,
        remote: Remote,
        local_port: Port,
        net: Arc<N>,
        should_ack: broadcast::Receiver<()>,
    ) -> Self {
        let seq_no = send_buf.tail().await;
        Self {
            send_buf,
            recv_buf,
            remote,
            local_port,
            net,
            seq_no,
            last_transmitted: Instant::now(),
            ack_batch_timeout: Duration::from_millis(1),
            last_acked: 0,
            zero_window_probe_interval: Duration::from_millis(1),
            send_ack_request: should_ack,
            last_ack_transmitted: 0,
            remaining_window_sz: TCP_DEFAULT_WINDOW_SZ,
        }
    }

    pub async fn run(mut self) {
        let mut segment = [0; MAX_SEGMENT_SZ];
        let mut segment_sz = MAX_SEGMENT_SZ;

        // Set upper bound on how long before acks are sent back to sender.
        let mut transmit_ack_interval = tokio::time::interval(self.ack_batch_timeout);
        let mut zero_window_probe_interval = tokio::time::interval(self.zero_window_probe_interval);
        let mut rtx_tick = tokio::time::interval(TCP_DEFAULT_RTX_TICK_INTERVAL);
        let mut window_sz_update = self.send_buf.window_size_update();
        let mut last_acked_update = self.send_buf.tail_update();

        loop {
            tokio::select! {
                _ = self.send_buf.wait_for_new_data(self.seq_no) => {
                    segment_sz = min(segment_sz, self.remaining_window_sz);
                    if segment_sz == 0  {
                        segment_sz = min(self.remaining_window_sz, MAX_SEGMENT_SZ);
                    }
                    if segment_sz > 0 {
                        match self.try_consume_and_send(&mut segment[..segment_sz]).await {
                            NextSendDecision::NextSegmentSize(sz) => segment_sz = sz,
                            NextSendDecision::SendBufClosed => break,
                        }
                    }
                }
                Ok(window_sz) = window_sz_update.recv() => {
                    self.remaining_window_sz = window_sz.into();
                }
                Ok(next_expected_seq_no) = last_acked_update.recv() => {
                    self.on_last_byte_acked_updated(next_expected_seq_no).await;
                }
                _ = zero_window_probe_interval.tick() => {
                    self.check_and_zero_window_probe().await;
                }
                _ = transmit_ack_interval.tick() => {
                    self.check_and_retransmit_ack().await;
                }
                o = self.send_ack_request.recv() => {
                    match o {
                        Ok(_) | Err(Lagged(_)) => {
                            self.send_ack().await.ok();
                        },
                        Err(Closed) => {
                            break; // connection closed
                        }
                    }
                }
                _ = rtx_tick.tick() => {
                    self.check_retransmission(&mut segment).await;
                }
            }
        }
    }

    async fn try_consume_and_send(&mut self, buf: &mut [u8]) -> NextSendDecision {
        match self.send_buf.try_slice(self.seq_no, buf).await {
            Ok(bytes_readable) => {
                // TODO: handle send failure
                if self.send(self.seq_no, buf).await.is_ok() {
                    self.seq_no += buf.len();
                    self.remaining_window_sz -= buf.len();

                    let next_seg_sz = min(
                        MAX_SEGMENT_SZ,
                        min(self.remaining_window_sz, bytes_readable),
                    );
                    NextSendDecision::NextSegmentSize(next_seg_sz)
                } else {
                    NextSendDecision::NextSegmentSize(buf.len())
                }
            }
            Err(e) => match e {
                SliceError::OutOfRange(unconsumed_sz) => {
                    if unconsumed_sz == 0 && self.send_buf.closed() {
                        return NextSendDecision::SendBufClosed;
                    }
                    NextSendDecision::NextSegmentSize(min(unconsumed_sz, self.remaining_window_sz))
                }
                SliceError::StartSeqTooLow(next_seq_no) => {
                    // SendBuf's tail has been advanced due to zero probing.
                    self.seq_no = next_seq_no;

                    NextSendDecision::NextSegmentSize(min(self.remaining_window_sz, MAX_SEGMENT_SZ))
                }
            },
        }
    }

    async fn check_and_zero_window_probe(&mut self) {
        if self.remaining_window_sz == 0 {
            self.zero_window_probe().await;
        }
    }

    async fn check_and_retransmit_ack(&mut self) {
        if self.last_transmitted.elapsed() > self.ack_batch_timeout {
            let curr_ack = self.recv_buf.head().await;
            if curr_ack != self.last_ack_transmitted {
                self.send_ack().await.ok();
            }
        }
    }

    async fn check_retransmission(&mut self, segment_buf: &mut [u8]) {
        if self
            .send_buf
            .try_slice(self.last_acked, segment_buf)
            .await
            .is_ok()
        {
            log::info!("Retransmitting {}", self.last_acked);
            // TODO: handle failure
            self.send(self.last_acked, segment_buf).await.unwrap();
        }
    }

    async fn on_last_byte_acked_updated(&mut self, next_expected_seq_no: usize) {
        self.last_acked = next_expected_seq_no;
    }

    async fn zero_window_probe(&mut self) {
        let mut buf = [0; 1];
        if self.send_buf.try_slice(self.seq_no, &mut buf).await.is_ok() {
            self.send(self.seq_no, &buf).await.ok();
        }
    }

    async fn send_ack(&mut self) -> Result<(), SendError> {
        // The empty-payload packet's main purpose is to update the remote
        // about our latest ACK sequence number.
        self.send(self.seq_no, &[]).await
    }

    async fn send(&mut self, seq_no: usize, payload: &[u8]) -> Result<(), SendError> {
        let mut bytes = Vec::new();
        let mut tcp_header = self.prepare_tcp_packet(seq_no).await;

        let src_ip = self
            .net
            .get_outbound_ip(self.remote.ip())
            .await
            .expect("Cannot find remote in the IP layer");
        let checksum = tcp_header
            .calc_checksum_ipv4_raw(src_ip, self.remote.ip().octets(), payload)
            .unwrap();
        tcp_header.checksum = checksum;
        let ack = tcp_header.acknowledgment_number;
        tcp_header.write(&mut bytes).unwrap();
        bytes.extend_from_slice(payload);
        self.net
            .send(&bytes, Protocol::Tcp, self.remote.ip())
            .await
            .map(|_| {
                self.last_transmitted = Instant::now();
                self.last_ack_transmitted = ack.try_into().unwrap();
            })
    }

    async fn prepare_tcp_packet(&self, seq_no: usize) -> TcpHeader {
        let src_port = self.local_port.0;
        let dst_port = self.remote.port().0;
        let seq_no = seq_no.try_into().expect("seq no overflow");
        let window_sz = self.recv_buf.window_size().await.try_into().unwrap();

        let mut header = TcpHeader::new(src_port, dst_port, seq_no, window_sz);
        header.ack = true;
        header.acknowledgment_number = self.recv_buf.head().await.try_into().unwrap();
        header
    }
}

pub struct RtxConfig {
    max_err_retries: usize,
    rtx_interval: Duration,
    timeout: Duration,
}

impl Default for RtxConfig {
    fn default() -> Self {
        Self {
            max_err_retries: 80,
            rtx_interval: Duration::from_millis(50),
            timeout: Duration::from_secs(3),
        }
    }
}

pub fn transport_single_message<F: FnOnce(TransmissionError) + Send + Sync + 'static, N: Net>(
    payload: Vec<u8>,
    remote: Remote,
    router: Arc<N>,
    cfg: RtxConfig,
    on_err: F,
) -> AckHandle {
    let (acked_tx, acked_rx) = oneshot::channel();

    tokio::spawn(async move {
        let transporter = SingleMessageTransport {
            payload,
            remote,
            net: router,
            rtx_cfg: cfg,
            acked_rx,
            on_err,
        };
        transporter.run().await;
    });

    AckHandle {
        acked_tx: Some(acked_tx),
    }
}

pub struct AckHandle {
    acked_tx: Option<oneshot::Sender<()>>,
}

impl AckHandle {
    pub fn acked(&mut self) {
        self.acked_tx
            .take()
            .expect("Ack handle should only be acked once")
            .send(())
            .ok();
    }
}

impl Drop for AckHandle {
    fn drop(&mut self) {
        // if didn't manually ack
        if self.acked_tx.is_some() {
            // assume dropping the handle means successful ack
            self.acked_tx.take().unwrap().send(()).ok();
        }
    }
}

#[derive(Debug)]
pub enum TransmissionError {
    Timeout,
    MaxRetransExceeded,
}

/// TCP Transporter that ensures delivery of only one packet.
struct SingleMessageTransport<F: FnOnce(TransmissionError) + Send, N: Net> {
    payload: Vec<u8>,
    remote: Remote,
    net: Arc<N>,
    rtx_cfg: RtxConfig,
    acked_rx: oneshot::Receiver<()>,
    on_err: F,
}

impl<F: FnOnce(TransmissionError) + Send, N: Net> SingleMessageTransport<F, N> {
    pub async fn run(mut self) {
        match tokio::time::timeout(self.rtx_cfg.timeout, self.transmission_loop()).await {
            Ok(succeeded) => {
                if !succeeded {
                    (self.on_err)(TransmissionError::MaxRetransExceeded);
                }
            }
            Err(_) => {
                // timed out
                (self.on_err)(TransmissionError::Timeout);
            }
        }
    }

    /// Transmit repeatedly, returning true if acked.
    async fn transmission_loop(&mut self) -> bool {
        let mut retried = 0;

        let mut retransmit = tokio::time::interval(self.rtx_cfg.rtx_interval);
        loop {
            tokio::select! {
                _ = retransmit.tick() => {
                    if self.send().await.is_err() {
                        retried += 1;
                        if retried >= self.rtx_cfg.max_err_retries {
                            return false;
                        }
                    }
                },
                _ = &mut self.acked_rx => {
                    return true;
                }
            }
        }
    }

    /// Sends the payload to transport to the remote.
    ///
    /// Errs when failed over the max retry limit. Otherwise, forward the send
    /// result to the caller.
    async fn send(&self) -> Result<(), SendError> {
        self.net
            .send(&self.payload, Protocol::Tcp, self.remote.ip())
            .await
    }
}
