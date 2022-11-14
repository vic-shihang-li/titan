use std::sync::atomic::AtomicUsize;

use etherparse::Ipv4HeaderSlice;

pub trait DropPolicy {
    fn should_drop(&self, ip_header: &Ipv4HeaderSlice<'_>) -> bool;
}

// TODO: use NeverDrop policy for better inline performance
#[allow(dead_code)]
#[derive(Default)]
pub struct NeverDrop;

impl DropPolicy for NeverDrop {
    fn should_drop(&self, _ip_header: &Ipv4HeaderSlice<'_>) -> bool {
        false
    }
}

pub struct DropFactor {
    never_drop: bool,
    factor: usize,
    count: AtomicUsize,
}

impl DropFactor {
    pub fn new(drop_factor: usize) -> Self {
        Self {
            never_drop: drop_factor == 0,
            factor: drop_factor,
            count: AtomicUsize::new(0),
        }
    }
}

impl DropPolicy for DropFactor {
    fn should_drop(&self, _ip_header: &Ipv4HeaderSlice<'_>) -> bool {
        if self.never_drop {
            return false;
        }

        let count = self
            .count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        count % self.factor == 0
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use etherparse::Ipv4Header;

    use super::*;

    #[test]
    fn drop_rate() {
        // drop 20% packet, or once every 5 packets.
        let dropper = DropFactor::new(5);

        let mut bytes = Vec::new();
        let ip_header_slice = {
            let ip_header = Ipv4Header::new(
                10,
                10,
                10,
                Ipv4Addr::new(0, 0, 0, 0).octets(),
                Ipv4Addr::new(0, 0, 0, 0).octets(),
            );
            ip_header.write(&mut bytes).unwrap();
            Ipv4HeaderSlice::from_slice(&bytes).unwrap()
        };

        let mut dropped: usize = 0;
        let iters = 1_000_000;
        for _ in 0..iters {
            if dropper.should_drop(&ip_header_slice) {
                dropped += 1;
            }
        }

        assert_eq!(dropped as f64, (iters as f64 * 0.2).floor());
    }
}
