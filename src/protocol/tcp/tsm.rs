use crate::protocol::tcp::socket::{TcpConn, TcpListener};
use crate::protocol::tcp::{TcpListenError, TcpSendError};
use crate::protocol::Protocol;
use crate::route::{Router, SendError};
use async_trait::async_trait;
use etherparse::{Ipv4HeaderSlice, TcpHeader, TcpHeaderSlice};
use rand::random;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::oneshot;

/// Tcp State Machine (TSM)
///
/// responsible for handling the state of a single Tcp Connection
///
pub struct Socket {
    id: u16,
    pub state: Box<dyn TcpState>,
    local_window_size: usize,
    remote_window_size: Option<u16>,
    pub sender: oneshot::Sender<()>,
    pub receiver: Option<oneshot::Receiver<()>>,
    router: Arc<Router>,
}

pub enum SocketHandlerEvent {
    ReceivedSyn(Box<dyn TcpState>),
    ReceivedSynAck(Box<dyn TcpState>),
}

pub enum StateTransition {
    toSynSent,
    toEstablished,
    toClosed,
    toSynReceived,
}

#[async_trait]
pub trait TcpState: Send + Sync {
    async fn handle_packet<'a>(
        &mut self,
        ip_header: &Ipv4HeaderSlice,
        tcp_header: &TcpHeaderSlice,
        payload: &[u8],
        // TODO: remove this arg.
        // To perform update on the parent Socket, I think this fn can return
        // an Option<SocketHandlerAction>, where SocketHandlerAction is a list
        // of things the Socket should do on behalf of the tcp state.
        //
        // ```
    ) -> Option<SocketHandlerEvent>;

    async fn transition(&mut self, event: StateTransition) -> Option<Box<dyn TcpState>>;
}

pub struct Closed {
    port: u16,
}

pub struct Listen {
    port: u16,
    listener: TcpListener,
}

pub struct SynSent {
    conn: Option<TcpConn>,
}

pub struct SynReceive {}

pub struct Established {
    conn: TcpConn,
}

impl Closed {
    fn new(port: u16) -> Self {
        Closed { port }
    }

    pub async fn send_syn(&mut self, tsm: &mut Socket) -> Result<Socket, TcpSendError> {
        let (sender, receiver) = oneshot::channel();
        // TODO send syn packet
        let syn_sent = Socket {
            id: self.port,
            state: Box::new(SynSent {conn: None}),
            local_window_size: tsm.local_window_size,
            remote_window_size: None,
            sender,
            receiver: Some(receiver),
            router: tsm.router.clone(),
        };
        Ok(syn_sent)
    }
}

impl Listen {
    async fn send_syn_ack<'a>(
        &mut self,
        ip_header: &Ipv4HeaderSlice<'a>,
        tcp_header: &TcpHeaderSlice<'a>,
    ) {
        // TODO send syn ack packet. Move this to TSM-LEVEL
        todo!();
        // let src_port = tcp_header.destination_port();
        // let dst_port = tcp_header.source_port();
        // let ack_num = tcp_header.sequence_number() + 1;
        // let seq_num = random::<u32>();
        // let mut packet = TcpHeader::new(dst_port, src_port, seq_num, tsm.local_window_size as u16);
        // packet.ack = true;
        // packet.syn = true;
        // packet.acknowledgment_number = ack_num;
        // let payload = [];
        // packet.checksum = packet
        //     .calc_checksum_ipv4_raw(ip_header.destination(), ip_header.source(), &payload)
        //     .unwrap();
        // let mut buf = Vec::new();
        // let len = packet.write(&mut buf).unwrap();
    }
}

#[async_trait]
impl TcpState for Closed {
    async fn handle_packet<'a>(
        &mut self,
        ip_header: &Ipv4HeaderSlice,
        tcp_header: &TcpHeaderSlice,
        payload: &[u8],
    ) -> Option<SocketHandlerEvent> {
        None
    }

    async fn transition(&mut self, event: StateTransition) -> Option<Box<dyn TcpState>> {
        match event {
            StateTransition::toSynSent => {
                let syn_sent = SynSent::from(*self);
                Some(Box::new(syn_sent))
            }
            _ => None,
        }
    }
}

