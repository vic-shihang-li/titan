use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    cmp::{max, min, Reverse},
    collections::BinaryHeap,
    sync::{atomic::AtomicU16, atomic::Ordering::SeqCst, Arc},
    time::{Duration, Instant},
    usize,
};

use tokio::sync::{broadcast, Mutex};

use crate::utils::sync::Notifier;

use super::TCP_DEFAULT_WINDOW_SZ;

#[derive(Debug)]
pub struct SendBufClosed;

/// The send-side TCP transmission buffer.
#[derive(Debug, Clone)]
pub struct SendBuf<const N: usize> {
    inner: Arc<Mutex<InnerSendBuf<N>>>,
    window_size: Arc<AtomicU16>,
    window_size_tx: broadcast::Sender<u16>,
    window_size_rx: Arc<broadcast::Receiver<u16>>,
    not_full: Notifier,
    empty: Notifier,
    written: Notifier,
    open: Arc<AtomicBool>,
    closing: Notifier,
}

impl<const N: usize> SendBuf<N> {
    /// Constructs a new SendBuf.
    pub fn new(initial_seq_no: usize) -> Self {
        let (window_size_tx, window_size_rx) = broadcast::channel(48);
        let window_size_rx = Arc::new(window_size_rx);
        Self {
            inner: Arc::new(Mutex::new(InnerSendBuf::new(initial_seq_no))),
            window_size: Arc::new(AtomicU16::new(N.try_into().unwrap())),
            window_size_tx,
            window_size_rx,
            not_full: Notifier::new(),
            empty: Notifier::new(),
            written: Notifier::new(),
            open: Arc::new(AtomicBool::new(true)),
            closing: Notifier::new(),
        }
    }

    /// Get the sequence number of the next, to-be-written, byte.
    pub async fn head(&self) -> usize {
        self.inner.lock().await.head
    }

    /// Get the last ACK, i.e., the next byte expected by the remote side.
    pub async fn tail(&self) -> usize {
        self.inner.lock().await.tail
    }

    /// Like tail(), but also returns the elapsed duration since the ACK was
    /// last updated.
    ///
    /// Note that the elapsed duration counts from the last time ACK was
    /// _mutated_ -- repeatedly setting ACK to the current ACK won't affect
    /// this timer. This conveniently ignores duplicated ACKs that may happen
    /// during transmission.
    pub async fn get_tail_and_age(&self) -> (usize, Duration) {
        let buf = self.inner.lock().await;
        (buf.tail, buf.last_tail_mutated.elapsed())
    }

    /// Set the window size that was sent to us by our remote.
    pub fn set_window_size(&self, window_size: u16) {
        self.window_size.store(window_size, SeqCst);
        self.window_size_tx
            .send(window_size)
            .expect("SendBuffer should maintain one subscriber");
    }

    /// Get the window size that was sent to us by our remote.
    pub fn window_size(&self) -> u16 {
        self.window_size.load(SeqCst)
    }

    /// Get notified when window size is updated.
    pub fn window_size_update(&self) -> broadcast::Receiver<u16> {
        self.window_size_tx.subscribe()
    }

    pub fn closed(&self) -> bool {
        !self.open.load(Ordering::Acquire)
    }

    /// Try to write bytes into the buffer, returning the number of bytes written.
    pub async fn write(&self, bytes: &[u8]) -> Result<usize, SendBufClosed> {
        if !self.open.load(Ordering::Acquire) {
            return Err(SendBufClosed);
        }
        let mut send_buf = self.inner.lock().await;
        let written = send_buf.write(bytes);
        if written > 0 {
            self.written.notify_all();
        }
        Ok(written)
    }

    /// Writes all bytes into the buffer.
    ///
    /// This function could block for a _long_ time, if it tries to write a
    /// large amount of bytes. This is because the SendBuf's space frees up only
    /// when the remote decides to consume data, and we do not know when the
    /// remote-side application code chooses to do so.
    ///
    /// To bound the amount of wait time, either use this API with
    /// `tokio::time::timeout` or use `SendBuf::write()`.
    pub async fn write_all(&self, bytes: &[u8]) -> Result<(), SendBufClosed> {
        if bytes.is_empty() {
            return Ok(());
        }

        let mut curr = 0;
        loop {
            if !self.open.load(Ordering::Acquire) {
                return Err(SendBufClosed);
            }
            let mut send_buf = self.inner.lock().await;
            let written = send_buf.write(&bytes[curr..]);
            curr += written;
            if written > 0 {
                self.written.notify_all();
            }
            if curr < bytes.len() {
                let not_full = self.not_full.notified();
                drop(send_buf);
                not_full.wait().await;
            } else {
                self.written.notify_all();
                break;
            }
        }

        Ok(())
    }

    /// Advances the tail pointer to the provided sequence number.
    ///
    /// Data up to but not including `seq_no` are discarded.
    ///
    /// Called when handling a new TCP header, which provides the latest ACK.
    pub async fn set_tail(&self, seq_no: usize) -> Result<(), SetTailError> {
        let mut buf = self.inner.lock().await;
        buf.set_tail(seq_no).map(|updated| {
            if updated {
                self.not_full.notify_all();
                if buf.read_remaining_size() == 0 {
                    self.empty.notify_all();
                }
            }
        })
    }

    /// Fills the passed-in buffer with unconsumed content from the head of the
    /// internal buffer. Blocks until the entire passed-in buffer is filled.
    ///
    /// This API is useful for waiting for data to show up in the buffer.
    ///
    /// Deadlock:
    ///
    /// This API can deadlock with concurrent writers if the passed-in
    /// buffer is too big. Slicing unconsumed content does not consume them
    /// (set_tail() does). Deadlock could occur if this caller is waiting for
    /// more data to arrive, while a concurrent writer is waiting for room in
    /// the buffer to free up.
    pub async fn slice_front(&self, buf: &mut [u8]) {
        loop {
            let send_buf = self.inner.lock().await;
            let unconsumed = send_buf.unconsumed();
            match unconsumed.slice_front(buf.len()) {
                Ok(slice) => {
                    slice.copy_into_buf(buf).unwrap();
                    break;
                }
                Err(_) => {
                    let written = self.written.notified();
                    drop(send_buf);
                    written.wait().await;
                }
            }
        }
    }

