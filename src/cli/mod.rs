mod parse;

use crate::node::Node;
use crate::protocol::tcp::prelude::{Port, SocketDescriptor};
use crate::protocol::tcp::{TcpAcceptError, TcpConnError, TcpListenError, TcpSendError};
use crate::protocol::Protocol;
use crate::repl::{HandleUserInput, HandleUserInputError, Repl};
use async_trait::async_trait;
use std::fs::File;
use std::io::Write;
use std::net::Ipv4Addr;
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    ListInterface(Option<String>),
    ListRoute(Option<String>),
    ListSockets(Option<String>),
    InterfaceDown(u16),
    InterfaceUp(u16),
    SendIPv4Packet(IPv4SendCmd),
    SendTCPPacket(SocketDescriptor, Vec<u8>),
    OpenListenSocket(Port),
    ConnectSocket(Ipv4Addr, Port),
    ReadSocket(TcpReadCmd),
    Shutdown(SocketDescriptor, TcpShutdownKind),
    Close(SocketDescriptor),
    SendFile(SendFileCmd),
    RecvFile(RecvFileCmd),
    Quit,
    None,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TcpShutdownKind {
    Read,
    Write,
    ReadWrite,
}

#[derive(Debug)]
pub enum SendFileError {
    OpenFile(std::io::Error),
    ReadFile(std::io::Error),
    Connect(TcpConnError),
    Send(TcpSendError),
}

#[derive(Debug, PartialEq, Eq)]
pub struct SendFileCmd {
    pub path: String,
    pub dest_ip: Ipv4Addr,
    pub port: Port,
}

impl SendFileCmd {
    fn new(path: String, dest_ip: Ipv4Addr, port: Port) -> Self {
        Self {
            path,
            dest_ip,
            port,
        }
    }
}
impl From<SendFileCmd> for Command {
    fn from(s: SendFileCmd) -> Self {
        Command::SendFile(s)
    }
}

#[derive(Debug)]
pub enum RecvFileError {
    FileIo(std::io::Error),
    Listen(TcpListenError),
    Accept(TcpAcceptError),
}