#[async_trait]
impl TcpState for Established {
    async fn handle_packet<'a>(
        &mut self,
        ip_header: &Ipv4HeaderSlice,
        tcp_header: &TcpHeaderSlice,
        payload: &[u8],
    ) -> Option<SocketHandlerEvent> {
        todo!();
    }

    async fn transition(&mut self, event: StateTransition) -> Option<Box<dyn TcpState>> {
        todo!();
    }
}

#[async_trait]
impl TcpState for Listen {
    async fn handle_packet<'a>(
        &mut self,
        ip_header: &Ipv4HeaderSlice,
        tcp_header: &TcpHeaderSlice,
        payload: &[u8],
    ) -> Option<SocketHandlerEvent> {
        if tcp_header.syn() {
            let syn_ack = self.send_syn_ack(ip_header, tcp_header).await;
        }
        todo!();
    }

    async fn transition(&mut self, event: StateTransition) -> Option<Box<dyn TcpState>> {
        todo!();
    }
}

impl From<SynSent> for Established {
    fn from(syn_sent: SynSent) -> Self {
        Established {
            conn: syn_sent.conn.unwrap(),
        }
    }
}

impl From<Listen> for SynReceive {
    fn from(listen: Listen) -> Self {
        SynReceive {}
    }
}

impl From<Closed> for SynSent {
    fn from(closed: Closed) -> Self {
        SynSent { conn: None }
    }
}

#[async_trait]
impl TcpState for SynSent {
    async fn handle_packet<'a>(
        &mut self,
        ip_header: &Ipv4HeaderSlice,
        tcp_header: &TcpHeaderSlice,
        payload: &[u8],
    ) -> Option<SocketHandlerEvent> {
        eprintln!("SYNSENT received packet");
        // hopefully it's a syn-ack
        if tcp_header.syn() && tcp_header.ack() {
            // it's a syn-ack, so we can move to established
        }
        eprintln!("SYN-SENT -> ESTABLISHED");
        None
    }
}

impl Socket {
    pub fn new(port: u16, router: Arc<Router>, local_window_size: usize) -> Self {
        let (sender, receiver) = oneshot::channel();
        Self {
            id: port,
            state: Box::new(Closed::new(port)),
            local_window_size,
            remote_window_size: None,
            sender,
            receiver: Some(receiver),
            router,
        }
    }

    pub async fn connect(&mut self, dst_addr: Ipv4Addr, dst_port: u16) -> Result<(), TcpSendError> {
        let mut state = self.state.transition(StateTransition::toSynSent).await;
        if let Some(s) = state {
            self.state = s;
        }
        Ok(())
    }

    pub async fn handle_packet<'a>(
        &mut self,
        ip_header: &Ipv4HeaderSlice<'a>,
        header: &TcpHeaderSlice<'a>,
        payload: &[u8],
    ) {
        match self.state
            .handle_packet(ip_header, header, payload).await {
            Some(SocketHandlerEvent::ReceivedSyn(s)) => {
                self.state = s;
                let random_sequence_number = random::<u32>();
                let mut packet = TcpHeader::new(
                    header.destination_port(),
                    header.source_port(),
                    random_sequence_number as u32,
                    self.local_window_size as u16,
                );
                packet.syn = true;
                packet.ack = true;
                packet.acknowledgment_number = header.sequence_number() + 1;
                eprintln!("Checksum: {}", packet.checksum);
                // TODO: send syn ack back to the "connection" connection
            }
            Some(SocketHandlerEvent::ReceivedSynAck(s)) => {
                self.state = s;
                // TODO: send ack back to the "listening" connection
                todo!();
            }
            None => {}
        }
    }
}