    /// Try to fill the passed-in buffer with data starting at a sequence number.
    ///
    /// The function succeeds when the head sequence number is less than or equal
    /// to `start_seq_no + buf.len()`, and `start_seq_no` is greater or equal to
    /// the tail sequence number.
    ///
    /// On success, `buf` is filled and the function returns how many bytes can
    /// be sliced after the slice contained in `buf`. On error, `buf` is not
    /// modified, and the error contains the number of unconsumed bytes starting
    /// at `start_seq_no`.
    pub async fn try_slice(
        &self,
        start_seq_no: usize,
        buf: &mut [u8],
    ) -> Result<usize, SliceError> {
        let send_buf = self.inner.lock().await;
        let unconsumed = send_buf.unconsumed();
        if start_seq_no < send_buf.tail {
            return Err(SliceError::StartSeqTooLow(send_buf.tail));
        }
        let offset = start_seq_no - send_buf.tail;
        unconsumed.slice(offset, buf.len()).map(|slice| {
            slice.copy_into_buf(buf).unwrap();
            send_buf.head - start_seq_no - buf.len() // remaining size
        })
    }

    /// Blocks until data there is data available at `seq_no`.
    pub async fn wait_for_new_data(&self, seq_no: usize) {
        loop {
            let send_buf = self.inner.lock().await;
            if send_buf.head > seq_no {
                // data has already arrived
                return;
            }
            let notifier = self.written.notified();
            drop(send_buf);

            notifier.wait().await;
        }
    }

    /// When a buffer is closed, wait for all of its content to be consumed.
    ///
    /// Returns the final sequence number of the buffer, which is both the head
    /// and the tail of the buffer.
    ///
    /// Panics if called on an unclosed buffer.
    pub async fn wait_for_empty(&self) -> usize {
        assert!(
            self.closed(),
            "Should only be used to drain buffer content after it is closed"
        );
        loop {
            let send_buf = self.inner.lock().await;
            if send_buf.read_remaining_size() == 0 {
                return send_buf.head;
            }
            let notifier = self.empty.notified();
            drop(send_buf);
            notifier.wait().await;
        }
    }

    pub async fn close(&self) -> Result<(), SendBufClosed> {
        if self
            .open
            .compare_exchange(true, false, Ordering::Acquire, Ordering::Relaxed)
            .unwrap()
        {
            Ok(())
        } else {
            Err(SendBufClosed)
        }
    }
}

/// A fixed-sized buffer for buffering data to be sent over TCP.
///
/// This is where the low-level mechanics of the TCP send buffer is. For a
/// higher-level API that is directly used by `TcpConn`, see `SendBuf`.
///
/// This struct is akin to a producer-consumer buffer. Here, the producer is
/// application code putting more data into the send buffer. The consumer is
/// TCP transmitting data to the receiver, advancing the consumed portion as
/// transmitted data are acked.
///
/// This struct does not know about usable window size, i.e. how much data can
/// be transmitted. It is up to the caller to maintain usable window size, and
/// advance the consumed portion upon acknowledgement.
#[derive(Debug)]
struct InnerSendBuf<const N: usize> {
    // Index of the last unacked byte in the byte stream.
    // This is the "tail" of a producer-consumer buffer.
    // This is SDR.UNA in the protocol.
    tail: usize,
    last_tail_mutated: Instant,
    // Index of the next byte to be written by the user.
    // This is LBW + 1, where LBW is last byte written in protocol terminology.
    // LBW must not overtake UNA in terms of ring buffer indices.
    head: usize,
    // Ring buffer.
    buf: [u8; N],
    // Debugging use only
    initial_seq_no: usize,
}

pub fn make_default_sendbuf(starting_seq_no: usize) -> SendBuf<TCP_DEFAULT_WINDOW_SZ> {
    SendBuf::<TCP_DEFAULT_WINDOW_SZ>::new(starting_seq_no)
}

pub fn make_default_recvbuf(starting_seq_no: usize) -> RecvBuf<TCP_DEFAULT_WINDOW_SZ> {
    RecvBuf::<TCP_DEFAULT_WINDOW_SZ>::new(starting_seq_no)
}

#[derive(Debug)]
pub enum SetTailError {
    TooBig,
    LowerThanCurrent,
}

#[derive(Debug)]
pub enum SliceError {
    /// Errs when the slice starts at a sequence number lower than the current
    /// buffer tail. Contains the current buffer tail in the error.
    StartSeqTooLow(usize),
    /// Errs when the requested slice goes beyond the actual slice.
    /// Returns the maximum number of bytes sliceable.
    OutOfRange(usize),
}

#[derive(Debug)]
pub enum SliceCopyError {
    TooSmall,
}

/// Slice into a ring buffer.
///
/// Abstract over how a slice could be split into two in a ring buffer.
#[derive(Debug)]
pub struct ByteSlice<'a> {
    first: &'a [u8],
    second: Option<&'a [u8]>,
}

impl<'a> ByteSlice<'a> {
    fn new(first: &'a [u8], second: Option<&'a [u8]>) -> Self {
        Self { first, second }
    }

    /// Gets number of bytes in this slice.
    pub fn len(&self) -> usize {
        match self.second {
            Some(second) => second.len() + self.first.len(),
            None => self.first.len(),
        }
    }

