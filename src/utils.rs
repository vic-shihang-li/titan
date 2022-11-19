use std::future::Future;
use std::time::Duration;

pub async fn loop_with_interval<Fut: Future<Output = ()>>(interval: Duration, f: impl Fn() -> Fut) {
    loop {
        f().await;
        tokio::time::sleep(interval).await;
    }
}

pub mod sync {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    };

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

    /// Like Notifier, this utility allows a sender to notify a waiter.
    /// Unlike Notifier, there is at most one pending notification in flight.
    /// If the waiter hasn't consumed a notification yet, subsequent sender
    /// notifications are ignored.
    #[derive(Debug)]
    pub struct DedupedNotifier {
        flag: AtomicBool,
        notifier: Notifier,
    }

    impl DedupedNotifier {
        pub fn new() -> Self {
            Self {
                flag: AtomicBool::new(false),
                notifier: Notifier::default(),
            }
        }

        pub fn notify(&self) -> Result<(), ()> {
            if self
                .flag
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                self.notifier.notify_all();
                Ok(())
            } else {
                Err(())
            }
        }

        pub async fn wait(&self) {
            let waiter = self.notifier.notified();
            waiter.wait().await;
            self.flag.store(false, Ordering::SeqCst);
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
