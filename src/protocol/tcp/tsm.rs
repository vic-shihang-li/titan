use std::net::Ipv4Addr;
use std::sync::Arc;
use async_trait::async_trait;
use etherparse::{Ipv4HeaderSlice, TcpHeader, TcpHeaderSlice};
use rand::random;
use crate::protocol::tcp::socket::{TcpConn, TcpListener};
use crate::protocol::tcp::{TcpListenError, TcpSendError};
use tokio::sync::oneshot;
use crate::protocol::Protocol;
use crate::route::{Router, SendError};

/// Tcp State Machine (TSM)
///
/// responsible for handling the state of a single Tcp Connection
pub struct Socket {
    id: u16,
    pub state: Box<dyn TcpState>,
    local_window_size: usize,
    remote_window_size: Option<u16>,
    sender: oneshot::Sender<()>,
    receiver: oneshot::Receiver<()>,
    router: Arc<Router>,
}


#[async_trait]
pub trait TcpState: Send + Sync {
    async fn handle_packet<'a>(&mut self, ip_header: &Ipv4HeaderSlice, tcp_header: &TcpHeaderSlice, payload: &[u8], tsm: &mut Socket)
    where
        Self: Sized;
}

struct Closed { port: u16, }

struct Listen { port: u16, listener: TcpListener}

struct SynSent { }

struct SynReceive { }

struct Established {
    conn: Arc<TcpConn>
}

impl Closed {
    fn new(port: u16) -> Self {
        Closed {
            port,
        }
    }

    async fn send_syn(&mut self, tsm: &mut Socket) -> Result<Socket, TcpSendError> {
        let (sender, receiver) = oneshot::channel();
        // TODO send syn packet
        let syn_sent = Socket {
            id: self.port,
            state: Box::new(SynSent {}),
            local_window_size: tsm.local_window_size,
            remote_window_size: None,
            sender,
            receiver,
            router: tsm.router.clone(),
        };
        Ok(syn_sent)
    }
}

impl Listen {
    async fn send_syn_ack<'a>(&mut self, ip_header: &Ipv4HeaderSlice<'a>, tcp_header: &TcpHeaderSlice<'a>, tsm: &mut Socket) -> Result<(), TcpSendError> {
        // TODO send syn ack packet
        let src_port = tcp_header.destination_port();
        let dst_port = tcp_header.source_port();
        let ack_num = tcp_header .sequence_number() + 1;
        let seq_num = random::<u32>();
        let mut packet = TcpHeader::new(dst_port,
                                    src_port,
                                    seq_num,
                                    tsm.local_window_size as u16,
        );
        packet.ack = true;
        packet.syn = true;
        packet.acknowledgment_number = ack_num;
        let payload = [];
        packet.checksum = packet.calc_checksum_ipv4_raw(ip_header.destination(),
                                                        ip_header.source(),
                                                        &payload).unwrap();
        let mut buf = Vec::new();
        let len = packet.write(&mut buf).unwrap();
        tsm.router.send(buf.as_slice(), Protocol::Tcp, ip_header.source_addr()).await.map_err(|e| TcpSendError {})

    }
}


#[async_trait]
impl TcpState for Established {
    async fn handle_packet<'a>(&mut self, ip_header: &Ipv4HeaderSlice, tcp_header: &TcpHeaderSlice, payload: &[u8], tsm: &mut Socket)
    where Self: Sized {
        todo!();
    }
}


#[async_trait]
impl TcpState for Listen {
    async fn handle_packet<'a>(&mut self, ip_header: &Ipv4HeaderSlice, tcp_header: &TcpHeaderSlice, payload: &[u8], tsm: &mut Socket)
    where
        Self: Sized,
    {
        if tcp_header.syn() {
            let syn_ack = self.send_syn_ack(ip_header, tcp_header, tsm).await.unwrap();
        }
    }
}

impl From<Listen> for SynReceive {
    fn from(listen: Listen) -> Self {
        SynReceive {}
    }
}

#[async_trait]
impl TcpState for SynSent {
    async fn handle_packet<'a>(&mut self, ip_header: &Ipv4HeaderSlice, tcp_header: &TcpHeaderSlice, payload: &[u8], tsm: &mut Socket)
    where
        Self: Sized,
    {
        tsm.sender.send(()).unwrap();
        tsm.state = Box::new(Established { conn: Arc::new(TcpConn {}) });
        eprintln!("SYN-SENT -> ESTABLISHED");
    }
}

impl Socket {


    pub fn new(port: u16, router: Arc<Router>, local_window_size: usize) -> Self {
        Self {
            id: port,
            state: Box::new(Closed::new(port)),
            local_window_size,
            remote_window_size: None,
            sender: oneshot::channel().0,
            receiver: oneshot::channel().1,
            router,
        }
    }

    pub async fn handle_packet<'a>(&mut self, ip_header: &Ipv4HeaderSlice<'a>, header: &TcpHeaderSlice<'a>, payload: &[u8]) {
        self.state.handle_packet(ip_header, header, payload, self).await.unwrap();
    }

    pub fn receiver(&self) -> &oneshot::Receiver<()> {
        &self.receiver
    }
}