    /// Checks whether the slice has data.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Construct a new slice that is a sub-slice of the current slice.
    pub fn slice_front(&self, n_bytes: usize) -> Result<ByteSlice<'a>, SliceError> {
        self.slice(0, n_bytes)
    }

    /// Construct a slice of [offset, offset + n_bytes).
    pub fn slice(&self, offset: usize, n_bytes: usize) -> Result<ByteSlice<'a>, SliceError> {
        let len = self.len();
        if offset > len {
            return Err(SliceError::OutOfRange(0));
        }
        if offset + n_bytes > len {
            return Err(SliceError::OutOfRange(len - offset));
        }

        let start = offset;
        let end = offset + n_bytes;
        match self.second {
            Some(second) => {
                if end <= self.first.len() {
                    // sliced content is all in the first slice
                    Ok(ByteSlice::new(&self.first[start..end], None))
                } else if start < self.first.len() {
                    // sliced content is in both slices
                    Ok(ByteSlice::new(
                        &self.first[start..],
                        Some(&second[..(end - self.first.len())]),
                    ))
                } else {
                    // sliced content is all in the second slice
                    Ok(ByteSlice::new(
                        &second[(start - self.first.len())..(end - self.first.len())],
                        None,
                    ))
                }
            }
            None => Ok(ByteSlice::new(&self.first[start..end], None)),
        }
    }

    pub fn copy_into_buf(&self, buf: &mut [u8]) -> Result<(), SliceCopyError> {
        if self.len() > buf.len() {
            return Err(SliceCopyError::TooSmall);
        }

        buf[..self.first.len()].copy_from_slice(self.first);

        if let Some(second) = self.second {
            let begin = self.first.len();
            let end = begin + second.len();
            buf[begin..end].copy_from_slice(second);
        }

        Ok(())
    }
}

impl<'a> PartialEq<[u8]> for ByteSlice<'a> {
    fn eq(&self, other: &[u8]) -> bool {
        match self.second {
            Some(second) => {
                if other.len() < self.first.len() {
                    // other is definitely shorter than self
                    return false;
                }

                let first_cmp = &other[..self.first.len()];
                let second_cmp = &other[self.first.len()..];

                self.first == first_cmp && second == second_cmp
            }
            None => other == self.first,
        }
    }
}

impl<const N: usize> InnerSendBuf<N> {
    pub fn new(initial_seq_no: usize) -> Self {
        Self {
            buf: [0; N],
            tail: initial_seq_no,
            head: initial_seq_no,
            last_tail_mutated: Instant::now(),
            initial_seq_no,
        }
    }

    /// Checks whether the send buffer has unacked data.
    pub fn is_empty(&self) -> bool {
        self.tail == self.head
    }

    /// Checks whether application can continue adding data to the buffer.
    pub fn is_full(&self) -> bool {
        let max = self.tail + self.size();
        assert!(self.head <= max);
        self.head == max
    }

    /// Attempts to add bytes to the buffer, returning the number of bytes successfully added.
    ///
    /// Returns 0 when the buffer was already full before this write.
    pub fn write(&mut self, bytes: &[u8]) -> usize {
        let remaining = self.write_remaining_size();
        let will_write = min(remaining, bytes.len());
        let bytes = &bytes[..will_write];

        let start = self.head % self.size();
        // write either all bytes or till the end of the vector.
        let end = min(self.size(), start + will_write);
        let to_write = end - start;
        self.buf[start..end].copy_from_slice(&bytes[..to_write]);

        let written = end - start;
        if written < will_write {
            // looped over to vector beginning, continue write.
            let end = will_write - written;
            self.buf[0..end].copy_from_slice(&bytes[written..written + end]);
        }

        self.head += will_write;

        will_write
    }

    /// Gets the slice of unconsumed bytes in the buffer.
    #[allow(clippy::needless_lifetimes)]
    pub fn unconsumed<'a>(&'a self) -> ByteSlice<'a> {
        let start = self.tail % self.size();
        let end = self.head % self.size();

        match start.cmp(&end) {
            std::cmp::Ordering::Less => ByteSlice::new(&self.buf[start..end], None),
            std::cmp::Ordering::Equal => {
                if self.head == self.tail {
                    ByteSlice::new(&self.buf[start..end], None)
                } else {
                    assert!(self.head == self.tail + self.size());
                    ByteSlice::new(&self.buf[start..], Some(&self.buf[..end]))
                }
            }
            std::cmp::Ordering::Greater => {
                let first = &self.buf[start..];
                let second = &self.buf[..end];
                ByteSlice::new(first, Some(second))
            }
        }
    }

    /// Attempts to advance the unconsumed portion to the provided sequence number.
    ///
    /// Errs when the provided sequence number is greater than the sequence
    /// number of the head of the buffer.
    ///
    /// On success, returns a boolean indicating whether the tail was mutated:
    /// i.e. return false when tail was already set to `seq_no`.
    pub fn set_tail(&mut self, seq_no: usize) -> Result<bool, SetTailError> {
        if seq_no == self.tail {
            return Ok(false);
        }

        if seq_no > self.head {
            Err(SetTailError::TooBig)
        } else if seq_no < self.tail {
            Err(SetTailError::LowerThanCurrent)
        } else {
            self.tail = seq_no;
            self.last_tail_mutated = Instant::now();
            Ok(true)
        }
    }

    /// Get how much data this buffer can hold.
    pub fn size(&self) -> usize {
        self.buf.len()
    }

    /// Get how many bytes can be written to the buffer before it is full.
    fn write_remaining_size(&self) -> usize {
        self.tail + self.size() - self.head
    }

    /// Get how many bytes can be read before the buffer becomes empty.
    fn read_remaining_size(&self) -> usize {
        self.head - self.tail
    }
}

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
struct SegmentMeta {
    seq_no: usize,
    size: usize,
}

impl PartialOrd for SegmentMeta {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.seq_no.partial_cmp(&other.seq_no)
    }
}

impl Ord for SegmentMeta {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.seq_no.cmp(&other.seq_no)
    }
}

#[derive(Debug)]
pub struct RecvBufClosed;

#[derive(Debug)]
pub enum FillError {
    /// Failed to fill the entire provided buffer because the receive buffer
    /// has been closed (because the remote has closed).
    /// Returns the number of bytes written into the buffer.
    Closed(usize),
}

/// The receiving-side of TCP transmission buffer.
#[derive(Debug, Clone)]
pub struct RecvBuf<const N: usize> {
    inner: Arc<Mutex<InnerRecvBuf<N>>>,
    written: Notifier,
    read: Notifier,
    open: Arc<AtomicBool>,
}

