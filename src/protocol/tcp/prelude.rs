use std::net::Ipv4Addr;

/// A tuple that uniquely identifies a remote location.
#[derive(Debug, Copy, Clone)]
pub struct Remote((Ipv4Addr, Port));

impl Remote {
    pub fn new(addr: Ipv4Addr, port: Port) -> Self {
        Self((addr, port))
    }

    pub fn ip(&self) -> Ipv4Addr {
        self.0 .0
    }

    pub fn port(&self) -> Port {
        self.0 .1
    }
}

#[derive(Hash, PartialEq, Eq, Debug, Copy, Clone)]
pub struct SocketDescriptor(pub u16);

impl From<u16> for SocketDescriptor {
    fn from(s: u16) -> Self {
        SocketDescriptor(s)
    }
}

#[derive(Hash, PartialEq, Eq, Debug, Copy, Clone)]
pub struct Port(pub u16);

impl From<u16> for Port {
    fn from(p: u16) -> Self {
        Port(p)
    }
}

#[derive(Hash, PartialEq, Eq, Debug, Copy, Clone)]
pub struct SocketId {
    remote: (Ipv4Addr, Port),
    local_port: Port,
}

impl SocketId {
    pub fn build() -> SocketIdBuilder {
        SocketIdBuilder::default()
    }

    pub fn for_listen_socket(local_port: Port) -> Self {
        Self {
            remote: (Ipv4Addr::new(0, 0, 0, 0), Port(0)),
            local_port,
        }
    }

    pub fn remote(&self) -> Remote {
        Remote::new(self.remote_ip(), self.remote_port())
    }

    pub fn remote_ip(&self) -> Ipv4Addr {
        self.remote.0
    }

    pub fn remote_port(&self) -> Port {
        self.remote.1
    }

    pub fn local_port(&self) -> Port {
        self.local_port
    }
}

#[derive(Default)]
pub struct SocketIdBuilder {
    remote_ip: Option<Ipv4Addr>,
    remote_port: Option<Port>,
    local_port: Option<Port>,
}

#[derive(Debug)]
pub enum BuildSocketIdError {
    NoRemoteIp,
    NoRemotePort,
    NoLocalPort,
}

impl SocketIdBuilder {
    pub fn with_remote_ip(&mut self, remote_ip: Ipv4Addr) -> &mut Self {
        self.remote_ip = Some(remote_ip);
        self
    }

    pub fn with_remote_port(&mut self, remote_port: Port) -> &mut Self {
        self.remote_port = Some(remote_port);
        self
    }

    pub fn with_local_port(&mut self, local_port: Port) -> &mut Self {
        self.local_port = Some(local_port);
        self
    }

    pub fn build(&self) -> Result<SocketId, BuildSocketIdError> {
        Ok(SocketId {
            remote: (
                self.remote_ip.ok_or(BuildSocketIdError::NoRemoteIp)?,
                self.remote_port.ok_or(BuildSocketIdError::NoRemotePort)?,
            ),
            local_port: self.local_port.ok_or(BuildSocketIdError::NoLocalPort)?,
        })
    }
}
