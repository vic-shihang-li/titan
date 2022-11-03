use std::cmp::min;

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

const DEFAULT_SENDBUF_SZ: usize = 1 << 16;

pub fn make_default_sendbuf() -> SendBuf<DEFAULT_SENDBUF_SZ> {
    SendBuf::<DEFAULT_SENDBUF_SZ>::new()
}

#[derive(Debug)]
pub enum AdvanceError {
    TooBig,
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
    pub fn slice(&self, n_bytes: usize) -> Option<ByteSlice<'a>> {
        if n_bytes <= self.len() {
            match self.second {
                Some(second) => {
                    if n_bytes <= self.first.len() {
                        Some(ByteSlice::new(&self.first[..n_bytes], None))
                    } else {
                        Some(ByteSlice::new(
                            self.first,
                            Some(&second[..(n_bytes - self.first.len())]),
                        ))
                    }
                }

                None => Some(ByteSlice::new(&self.first[..n_bytes], None)),
            }
        } else {
            None
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
    pub fn unconsumed<'a>(&'a self) -> ByteSlice<'a> {
        let start = self.tail % self.size();
        let end = self.head % self.size();

        match start.cmp(&end) {
            std::cmp::Ordering::Less => ByteSlice::new(&self.buf[start..end], None),
            std::cmp::Ordering::Equal => ByteSlice::new(&self.buf[start..end], None),
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

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        thread::yield_now,
    };

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
                    let pending_buf = buf.unconsumed().slice(expect_len).unwrap();
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
                    if b.read_remaining_size() < data.len() {
                        drop(b);
                        yield_now();
                        continue;
                    }

                    let got = b.unconsumed().slice(data.len()).unwrap();
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