impl<const N: usize> RecvBuf<N> {
    /// Constructs a new RecvBuf.
    pub fn new(starting_seq_no: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(InnerRecvBuf::new(starting_seq_no))),
            written: Notifier::new(),
            read: Notifier::new(),
            open: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Get the next sequence number expected to be sent by the sender.
    ///
    /// Note that `head` intentionally ignores early arrivals. Packets with later
    /// sequence numbers may or may not have arrived, but the packets are only
    /// consecutive up to the sequence number returned by this method.
    pub async fn head(&self) -> usize {
        self.inner.lock().await.head
    }

    /// Get the sequence number of the next byte to be consumed by the
    /// application layer.
    pub async fn tail(&self) -> usize {
        self.inner.lock().await.tail
    }

    /// Get the window size to be advertized to remote.
    ///
    /// This is the allocated buffer size minus unconsumed, consecutive bytes.
    pub async fn window_size(&self) -> usize {
        self.inner.lock().await.write_remaining_size()
    }

    pub async fn close(&self) -> Result<(), RecvBufClosed> {
        self.open
            .compare_exchange(true, false, Ordering::Acquire, Ordering::Relaxed)
            .map(|_| {
                // notify pending readers, if any
                self.written.notify_all();
            })
            .map_err(|_| RecvBufClosed)
    }

    pub fn closed(&self) -> bool {
        !self.open.load(Ordering::Acquire)
    }

    /// Try to fill the provided buffer.
    ///
    /// The method returns a slice of the written bytes, which will be a subslice
    /// of the provided buffer.
    ///
    /// Filling the buffer simultaneously advances the buffer tail: bytes, once
    /// consumed, are discarded from RecvBuf.
    pub async fn try_fill<'a>(&'a self, dest: &'a mut [u8]) -> Result<&'a [u8], RecvBufClosed> {
        if self.closed() {
            return Err(RecvBufClosed);
        }
        let consumed = self.inner.lock().await.try_fill(dest);
        if !consumed.is_empty() {
            self.read.notify_all();
        }
        Ok(consumed)
    }

    /// Try to write some bytes into the `dest` buffer, returning the number of
    /// bytes written.
    ///
    /// Blocks until at least one byte is written into `dest`.
    pub async fn try_fill_some<'a>(&'a self, dest: &'a mut [u8]) -> Result<usize, RecvBufClosed> {
        assert!(!dest.is_empty());

        loop {
            if self.closed() {
                return Err(RecvBufClosed);
            }
            let mut recv_buf = self.inner.lock().await;
            let consumed = recv_buf.try_fill(dest);
            if !consumed.is_empty() {
                self.read.notify_all();
                return Ok(consumed.len());
            }
            let written = self.written.notified();
            drop(recv_buf);
            written.wait().await;
        }
    }

    /// Fill the entire provided buffer.
    ///
    /// Filling the buffer simultaneously advances the buffer tail: bytes, once
    /// consumed, are discarded from RecvBuf.
    pub async fn fill<'a>(&self, dest: &'a mut [u8]) -> Result<(), FillError> {
        if dest.is_empty() {
            return Ok(());
        }

        let mut curr = 0;
        loop {
            let mut recv_buf = self.inner.lock().await;
            let consumed = recv_buf.try_fill(&mut dest[curr..]);
            curr += consumed.len();
            if !consumed.is_empty() {
                self.read.notify_all();
            }
            if curr < dest.len() {
                if self.closed() {
                    // will not receive any more bytes from the remote
                    return Err(FillError::Closed(curr));
                }
                let written = self.written.notified();
                drop(recv_buf);
                written.wait().await;
            } else {
                break;
            }
        }

        Ok(())
    }

    /// Try to write the provided bytes into RecvBuf, starting at the provided
    /// sequence number.
    ///
    /// To account for early arrivals, the sequence number to start writing at
    /// need not be the head of the buffer.
    ///
    /// If `seq_no != head`, then the buffer head is not updated.
    ///
    /// If `seq_no == head`, then the buffer head is updated by at least the
    /// size of `bytes` (if this write does not fail for other reasons). This
    /// write will consume the early arrival segments that are contiguous with
    /// this write, advancing the head pointer to the furthest extent.
    pub async fn try_write(&self, seq_no: usize, bytes: &[u8]) -> Result<(), WriteRangeError> {
        self.inner
            .lock()
            .await
            .write(seq_no, bytes)
            .map(|_| self.written.notify_all())
    }

    /// Like `RecvBuf::try_write()`, but blocks until all of `bytes` are written.
    ///
    /// Calling this method could block for a _long_ time. This is because the
    /// method can wait for room to write in, and there's only new room available
    /// if the application layer chooses to consume content. Since we do not
    /// know when the application layer will consume, this method can block for
    /// an arbitrary amount of time.
    pub async fn write(&self, seq_no: usize, bytes: &[u8]) -> Result<(), WriteRangeError> {
        loop {
            let mut recv_buf = self.inner.lock().await;
            match recv_buf.write(seq_no, bytes) {
                Ok(_) => {
                    self.written.notify_all();
                    return Ok(());
                }
                Err(e) => {
                    match e {
                        WriteRangeError::SeqNoTooSmall(_) => return Err(e),
                        WriteRangeError::ExceedBuffer(_) => {
                            // wait for room to free up
                            let has_room = self.read.notified();
                            drop(recv_buf);
                            has_room.wait().await;
                        }
                    }
                }
            }
        }
    }
}

/// A fixed-sized buffer for constructing a contiguous byte stream over TCP.
///
/// Upon receiving a packet, the payload can be written into this buffer via
/// `RecvBuf::write()`. Additionally, window size can be probed by inspecting
/// `RecvBuf::write_remaining_size()`.
#[derive(Debug)]
struct InnerRecvBuf<const N: usize> {
    buf: [u8; N],
    tail: usize,
    head: usize,
    early_arrivals: BinaryHeap<Reverse<SegmentMeta>>,
    // Debugging use only
    initial_seq_no: usize,
}

