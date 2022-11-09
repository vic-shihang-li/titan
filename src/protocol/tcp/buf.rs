use std::{
    cmp::{max, min, Reverse},
    collections::BinaryHeap,
};

use super::TCP_DEFAULT_WINDOW_SZ;

/// A fixed-sized buffer for buffering data to be sent over TCP.
///
/// This is akin to a producer-consumer buffer. Here, the producer is application code putting more
/// data into the send buffer. The consumer is TCP transmitting data to the receiver, advancing the
/// consumed portion as transmitted data are acked.
///
/// This struct does not know about usable window size, i.e. how much data can be transmitted. It is
/// up to the caller to maintain usable window size, and advance the consumed portion upon
/// acknowledgement.
pub struct SendBuf<const N: usize> {
    // Index of the last unacked byte in the byte stream.
    // This is the "tail" of a producer-consumer buffer.
    // This is SDR.UNA in the protocol.
    tail: usize,
    // Index of the next byte to be written by the user.
    // This is LBW + 1, where LBW is last byte written in protocol terminology.
    // LBW must not overtake UNA in terms of ring buffer indices.
    head: usize,
    // Ring buffer.
    buf: [u8; N],
}

pub fn make_default_sendbuf() -> SendBuf<TCP_DEFAULT_WINDOW_SZ> {
    SendBuf::<TCP_DEFAULT_WINDOW_SZ>::new()
}

pub fn make_default_recvbuf(starting_seq_no: usize) -> RecvBuf<TCP_DEFAULT_WINDOW_SZ> {
    RecvBuf::<TCP_DEFAULT_WINDOW_SZ>::new(starting_seq_no)
}

#[derive(Debug)]
pub enum AdvanceError {
    TooBig,
}

#[derive(Debug)]
pub struct SliceError {
    requested: usize,
    got: usize,
}

/// Slice into a ring buffer.
///
/// Abstract over how a slice could be split into two in a ring buffer.
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
        if n_bytes <= self.len() {
            match self.second {
                Some(second) => {
                    if n_bytes <= self.first.len() {
                        Ok(ByteSlice::new(&self.first[..n_bytes], None))
                    } else {
                        Ok(ByteSlice::new(
                            self.first,
                            Some(&second[..(n_bytes - self.first.len())]),
                        ))
                    }
                }
                None => Ok(ByteSlice::new(&self.first[..n_bytes], None)),
            }
        } else {
            Err(SliceError {
                requested: n_bytes,
                got: self.len(),
            })
        }
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

