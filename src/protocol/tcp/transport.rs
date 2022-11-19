use std::{
    cmp::min,
    sync::Arc,
    time::{Duration, Instant},
};

use etherparse::TcpHeader;
use tokio::sync::oneshot;

use crate::{
    protocol::Protocol,
    route::{Router, SendError},
    utils::sync::DedupedNotifier,
};

use super::{
    buf::{RecvBuf, SendBuf, SliceError},
    Port, Remote, MAX_SEGMENT_SZ, TCP_DEFAULT_WINDOW_SZ,
};

pub struct TcpTransport<const N: usize> {
    send_buf: SendBuf<N>,
    recv_buf: RecvBuf<N>,
    remote: Remote,
    local_port: Port,
    router: Arc<Router>,
    seq_no: usize,
    last_transmitted: Instant,
    ack_batch_timeout: Duration,
    retrans_interval: Duration,
    zero_window_probe_interval: Duration,
    send_ack_request: Arc<DedupedNotifier>,
    last_ack_transmitted: usize,
    remaining_window_sz: usize,
}

impl<const N: usize> TcpTransport<N> {
    pub async fn init(
        send_buf: SendBuf<N>,
        recv_buf: RecvBuf<N>,
        remote: Remote,
        local_port: Port,
        router: Arc<Router>,
        should_ack: Arc<DedupedNotifier>,
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
        let mut retrans_interval = tokio::time::interval(self.retrans_interval);
        let mut window_sz_update = self.send_buf.window_size_update();

        loop {
            tokio::select! {
                _ = self.send_buf.wait_for_new_data(self.seq_no) => {
                    segment_sz = min(segment_sz, self.remaining_window_sz);
                    if segment_sz == 0  {
                        segment_sz = min(self.remaining_window_sz, MAX_SEGMENT_SZ);
                    }
                    if segment_sz > 0 {
                        segment_sz = self.try_consume_and_send(&mut segment[..segment_sz]).await;
                    }
                }
                Ok(window_sz) = window_sz_update.recv() => {
                    self.remaining_window_sz = window_sz.into();
                }
                _ = zero_window_probe_interval.tick() => {
                    self.check_and_zero_window_probe().await;
                }
                _ = transmit_ack_interval.tick() => {
                    self.check_and_retransmit_ack().await;
                }
                _ = self.send_ack_request.wait() => {
                    self.send_ack().await.ok();
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

        match self.send_buf.try_slice(self.seq_no, buf).await {
            Ok(bytes_readable) => {
                // TODO: handle send failure
                if self.send(self.seq_no, buf).await.is_ok() {
                    self.seq_no += buf.len();
                    self.remaining_window_sz -= buf.len();
                    next_segment_sz = min(
                        MAX_SEGMENT_SZ,
                        min(self.remaining_window_sz, bytes_readable),
                    );
                }
            }
            Err(e) => match e {
                SliceError::OutOfRange(unconsumed_sz) => {
                    next_segment_sz = min(unconsumed_sz, self.remaining_window_sz);
                }
                SliceError::StartSeqTooLow(next_seq_no) => {
                    // send buf's tail has been advanced due to zero probing.
                    self.seq_no = next_seq_no;
                }
            },
        }

        next_segment_sz
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
        let (tail, last_ack_update_time) = self.send_buf.get_tail_and_age().await;

        if last_ack_update_time > self.retrans_interval {
            match self.send_buf.try_slice(tail, segment_buf).await {
                Ok(_) => {
                    self.send(tail, segment_buf).await.ok();
                }
                Err(e) => match e {
                    SliceError::OutOfRange(remaining_sz) => {
                        if remaining_sz == 0 {
                            return;
                        }
                        // Drain all unacked bytes.
                        // Here, if try_slice() errs, the bytes trying to be
                        // sliced must have been acked.
                        if self
                            .send_buf
                            .try_slice(tail, &mut segment_buf[..remaining_sz])
                            .await
                            .is_ok()
                        {
                            self.send(tail, &segment_buf[..remaining_sz]).await.ok();
                        }
                    }
                    SliceError::StartSeqTooLow(_) => {
                        // OK. The segment to retransmit was just acked.
                    }
                },
            }
        }
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
        let tcp_header = self.prepare_tcp_packet(seq_no).await;
        let ack = tcp_header.acknowledgment_number;
        tcp_header.write(&mut bytes).unwrap();
        bytes.extend_from_slice(payload);

        self.router
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
        header.syn = true;
        header.ack = true;
        header.acknowledgment_number = self.recv_buf.head().await.try_into().unwrap();

        header
    }
}

pub struct RetransmissionConfig {
    max_err_retries: usize,
    retrans_interval: Duration,
    timeout: Duration,
}

impl Default for RetransmissionConfig {
    fn default() -> Self {
        Self {
            max_err_retries: 3,
            retrans_interval: Duration::from_millis(50),
            timeout: Duration::from_secs(1),
        }
    }
}

pub fn transport_single_message<F: FnOnce(TransmissionError) + Send + Sync + 'static>(
    payload: Vec<u8>,
    remote: Remote,
    router: Arc<Router>,
    cfg: RetransmissionConfig,
    on_err: F,
) -> AckHandle {
    let (acked_tx, acked_rx) = oneshot::channel();

    tokio::spawn(async move {
        let transporter = SingleMessageTransport {
            payload,
            remote,
            router,
            retransmission_cfg: cfg,
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
struct SingleMessageTransport<F: FnOnce(TransmissionError) + Send> {
    payload: Vec<u8>,
    remote: Remote,
    router: Arc<Router>,
    retransmission_cfg: RetransmissionConfig,
    acked_rx: oneshot::Receiver<()>,
    on_err: F,
}

impl<F: FnOnce(TransmissionError) + Send> SingleMessageTransport<F> {
    pub async fn run(mut self) {
        match tokio::time::timeout(self.retransmission_cfg.timeout, self.transmission_loop()).await
        {
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

        let mut retransmit = tokio::time::interval(self.retransmission_cfg.retrans_interval);
        loop {
            tokio::select! {
                _ = retransmit.tick() => {
                    if self.send().await.is_err() {
                        retried += 1;
                        if retried >= self.retransmission_cfg.max_err_retries {
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
        self.router
            .send(&self.payload, Protocol::Tcp, self.remote.ip())
            .await
    }
}
