use std::future::Future;
use std::time::Duration;

pub async fn loop_with_interval<Fut: Future<Output = ()>>(interval: Duration, f: impl Fn() -> Fut) {
    loop {
        f().await;
        tokio::time::sleep(interval).await;
    }
}

/// Time any arbitrary expression's execution time.
#[macro_export]
macro_rules! timed {
    ($e: expr) => {{
        let t = Instant::now();
        $e;
        t.elapsed()
    }};
}

pub mod net {
    use std::net::{Ipv4Addr, SocketAddr};

    use etherparse::Ipv4Header;

    pub fn localhost_with_port(port: u16) -> SocketAddr {
        SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port)
    }

    #[derive(Default, Copy, Clone)]
    pub struct Ipv4PacketBuilder<'a> {
        payload: Option<&'a [u8]>,
        ttl: Option<u8>,
        protocol: Option<u8>,
        src: Option<Ipv4Addr>,
        dst: Option<Ipv4Addr>,
    }

    #[derive(Debug)]
    pub enum BuildError {
        NoPayload,
        NoProtocol,
        NoSourceAddress,
        NoDestinationAddress,
        PayloadTooLong,
    }

    impl<'a> Ipv4PacketBuilder<'a> {
        pub fn with_payload(&mut self, payload: &'a [u8]) -> &mut Self {
            self.payload = Some(payload);
            self
        }

        pub fn with_ttl(&mut self, ttl: u8) -> &mut Self {
            self.ttl = Some(ttl);
            self
        }

        pub fn with_protocol<P: Into<u8>>(&mut self, protocol: P) -> &mut Self {
            self.protocol = Some(protocol.into());
            self
        }

        pub fn with_src(&mut self, src: Ipv4Addr) -> &mut Self {
            self.src = Some(src);
            self
        }

        pub fn with_dst(&mut self, dst: Ipv4Addr) -> &mut Self {
            self.dst = Some(dst);
            self
        }

        pub fn build(self) -> Result<Vec<u8>, BuildError> {
            let mut buf = Vec::new();
            let payload = self.payload.ok_or(BuildError::NoPayload)?;
            let payload_len: u16 = payload
                .len()
                .try_into()
                .map_err(|_| BuildError::PayloadTooLong)?;
            let protocol = self.protocol.ok_or(BuildError::NoProtocol)?;
            let src = self.src.ok_or(BuildError::NoSourceAddress)?;
            let dst = self.dst.ok_or(BuildError::NoDestinationAddress)?;
            let ttl = self.ttl.unwrap_or_else(Ipv4PacketBuilder::default_ttl);

            let ip_header = Ipv4Header::new(payload_len, ttl, protocol, src.octets(), dst.octets());

            ip_header
                .write(&mut buf)
                .expect("IP header serialization error");

            buf.extend_from_slice(payload);

            Ok(buf)
        }

        fn default_ttl() -> u8 {
            15
        }
    }
}

pub mod sync {
    use std::sync::{Arc, Mutex as StdMutex};

    use tokio::sync::broadcast;

    /// A condition-variable like notification utility.
    ///
    /// ```ignore
    /// use tokio::sync::Mutex;
    /// use std::sync::Arc;
    /// use crate::utils::sync::Notifier;
    ///
    /// struct State;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let notifier = Notifier::new();
    ///     let notifier2 = notifier.clone();
    ///
    ///     let state = Arc::new(Mutex::new(State {}));
    ///     let state2 = state.clone();
    ///
    ///     tokio::spawn(async move {
    ///         let state_guard = state2.lock().await;
    ///         // want to wait for some condition to become true...
    ///
    ///         // 1. Register interest
    ///         let handle = notifier2.notified();
    ///
    ///         // 2. Release mutex (to let others in, so the waited condition
    ///         // can become true)
    ///         drop(state_guard);
    ///
    ///         // 3. Block until notified.
    ///         handle.wait().await;
    ///
    ///
    ///         // 4. Validate the waited condition has become true.
    ///         // ...
    ///     });
    ///
    ///     // perform some update on state
    ///     notifier.notify_all();
    /// }
    ///
    /// ```
    #[derive(Clone, Debug)]
    pub struct Notifier {
        tx: broadcast::Sender<()>,
        // Keep at least one Receiver alive to keep the broadcast channel alive.
        _rx: Arc<broadcast::Receiver<()>>,
    }

    impl Default for Notifier {
        fn default() -> Self {
            let (tx, rx) = broadcast::channel(1024);
            Self {
                tx,
                _rx: Arc::new(rx),
            }
        }
    }

    impl Notifier {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn notify_all(&self) {
            self.tx.send(()).unwrap();
        }

        pub fn notified(&self) -> NotificationHandle {
            let notifier = self.tx.subscribe();
            NotificationHandle(notifier)
        }
    }

    pub struct NotificationHandle(broadcast::Receiver<()>);

    impl NotificationHandle {
        pub async fn wait(mut self) {
            loop {
                match self.0.recv().await {
                    Ok(_) => break,
                    Err(e) => match e {
                        broadcast::error::RecvError::Lagged(_) => break,
                        _ => unreachable!(),
                    },
                }
            }
        }
    }

    #[derive(Debug)]
    pub enum SendError {
        AlreadySent,
        ReceiverDropped,
    }

    /// Like `tokio::sync::oneshot::Sender`, but allows multiple threads to
    /// race to send the oneshot message.
    ///
    /// Unlike `tokio::sync::oneshot::Sender`, which statically guarantees up
    /// to one message is sent, this struct gives a runtime SendError for
    /// threads that failed the race to send the oneshot message.
    #[derive(Debug, Clone)]
    pub struct RaceOneShotSender<T> {
        // Note: we use Mutex from the std library here to avoid making send()
        // async, just like the tokio version of oneshot::Sender.
        inner: Arc<StdMutex<Option<tokio::sync::oneshot::Sender<T>>>>,
    }

    impl<T> From<tokio::sync::oneshot::Sender<T>> for RaceOneShotSender<T> {
        fn from(s: tokio::sync::oneshot::Sender<T>) -> Self {
            Self {
                inner: Arc::new(StdMutex::new(Some(s))),
            }
        }
    }

    impl<T> RaceOneShotSender<T> {
        pub fn send(&self, message: T) -> Result<(), SendError> {
            let mut sender = self.inner.lock().unwrap();
            if let Some(sender) = sender.take() {
                sender
                    .send(message)
                    .map_err(|_| SendError::ReceiverDropped)?;
                Ok(())
            } else {
                Err(SendError::AlreadySent)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod net {

        use std::net::Ipv4Addr;

        use etherparse::Ipv4Header;

        use crate::utils::net::Ipv4PacketBuilder;

        #[test]
        fn build_packet() {
            let payload = vec![1; 16];
            let src = Ipv4Addr::new(127, 0, 0, 1);
            let dst = Ipv4Addr::new(127, 0, 0, 2);
            let ttl = 10;
            let protocol = 12;

            let packet_bytes = Ipv4PacketBuilder::default()
                .with_src(src)
                .with_dst(dst)
                .with_ttl(ttl)
                .with_payload(payload.as_slice())
                .with_protocol(protocol)
                .build()
                .unwrap();

            let mut expected = Vec::new();
            Ipv4Header::new(
                payload.len().try_into().unwrap(),
                ttl,
                protocol,
                src.octets(),
                dst.octets(),
            )
            .write(&mut expected)
            .unwrap();
            expected.extend_from_slice(&payload);

            assert_eq!(expected, packet_bytes);
        }
    }
}