impl From<std::io::Error> for RecvFileError {
    fn from(e: std::io::Error) -> Self {
        RecvFileError::FileIo(e)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct RecvFileCmd {
    pub out_path: String,
    pub port: Port,
}

impl RecvFileCmd {
    fn new(out_path: String, port: Port) -> Self {
        Self { out_path, port }
    }
}

impl From<RecvFileCmd> for Command {
    fn from(s: RecvFileCmd) -> Self {
        Command::RecvFile(s)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct TcpReadCmd {
    descriptor: SocketDescriptor,
    num_bytes: usize,
    would_block: bool,
}

impl TcpReadCmd {
    fn new_blocking(descriptor: SocketDescriptor, num_bytes: usize) -> Self {
        Self {
            descriptor,
            num_bytes,
            would_block: true,
        }
    }

    fn new_nonblocking(descriptor: SocketDescriptor, num_bytes: usize) -> Self {
        Self {
            descriptor,
            num_bytes,
            would_block: false,
        }
    }
}

impl From<TcpReadCmd> for Command {
    fn from(r: TcpReadCmd) -> Self {
        Command::ReadSocket(r)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct IPv4SendCmd {
    virtual_ip: Ipv4Addr,
    protocol: Protocol,
    payload: String,
}

pub struct Cli {
    node: Arc<Node>,
}

#[async_trait]
impl HandleUserInput for Cli {
    async fn handle(
        &mut self,
        user_input: String,
    ) -> Result<(), crate::repl::HandleUserInputError> {
        match parse::parse_command(user_input) {
            Ok(Command::Quit) => return Err(HandleUserInputError::Terminate),
            Ok(Command::None) => (),
            Ok(cmd) => self.execute_command(cmd).await,
            Err(e) => {
                eprintln!("{e}");
            }
        };

        Ok(())
    }
}

impl Cli {
    pub fn new(node: Arc<Node>) -> Self {
        Self { node }
    }

    pub async fn run(self) {
        let h = tokio::spawn(async move {
            Repl::new(self, Some(">> ".into())).serve().await;
        });
        h.await.expect("CLI should not panic");
    }

    async fn execute_command(&self, cmd: Command) {
        match cmd {
            Command::None => (),
            Command::ListInterface(op) => {
                self.print_interfaces(op).await;
            }
            Command::ListRoute(op) => {
                self.print_routes(op).await;
            }
            Command::ListSockets(op) => {
                self.print_sockets(op).await;
            }
            Command::InterfaceDown(interface) => {
                eprintln!("Turning down interface {}", interface);
                if let Err(e) = self.node.deactivate(interface).await {
                    eprintln!("Failed to turn interface {} down: {:?}", interface, e);
                }
            }
            Command::InterfaceUp(interface) => {
                eprintln!("Turning up interface {}", interface);
                if let Err(e) = self.node.activate(interface).await {
                    eprintln!("Failed to turn interface {} up: {:?}", interface, e);
                }
            }
            Command::SendIPv4Packet(cmd) => {
                eprintln!(
                    "Sending packet \"{}\" with protocol {:?} to {}",
                    cmd.payload, cmd.protocol, cmd.virtual_ip
                );
                if let Err(e) = self
                    .node
                    .send(cmd.payload.as_bytes(), cmd.protocol, cmd.virtual_ip)
                    .await
                {
                    eprintln!("Failed to send packet: {:?}", e);
                }
            }
            Command::SendTCPPacket(socket_descriptor, payload) => {
                self.tcp_send(socket_descriptor, payload).await;
            }
            Command::OpenListenSocket(port) => {
                self.open_listen_socket_on(port).await;
            }
            Command::ConnectSocket(ip, port) => {
                self.connect(ip, port).await;
            }
            Command::ReadSocket(cmd) => {
                self.tcp_read(cmd).await;
            }
            Command::Shutdown(socket, opt) => {
                self.shutdown(socket, opt).await;
            }
            Command::Close(socket_descriptor) => {
                self.close_socket(socket_descriptor).await;
            }
            Command::SendFile(cmd) => {
                self.send_file(cmd);
            }
            Command::RecvFile(cmd) => self.recv_file(cmd),
            Command::Quit => {
                eprintln!("Quitting");
            }
        }
    }

    async fn print_interfaces(&self, file: Option<String>) {
        let mut id = 0;
        match file {
            Some(file) => {
                let mut f = File::create(file).unwrap();
                f.write_all(b"id\tstate\tlocal\t\tremote\tport\n").unwrap();
                for link in &*self.node.iter_links().await {
                    f.write_all(format!("{}\t{}\n", id, link).as_bytes())
                        .unwrap();
                    id += 1;
                }
            }
            None => {
                println!("id\tstate\tlocal\t\tremote\t        port");
                for link in &*self.node.iter_links().await {
                    println!("{}\t{}", id, link);
                    id += 1;
                }
            }
        }
    }

    async fn print_routes(&self, _file: Option<String>) {
        todo!()
    }

    async fn print_sockets(&self, file: Option<String>) {
        self.node.print_sockets(file).await;
    }

    async fn tcp_send(&self, socket_descriptor: SocketDescriptor, payload: Vec<u8>) {
        if let Err(e) = self.node.tcp_send(socket_descriptor, &payload).await {
            eprintln!(
                "Failed to send on socket {}. Error: {:?}",
                socket_descriptor.0, e
            )
        }
    }

    async fn tcp_read(&self, cmd: TcpReadCmd) {
        if cmd.would_block {
            match self.node.tcp_read(cmd.descriptor, cmd.num_bytes).await {
                Ok(bytes) => {
                    println!("{}", String::from_utf8_lossy(&bytes))
                }
                Err(e) => {
                    eprintln!("Failed to read: {:?}", e);
                }
            }
        } else {
            let node = self.node.clone();
            tokio::spawn(async move {
                match node.tcp_read(cmd.descriptor, cmd.num_bytes).await {
                    Ok(bytes) => {
                        println!("{}", String::from_utf8_lossy(&bytes));
                    }
                    Err(e) => {
                        eprintln!("Failed to read: {:?}", e);
                    }
                }
            });
        }
    }

    async fn shutdown(&self, descriptor: SocketDescriptor, option: TcpShutdownKind) {
        match self.node.get_socket_by_descriptor(descriptor).await {
            Some(socket) => match option {
                TcpShutdownKind::Read => socket.close_read().await,
                TcpShutdownKind::Write => socket.close().await,
                TcpShutdownKind::ReadWrite => socket.close_rw().await,
            },
            None => {
                eprintln!("Socket {} not found", descriptor.0)
            }
        }
    }

    async fn open_listen_socket_on(&self, port: Port) {
        match self.node.listen(port).await {
            Ok(_) => eprintln!("Listen socket opened on port {}", port.0),
            Err(e) => {
                eprintln!("Failed to listen on port {}. Error: {:?}", port.0, e)
            }
        }
    }

    async fn connect(&self, ip: Ipv4Addr, port: Port) {
        match self.node.connect(ip, port).await {
            Ok(conn) => {
                let socket_descriptor = self
                    .node
                    .get_socket_descriptor(conn.socket_id())
                    .await
                    .unwrap();
                eprintln!("Connection established. ID: {}", socket_descriptor.0);
            }
            Err(e) => {
                eprintln!("Failed to connect to {}:{}. Error: {:?}", ip, port.0, e)
            }
        }
    }

    fn send_file(&self, cmd: SendFileCmd) {
        let node = self.node.clone();
        tokio::spawn(async move {
            match node.send_file(cmd).await {
                Ok(_) => {
                    eprintln!("Send file complete.");
                }
                Err(e) => {
                    eprintln!("Failed to send file. Error: {:?}", e)
                }
            }
        });
    }

    fn recv_file(&self, cmd: RecvFileCmd) {
        let node = self.node.clone();
        tokio::spawn(async move {
            match node.recv_file(cmd).await {
                Ok(_) => {
                    eprintln!("Receive file complete");
                }
                Err(e) => {
                    eprintln!("Failed to receive file. Error: {:?}", e)
                }
            }
        });
    }

    async fn close_socket(&self, socket_descriptor: SocketDescriptor) {
        if self
            .node
            .close_socket_by_descriptor(socket_descriptor)
            .await
            .is_err()
        {
            eprintln!(
                "Failed to close socket: socket {} does not exist",
                socket_descriptor.0
            );
        }
    }
}
