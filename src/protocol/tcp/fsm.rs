
pub struct TcpConnection<S> {
    id: u16,
    state: S,
}

impl TcpConnection<Closed> {
    pub fn new(sock: u16) -> Self {
        TcpConnection {
            id: sock,
            state: Closed::new(sock),
        }
    }
}
pub struct Closed {
    socket: u16,
}

impl Closed {
    fn new(sock: u16) -> Self {
        Closed {
            socket: sock,
        }
    }
}

pub struct Listen {
    socket: u16,
}


struct SynSent {
    socket: u16,
}

struct SynReceived {
    socket: u16,
}

struct Established {
    socket: u16,
}

struct FinWait1 {
    socket: u16,
}

struct FinWait2 {
    socket: u16,
}

struct Closing {
    socket: u16,
}

struct TimeWait {
    socket: u16,
}

struct CloseWait {
    socket: u16,
}

struct LastAck {
    socket: u16,
}