impl<const N: usize> SendBuf<N> {
    pub fn new() -> Self {
        Self {
            buf: [0; N],
            tail: 0,
            head: 0,
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
            self.buf[0..end].copy_from_slice(&bytes[..end]);
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
                    ByteSlice::new(&self.buf, None)
                }
            }
            std::cmp::Ordering::Greater => {
                let first = &self.buf[start..];
                let second = &self.buf[..end];
                ByteSlice::new(first, Some(second))
            }
        }
    }

    /// Attempts to advance the unconsumed portion by N bytes.
    ///
    /// Errs when N is greater than the number of unconsumed bytes.
    pub fn advance(&mut self, n_bytes: usize) -> Result<(), AdvanceError> {
        if n_bytes <= self.read_remaining_size() {
            self.tail += n_bytes;
            Ok(())
        } else {
            Err(AdvanceError::TooBig)
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

#[derive(PartialEq, Eq)]
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

/// A fixed-sized buffer for constructing a contiguous byte stream over TCP.
///
/// Upon receiving a packet, the payload can be written into this buffer via
/// `RecvBuf::write()`. Additionally, window size can be probed by inspecting
/// `RecvBuf::write_remaining_size()`.
pub struct RecvBuf<const N: usize> {
    buf: [u8; N],
    tail: usize,
    head: usize,
    early_arrivals: BinaryHeap<Reverse<SegmentMeta>>,
}

#[derive(Debug)]
pub enum ConsumeError {
    DestTooSmall,
}

#[derive(Debug)]
pub enum WriteRangeError {
    /// Starting at a sequence number below the minimal bound.
    SeqNoTooSmall,
    /// Writing too much into the buffer.
    ExceedBuffer,
}

impl<const N: usize> RecvBuf<N> {
    pub fn new(starting_seq_no: usize) -> Self {
        Self {
            buf: [0; N],
            tail: starting_seq_no,
            head: starting_seq_no,
            early_arrivals: BinaryHeap::new(),
        }
    }

    /// Attempts to obtain N bytes from the internal buffer.
    ///
    /// The function will write up to N bytes into the destination buffer,
    /// returning a slice of written bytes to the caller.
    ///
    /// Bytes can only be consumed once; the internal buffer is free to discard
    /// consumed bytes.
    pub fn consume<'a>(
        &mut self,
        n_bytes: usize,
        dest: &'a mut [u8],
    ) -> Result<&'a [u8], ConsumeError> {
        if dest.len() < n_bytes {
            return Err(ConsumeError::DestTooSmall);
        }

        let remaining = self.read_remaining_size();

        match remaining {
            0 => Ok(&dest[0..0]),
            _ => {
                let to_consume = min(remaining, n_bytes);
                self.consume_unchecked(to_consume, dest);
                Ok(&dest[0..to_consume])
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

impl<const N: usize> RecvBuf<N> {
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
            return Err(WriteRangeError::SeqNoTooSmall);
        }
        if end_seq_no > max {
            return Err(WriteRangeError::ExceedBuffer);
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
            let top = self.early_arrivals.peek().unwrap();
            if top.0.seq_no <= self.head {
                let top = self.early_arrivals.pop().unwrap();
                self.head = max(self.head, top.0.seq_no + top.0.size);
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

    #[cfg(test)]
    mod send {
        use super::*;

        #[test]
        fn write_until_full() {
            let data = [1; 15];

            let mut buf = make_default_sendbuf();
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

            let mut buf = make_default_sendbuf();
            fill_buf(&mut buf, &data);

            while !buf.is_empty() {
                match buf.advance(data.len()) {
                    Ok(_) => {
                        let expect_len = min(data.len(), buf.read_remaining_size());
                        let pending_buf = buf.unconsumed().slice_front(expect_len).unwrap();
                        assert!(pending_buf == data[..expect_len]);
                    }
                    Err(_) => {
                        buf.advance(buf.write_remaining_size()).unwrap();
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
            let buf = Arc::new(Mutex::new(make_default_sendbuf()));

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
                        assert!(got == data[..]);
                        b.advance(data.len()).unwrap();
                        break;
                    }
                }
            });

            producer.join().unwrap();
            consumer.join().unwrap();
        }

        fn fill_buf<const N: usize>(buf: &mut SendBuf<N>, data: &[u8]) {
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
    mod recv {

        use rand::{thread_rng, Rng};

        use super::*;

        #[test]
        fn contiguous_write_till_full() {
            let start_seq_no = 231251;
            let data = [1, 2, 3, 4, 5, 6, 7, 8];

            let mut buf = make_default_recvbuf(start_seq_no);
            assert!(buf.write_remaining_size() == DEFAULT_BUF_SZ);
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
                            (start_seq_no + total, start_seq_no + DEFAULT_BUF_SZ)
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

            assert_eq!(total, DEFAULT_BUF_SZ);
            assert_eq!(buf.write_remaining_size(), 0);
            assert_eq!(buf.read_remaining_size(), DEFAULT_BUF_SZ);
            assert_eq!(
                buf.write_range(),
                (start_seq_no + DEFAULT_BUF_SZ, start_seq_no + DEFAULT_BUF_SZ)
            );
        }

        #[test]
        fn contiguous_read_from_full() {
            let start_seq_no = 1291241;
            let data = [1, 2, 3, 4, 5, 6, 7, 8];

            let mut buf = make_default_recvbuf(start_seq_no);
            fill_buf(&mut buf, start_seq_no, &data);

            assert_eq!(buf.write_remaining_size(), 0);
            assert_eq!(buf.read_remaining_size(), DEFAULT_BUF_SZ);
            assert_eq!(
                buf.write_range(),
                (start_seq_no + DEFAULT_BUF_SZ, start_seq_no + DEFAULT_BUF_SZ)
            );

            let mut total_consumed = 0;
            let mut bytes = vec![0; 16];
            loop {
                let consumed = buf.consume(data.len(), &mut bytes).unwrap();
                if consumed.is_empty() {
                    break;
                }
                assert_eq!(&consumed, &data);
                total_consumed += consumed.len();

                assert_eq!(buf.read_remaining_size(), DEFAULT_BUF_SZ - total_consumed);
                assert_eq!(buf.write_remaining_size(), total_consumed);
                assert_eq!(
                    buf.write_range(),
                    (
                        start_seq_no + DEFAULT_BUF_SZ,
                        start_seq_no + DEFAULT_BUF_SZ + total_consumed
                    )
                );
            }

            assert_eq!(total_consumed, DEFAULT_BUF_SZ);
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
            let buf = Arc::new(Mutex::new(make_default_recvbuf(start_seq_no)));

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
                        let consumed = b
                            .consume(data.len() - curr, &mut consume_buf[curr..])
                            .unwrap();
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
            let mut buf = make_default_recvbuf(start_seq_no);

            //      |-------------WRITEABLE------------|
            // |xxxx-----------------------------------|
            //  ^   ^
            //  0   8

            buf.write(start_seq_no, &data).unwrap();
            assert!(!buf.has_early_arrival());
            assert_eq!(buf.write_remaining_size(), DEFAULT_BUF_SZ - data.len());
            assert_eq!(buf.expected_next(), 8);
            assert_eq!(
                buf.write_range(),
                (start_seq_no + data.len(), start_seq_no + DEFAULT_BUF_SZ)
            );

            //      |-------------WRITEABLE------------|
            // |xxxx-------------xxxx------------------|
            //  ^   ^            ^   ^
            //  0   8            100 108

            buf.write(start_seq_no + 100, &data).unwrap();
            assert!(buf.has_early_arrival());
            assert_eq!(buf.write_remaining_size(), DEFAULT_BUF_SZ - data.len());
            assert_eq!(buf.expected_next(), 8);
            assert_eq!(
                buf.write_range(),
                (start_seq_no + data.len(), start_seq_no + DEFAULT_BUF_SZ)
            );

            //      |-------------WRITEABLE------------|
            // |xxxx-------------xxxx----xxxx----------|
            //  ^   ^            ^   ^   ^   ^
            //  0   8            100 108 135 143

            buf.write(start_seq_no + 135, &data).unwrap();
            assert!(buf.has_early_arrival());
            assert_eq!(buf.write_remaining_size(), DEFAULT_BUF_SZ - data.len());
            assert_eq!(buf.expected_next(), 8);
            assert_eq!(
                buf.write_range(),
                (start_seq_no + data.len(), start_seq_no + DEFAULT_BUF_SZ)
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
                DEFAULT_BUF_SZ - data.len() - payload.len()
            );
            assert_eq!(
                buf.write_range(),
                (
                    start_seq_no + data.len() + payload.len(),
                    start_seq_no + DEFAULT_BUF_SZ
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
                (start_seq_no + 108, start_seq_no + DEFAULT_BUF_SZ)
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
                (start_seq_no + 208, start_seq_no + DEFAULT_BUF_SZ)
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
            let buf = Arc::new(Mutex::new(make_default_recvbuf(start_seq_no)));

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
                                if matches!(e, WriteRangeError::ExceedBuffer) {
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
                                    if matches!(e, WriteRangeError::ExceedBuffer) {
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
                        let consumed = b
                            .consume(data.len() - curr, &mut consume_buf[curr..])
                            .unwrap();
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

        fn fill_buf<const N: usize>(buf: &mut RecvBuf<N>, start_seq_no: usize, data: &[u8]) {
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