#[derive(Debug)]
pub enum WriteRangeError {
    /// Starting at a sequence number below the minimal bound. Contains the
    /// smallest sequence number that can be written to.
    SeqNoTooSmall(usize),
    /// Writing too much into the buffer. Contains the sequence number
    /// corresponding to the tail of the buffer.
    ExceedBuffer(usize),
}

impl<const N: usize> InnerRecvBuf<N> {
    pub fn new(initial_seq_no: usize) -> Self {
        Self {
            buf: [0; N],
            tail: initial_seq_no,
            head: initial_seq_no,
            early_arrivals: BinaryHeap::new(),
            initial_seq_no,
        }
    }

    /// Attempts to fill the provided buffer.
    ///
    /// The function returns a slice of written bytes to the caller. The
    /// returned slice is a subslice of the provided buffer.
    ///
    /// Bytes can only be consumed once; the internal buffer is free to discard
    /// consumed bytes.
    pub fn try_fill<'a>(&mut self, dest: &'a mut [u8]) -> &'a [u8] {
        let n_bytes = dest.len();
        let remaining = self.read_remaining_size();

        match remaining {
            0 => &dest[0..0],
            _ => {
                let to_consume = min(remaining, n_bytes);
                self.consume_unchecked(to_consume, dest);
                &dest[0..to_consume]
            }
        }
    }

    /// Attempts to write bytes starting at a sequence number.
    ///
    /// This method errs when the write remaining size of this buffer is less
    /// than the number of bytes to be written.
    ///
    /// Either all bytes or no byte will be written: in the error case, no byte
    /// shall be written in the buffer.
    pub fn write(&mut self, seq_no: usize, bytes: &[u8]) -> Result<(), WriteRangeError> {
        self.validate_write_range(seq_no, seq_no + bytes.len())
            .map(|_| self.write_unchecked(seq_no, bytes))
    }

    /// Whether the internal byte buffer is non-contiguous, i.e. some bytes
    /// arrived while some other bytes before them have not arrived.
    pub fn has_early_arrival(&self) -> bool {
        !self.early_arrivals.is_empty()
    }

    /// Get the next sequence number expected to be sent by the sender.
    pub fn expected_next(&self) -> usize {
        self.head
    }

    /// Get the buffer room between "expected_next" and the end of the buffer.
    pub fn write_remaining_size(&self) -> usize {
        self.tail + self.size() - self.head
    }

    /// Get the [min, max) sequence number that can be written into.
    pub fn write_range(&self) -> (usize, usize) {
        (self.head, self.tail + self.buf.len())
    }

    /// Get the number of consumable bytes.
    ///
    /// Note that early arrival bytes are not consumable.
    pub fn read_remaining_size(&self) -> usize {
        self.head - self.tail
    }
}

impl<const N: usize> InnerRecvBuf<N> {
    fn size(&self) -> usize {
        self.buf.len()
    }

    /// Whether the provided range is within write range.
    ///
    /// end_seq_no is exclusive: this checks writing up to but not including
    /// end_seq_no.
    fn validate_write_range(
        &self,
        start_seq_no: usize,
        end_seq_no: usize,
    ) -> Result<(), WriteRangeError> {
        let (min, max) = self.write_range();
        // start_seq_no >= min && end_seq_no <= max

        if start_seq_no < min {
            return Err(WriteRangeError::SeqNoTooSmall(min));
        }
        if end_seq_no > max {
            return Err(WriteRangeError::ExceedBuffer(max));
        }

        Ok(())
    }

    fn write_unchecked(&mut self, seq_no: usize, bytes: &[u8]) {
        self.write_into_buf(seq_no, bytes);

        if seq_no == self.head {
            self.head += bytes.len();
            if self.has_early_arrival() {
                self.drain_early_arrivals();
            }
        } else {
            self.early_arrivals.push(Reverse(SegmentMeta {
                seq_no,
                size: bytes.len(),
            }));
        }
    }

    /// Consume exactly n bytes.
    fn consume_unchecked(&mut self, n_bytes: usize, dest: &mut [u8]) {
        let start = self.tail % self.size();
        let end = min(start + n_bytes, self.size());
        let to_copy = end - start;

        let mut curr = 0;
        dest[curr..to_copy].copy_from_slice(&self.buf[start..end]);
        curr += to_copy;

        let copied = end - start;
        if copied < n_bytes {
            // wrap over
            let remaining = n_bytes - copied;
            dest[curr..curr + remaining].copy_from_slice(&self.buf[..remaining]);
        }

        self.tail += n_bytes;
    }

    fn write_into_buf(&mut self, seq_no: usize, bytes: &[u8]) {
        let start = seq_no % self.size();
        let end = min(start + bytes.len(), self.size());
        let to_write = end - start;
        self.buf[start..end].copy_from_slice(&bytes[..to_write]);

        if to_write < bytes.len() {
            // wrap over
            let end = bytes.len() - to_write;
            self.buf[..end].copy_from_slice(&bytes[to_write..]);
        }
    }

