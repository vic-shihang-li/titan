use std::sync::atomic::{AtomicUsize, Ordering};

use etherparse::TcpHeaderSlice;

pub trait AckPolicy {
    fn should_ack(&self, tcp_header: &TcpHeaderSlice<'_>) -> bool;
}

/// Utility that always ACKs a packet on arrival.
#[derive(Default, Debug)]
pub struct AlwaysAck;

impl AckPolicy for AlwaysAck {
    #[inline]
    fn should_ack(&self, _tcp_header: &TcpHeaderSlice<'_>) -> bool {
        true
    }
}

/// Utility that allows a TcpConn to batch up to LAG Ack packets.
#[derive(Default, Debug)]
pub struct BatchedAck<const LAG: usize> {
    count: AtomicUsize,
}

impl<const LAG: usize> AckPolicy for BatchedAck<LAG> {
    #[inline]
    fn should_ack(&self, _tcp_header: &TcpHeaderSlice<'_>) -> bool {
        LAG == 0 || self.count.fetch_add(1, Ordering::Relaxed) % LAG == 0
    }
}