    fn drain_early_arrivals(&mut self) {
        while !self.early_arrivals.is_empty() {
            let top = self.early_arrivals.peek().unwrap().0;
            if top.seq_no <= self.head {
                self.early_arrivals.pop().unwrap();
                self.head = max(self.head, top.seq_no + top.size);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        thread::yield_now,
    };

    use super::*;

    fn make_default_inner_sendbuf(start_seq_no: usize) -> InnerSendBuf<TCP_DEFAULT_WINDOW_SZ> {
        InnerSendBuf::<TCP_DEFAULT_WINDOW_SZ>::new(start_seq_no)
    }

    fn make_default_inner_recvbuf(start_seq_no: usize) -> InnerRecvBuf<TCP_DEFAULT_WINDOW_SZ> {
        InnerRecvBuf::<TCP_DEFAULT_WINDOW_SZ>::new(start_seq_no)
    }

    #[cfg(test)]
    mod send {

        use super::*;

        #[tokio::test]
        async fn read_write() {
            let base_data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
            let data: Vec<_> = base_data.into_iter().cycle().take(1024).collect();
            let data2 = data.clone();

            let num_repeats = 100_000;
            let initial_seq_no = 33;
            let buf = make_default_sendbuf(initial_seq_no);

            let producer_buf = buf.clone();
            let producer = tokio::spawn(async move {
                for _ in 0..num_repeats {
                    producer_buf.write_all(&data).await.unwrap();
                }
            });

            let consumer = tokio::spawn(async move {
                let mut out_buf = vec![0; data2.len()];
                let mut seq_no = initial_seq_no;
                for _ in 0..num_repeats {
                    buf.slice_front(&mut out_buf).await;
                    assert_eq!(out_buf, data2);
                    seq_no += out_buf.len();
                    buf.set_tail(seq_no).await.unwrap();
                }
            });

            producer.await.unwrap();
            consumer.await.unwrap();
        }

        #[tokio::test]
        async fn read_write_with_try_slice() {
            let base_data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
            let data: Vec<_> = base_data.into_iter().cycle().take(1024).collect();
            let data2 = data.clone();

            let num_repeats = 1_000_000;
            let initial_seq_no = 33;
            let buf = make_default_sendbuf(initial_seq_no);

            let producer_buf = buf.clone();
            let producer = tokio::spawn(async move {
                for _ in 0..num_repeats {
                    producer_buf.write_all(&data).await.unwrap();
                }
            });

            let consumer = tokio::spawn(async move {
                let mut out_buf = vec![0; data2.len()];
                let mut seq_no = initial_seq_no;
                for _ in 0..num_repeats {
                    match buf.try_slice(seq_no, &mut out_buf).await {
                        Ok(_) => {
                            assert_eq!(out_buf, data2);
                            seq_no += out_buf.len();
                        }
                        Err(_) => {
                            // Ran out of data to slice, discard all consumed content.
                            buf.set_tail(seq_no).await.unwrap();

                            // Blocks until data has been written in.
                            buf.slice_front(&mut out_buf).await;
                            assert_eq!(out_buf, data2);
                            seq_no += out_buf.len();
                        }
                    }
                }
            });

            producer.await.unwrap();
            consumer.await.unwrap();
        }
    }

    #[cfg(test)]
    mod recv {
        use super::*;

        #[tokio::test]
        async fn read_write() {
            let base_data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
            let data: Vec<_> = base_data.into_iter().cycle().take(1024).collect();
            let data2 = data.clone();

            let num_repeats = 100_000;
            let initial_seq_no = 87;
            let buf = make_default_recvbuf(initial_seq_no);

            let producer_buf = buf.clone();
            let producer = tokio::spawn(async move {
                let mut seq_no = initial_seq_no;
                for _ in 0..num_repeats {
                    producer_buf.write(seq_no, &data).await.unwrap();
                    seq_no += data.len();
                }
            });

            let consumer = tokio::spawn(async move {
                let mut out_buf = vec![0; data2.len()];
                for _ in 0..num_repeats {
                    buf.fill(&mut out_buf).await.unwrap();
                    assert_eq!(out_buf, data2);
                }
            });

            producer.await.unwrap();
            consumer.await.unwrap();
        }
    }

    #[cfg(test)]
    mod inner_send {
        use super::*;

        #[test]
        fn write_until_full() {
            let data = [1; 15];

            let mut buf = make_default_inner_sendbuf(98989);
            assert!(buf.is_empty());

            let mut total_written = 0;
            loop {
                let written = buf.write(&data);
                if written == 0 {
                    break;
                }
                total_written += written;
            }

            assert!(buf.is_full());
            assert_eq!(total_written, buf.size());
        }

        #[test]
        fn read_until_empty() {
            let data = [1, 2, 3, 4, 5, 6, 7, 8];

            let initial_seq_no = 56;
            let mut buf = make_default_inner_sendbuf(initial_seq_no);
            fill_buf(&mut buf, &data);

            let mut seq_no = initial_seq_no;
            while !buf.is_empty() {
                match buf.set_tail(seq_no + data.len()) {
                    Ok(_) => {
                        seq_no += data.len();
                        let expect_len = min(data.len(), buf.read_remaining_size());
                        let pending_buf = buf.unconsumed().slice_front(expect_len).unwrap();
                        assert!(pending_buf == data[..expect_len]);
                    }
                    Err(_) => {
                        buf.set_tail(seq_no + buf.read_remaining_size()).unwrap();
                        seq_no += buf.read_remaining_size();
                    }
                }
            }

            assert!(buf.is_empty());
            assert!(buf.unconsumed().is_empty());
        }

        #[test]
        fn read_write() {
            // Simulate real SendBuf interactions in TCP.
            //
            // Here, a thread acts as application code writing into this buffer.
            // Another thread in the TCP stack consumes from this buffer.
            //
            // Bytes are marked as consumed when they're acked, so there will be
            // some bytes that are transmitted but not acked; these bytes remain
            // unconsumed from SendBuf's perspective.

            let data = [1, 2, 3, 4, 5, 6, 7, 8];
            let num_repeats = 1_000_000;
            let initial_seq_no = 77;
            let buf = Arc::new(Mutex::new(make_default_inner_sendbuf(initial_seq_no)));

            let producer_buf = buf.clone();
            let producer = std::thread::spawn(move || {
                for _ in 0..num_repeats {
                    let mut start = 0;
                    while start != data.len() {
                        let mut buf = producer_buf.lock().unwrap();
                        let written = buf.write(&data[start..]);
                        start += written;
                        if written < data.len() - start {
                            yield_now();
                        }
                    }
                }
            });

            let consumer = std::thread::spawn(move || {
                let mut seq_no = initial_seq_no;
                for _ in 0..num_repeats {
                    loop {
                        let mut b = buf.lock().unwrap();
                        let remaining = b.read_remaining_size();
                        if remaining < data.len() {
                            drop(b);
                            yield_now();
                            continue;
                        }
                        let got = b.unconsumed().slice_front(data.len()).unwrap();
                        assert_eq!(got, data[..]);
                        seq_no += data.len();
                        b.set_tail(seq_no).unwrap();
                        break;
                    }
                }
            });

            producer.join().unwrap();
            consumer.join().unwrap();
        }

        fn fill_buf<const N: usize>(buf: &mut InnerSendBuf<N>, data: &[u8]) {
            loop {
                let written = buf.write(data);
                if written == 0 {
                    break;
                }
            }
            assert!(buf.is_full());
        }
    }

    #[cfg(test)]
    mod inner_recv {

        use rand::{thread_rng, Rng};

        use super::*;

        #[test]
        fn contiguous_write_till_full() {
            let start_seq_no = 231251;
            let data = [1, 2, 3, 4, 5, 6, 7, 8];

            let mut buf = make_default_inner_recvbuf(start_seq_no);
            assert!(buf.write_remaining_size() == TCP_DEFAULT_WINDOW_SZ);
            assert_eq!(
                buf.write_range(),
                (start_seq_no, start_seq_no + buf.write_remaining_size())
            );

            let mut curr = start_seq_no;
            let mut total = 0;
            loop {
                match buf.write(curr, &data) {
                    Ok(_) => {
                        curr += data.len();
                        total += data.len();
                        assert_eq!(buf.read_remaining_size(), total);
                        assert_eq!(buf.expected_next(), start_seq_no + total);
                        assert_eq!(
                            buf.write_range(),
                            (start_seq_no + total, start_seq_no + TCP_DEFAULT_WINDOW_SZ)
                        );
                    }
                    Err(_) => {
                        let remaining = buf.write_remaining_size();
                        buf.write(curr, &data[..remaining]).unwrap();
                        total += remaining;
                        break;
                    }
                }
            }

            assert_eq!(total, TCP_DEFAULT_WINDOW_SZ);
            assert_eq!(buf.write_remaining_size(), 0);
            assert_eq!(buf.read_remaining_size(), TCP_DEFAULT_WINDOW_SZ);
            assert_eq!(
                buf.write_range(),
                (
                    start_seq_no + TCP_DEFAULT_WINDOW_SZ,
                    start_seq_no + TCP_DEFAULT_WINDOW_SZ
                )
            );
        }

        #[test]
        fn contiguous_read_from_full() {
            let start_seq_no = 1291241;
            let data = [1, 2, 3, 4, 5, 6, 7, 8];

            let mut buf = make_default_inner_recvbuf(start_seq_no);
            fill_buf(&mut buf, start_seq_no, &data);

            assert_eq!(buf.write_remaining_size(), 0);
            assert_eq!(buf.read_remaining_size(), TCP_DEFAULT_WINDOW_SZ);
            assert_eq!(
                buf.write_range(),
                (
                    start_seq_no + TCP_DEFAULT_WINDOW_SZ,
                    start_seq_no + TCP_DEFAULT_WINDOW_SZ
                )
            );

            let mut total_consumed = 0;
            let mut bytes = vec![0; 16];
            loop {
                let consumed = buf.try_fill(&mut bytes[..data.len()]);
                if consumed.len() < data.len() {
                    assert_eq!(consumed, &data[..consumed.len()]);
                    total_consumed += consumed.len();
                    break;
                }
                assert_eq!(consumed, &data);
                total_consumed += consumed.len();

                assert_eq!(
                    buf.read_remaining_size(),
                    TCP_DEFAULT_WINDOW_SZ - total_consumed
                );
                assert_eq!(buf.write_remaining_size(), total_consumed);
                assert_eq!(
                    buf.write_range(),
                    (
                        start_seq_no + TCP_DEFAULT_WINDOW_SZ,
                        start_seq_no + TCP_DEFAULT_WINDOW_SZ + total_consumed
                    )
                );
            }

            assert_eq!(total_consumed, TCP_DEFAULT_WINDOW_SZ);
        }

        #[test]
        fn contiguous_read_write() {
            // Simulate real RecvBuf interations in TCP.
            //
            // Here, the producer thread acts as the thread in the TCP stack
            // that's pushing data into the buffer. A separate consumer thread
            // acts as a the application-level thread that's consuming data.

            let start_seq_no = 91215;
            let num_repeats = 1_000_000;
            let data = [1, 2, 3, 4, 5, 6, 7, 8];
            let buf = Arc::new(Mutex::new(make_default_inner_recvbuf(start_seq_no)));

            let producer_buf = buf.clone();
            let producer = std::thread::spawn(move || {
                let mut curr = start_seq_no;
                for _ in 0..num_repeats {
                    loop {
                        let mut b = producer_buf.lock().unwrap();
                        match b.write(curr, &data) {
                            Ok(_) => {
                                curr += data.len();
                                break;
                            }
                            Err(_) => {
                                yield_now();
                            }
                        }
                    }
                }
            });

            let consumer = std::thread::spawn(move || {
                let mut consume_buf = vec![0; 16];
                for _ in 0..num_repeats {
                    let mut curr = 0;
                    loop {
                        let mut b = buf.lock().unwrap();
                        let consumed = b.try_fill(&mut consume_buf[curr..data.len()]);
                        curr += consumed.len();
                        if consumed.is_empty() {
                            yield_now();
                        } else if curr == data.len() {
                            assert_eq!(consume_buf[..data.len()], data);
                            break;
                        }
                    }
                }
                assert!(buf.lock().unwrap().read_remaining_size() == 0);
            });

            producer.join().unwrap();
            consumer.join().unwrap();
        }

        #[test]
        fn non_consecutive_rw() {
            let start_seq_no = 0;
            let data = [1, 2, 3, 4, 5, 6, 7, 8];
            let mut buf = make_default_inner_recvbuf(start_seq_no);

            //      |-------------WRITEABLE------------|
            // |xxxx-----------------------------------|
            //  ^   ^
            //  0   8

            buf.write(start_seq_no, &data).unwrap();
            assert!(!buf.has_early_arrival());
            assert_eq!(
                buf.write_remaining_size(),
                TCP_DEFAULT_WINDOW_SZ - data.len()
            );
            assert_eq!(buf.expected_next(), 8);
            assert_eq!(
                buf.write_range(),
                (
                    start_seq_no + data.len(),
                    start_seq_no + TCP_DEFAULT_WINDOW_SZ
                )
            );

            //      |-------------WRITEABLE------------|
            // |xxxx-------------xxxx------------------|
            //  ^   ^            ^   ^
            //  0   8            100 108

            buf.write(start_seq_no + 100, &data).unwrap();
            assert!(buf.has_early_arrival());
            assert_eq!(
                buf.write_remaining_size(),
                TCP_DEFAULT_WINDOW_SZ - data.len()
            );
            assert_eq!(buf.expected_next(), 8);
            assert_eq!(
                buf.write_range(),
                (
                    start_seq_no + data.len(),
                    start_seq_no + TCP_DEFAULT_WINDOW_SZ
                )
            );

            //      |-------------WRITEABLE------------|
            // |xxxx-------------xxxx----xxxx----------|
            //  ^   ^            ^   ^   ^   ^
            //  0   8            100 108 135 143

            buf.write(start_seq_no + 135, &data).unwrap();
            assert!(buf.has_early_arrival());
            assert_eq!(
                buf.write_remaining_size(),
                TCP_DEFAULT_WINDOW_SZ - data.len()
            );
            assert_eq!(buf.expected_next(), 8);
            assert_eq!(
                buf.write_range(),
                (
                    start_seq_no + data.len(),
                    start_seq_no + TCP_DEFAULT_WINDOW_SZ
                )
            );

            //            |---------WRITEABLE----------|
            // |xxxxxxxxxx-------xxxx----xxxx----------|
            //  ^         ^      ^   ^   ^   ^
            //  0         63     100 108 135 143

            let payload = [8; 55];
            buf.write(start_seq_no + data.len(), &payload).unwrap();
            assert!(buf.has_early_arrival());
            assert_eq!(buf.expected_next(), 63);
            assert_eq!(
                buf.write_remaining_size(),
                TCP_DEFAULT_WINDOW_SZ - data.len() - payload.len()
            );
            assert_eq!(
                buf.write_range(),
                (
                    start_seq_no + data.len() + payload.len(),
                    start_seq_no + TCP_DEFAULT_WINDOW_SZ
                )
            );

            //                       |----WRITEABLE----|
            // |xxxxxxxxxxxxxxxxxxxxx----xxxx----------|
            //  ^                    ^   ^   ^
            //  0                    108 135 143

            let payload = [3; 37];
            buf.write(start_seq_no + 63, &payload).unwrap();
            assert!(buf.has_early_arrival());
            assert_eq!(buf.expected_next(), 108);
            assert_eq!(
                buf.write_range(),
                (start_seq_no + 108, start_seq_no + TCP_DEFAULT_WINDOW_SZ)
            );

            //                                    |-WT-|
            // |xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-----|
            //  ^                                 ^
            //  0                                 208

            let payload = [1; 100];
            buf.write(start_seq_no + 108, &payload).unwrap();
            assert!(!buf.has_early_arrival());
            assert_eq!(buf.expected_next(), 208);
            assert_eq!(
                buf.write_range(),
                (start_seq_no + 208, start_seq_no + TCP_DEFAULT_WINDOW_SZ)
            );
        }

        #[test]
        fn multithreaded_non_consecutive_rw() {
            // Simulate real data arrival pattern in TCP.
            //
            // Tests that the byte stream consumed is contiguous even when
            // data arrives in different order.

            let data: Vec<u8> = (0..255).collect();

            let start_seq_no = 91215;
            let num_repeats = 100_000;
            let buf = Arc::new(Mutex::new(make_default_inner_recvbuf(start_seq_no)));

            let producer_buf = buf.clone();
            let producer_data = data.clone();
            let producer = std::thread::spawn(move || {
                let mut rng = thread_rng();
                let mut curr = start_seq_no;

                for _ in 0..num_repeats {
                    let target = curr + producer_data.len();
                    let mut num_iters = 0;
                    let mut succeeded = false;

                    while num_iters < 10 {
                        let mut b = producer_buf.lock().unwrap();

                        // introduce jitter in arrival
                        let offset = rng.gen_range(0..producer_data.len());
                        match b.write(curr + offset, &producer_data[offset..]) {
                            Ok(_) => {}
                            Err(e) => {
                                if matches!(e, WriteRangeError::ExceedBuffer(_)) {
                                    yield_now();
                                }
                            }
                        }

                        assert!(b.expected_next() <= target);
                        if b.expected_next() == target {
                            succeeded = true;
                            break;
                        }

                        num_iters += 1;
                    }

                    if !succeeded {
                        loop {
                            let mut b = producer_buf.lock().unwrap();
                            match b.write(curr, &producer_data) {
                                Ok(_) => break,
                                Err(e) => {
                                    if matches!(e, WriteRangeError::ExceedBuffer(_)) {
                                        yield_now();
                                    }
                                }
                            }
                        }
                    }

                    curr = target;
                }
            });

            let consumer = std::thread::spawn(move || {
                let mut consume_buf = vec![0; 1024];
                for _ in 0..num_repeats {
                    let mut curr = 0;
                    loop {
                        let mut b = buf.lock().unwrap();
                        let consumed = b.try_fill(&mut consume_buf[curr..data.len()]);
                        curr += consumed.len();
                        if consumed.is_empty() {
                            yield_now();
                        } else if curr == data.len() {
                            assert_eq!(consume_buf[..data.len()], data);
                            break;
                        }
                    }
                }
                assert!(buf.lock().unwrap().read_remaining_size() == 0);
            });

            producer.join().unwrap();
            consumer.join().unwrap();
        }

        fn fill_buf<const N: usize>(buf: &mut InnerRecvBuf<N>, start_seq_no: usize, data: &[u8]) {
            let mut curr = start_seq_no;
            loop {
                match buf.write(curr, data) {
                    Ok(_) => curr += data.len(),
                    Err(_) => {
                        buf.write(curr, &data[..buf.write_remaining_size()])
                            .unwrap();
                        break;
                    }
                }
            }
        }
    }
}
