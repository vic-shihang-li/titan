use crate::node::Node;
use crate::protocol::tcp::{
    Port, SocketDescriptor, TcpAcceptError, TcpConnError, TcpListenError, TcpSendError,
};
use crate::protocol::Protocol;
use rustyline::{error::ReadlineError, Editor};
use std::fmt::Display;
use std::fs::File;
use std::io::Write;
use std::net::Ipv4Addr;
use std::str::SplitWhitespace;
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
    ReadSocket(TCPReadCmd),
    Shutdown(SocketDescriptor, TcpShutdownKind),
    Close(SocketDescriptor),
    SendFile(SendFileCmd),
    RecvFile(RecvFileCmd),
    Quit,
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
pub struct TCPReadCmd {
    descriptor: SocketDescriptor,
    num_bytes: usize,
    would_block: bool,
}

impl TCPReadCmd {
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

impl From<TCPReadCmd> for Command {
    fn from(r: TCPReadCmd) -> Self {
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

impl Cli {
    pub fn new(node: Arc<Node>) -> Self {
        Self { node }
    }

    pub async fn run(&self) {
        let mut rl = Editor::<()>::new().unwrap();
        let mut shutdown_flag = false;
        loop {
            let readline = rl.readline(">> ");
            match readline {
                Ok(mut line) => {
                    line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if line == "q" {
                        eprintln!("Commencing Graceful Shutdown");
                        shutdown_flag = true;
                    }
                    match Self::parse_command(line) {
                        Ok(cmd) => {
                            self.execute_command(cmd).await;
                        }
                        Err(e) => {
                            eprintln!("{e}")
                        }
                    }
                    if shutdown_flag {
                        break;
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    eprintln!("CTRL-C");
                    break;
                }
                Err(ReadlineError::Eof) => {
                    eprintln!("CTRL-D");
                    break;
                }
                Err(err) => {
                    eprintln!("Error: {:?}", err);
                    break;
                }
            }
        }
    }

    fn parse_command(line: String) -> Result<Command, ParseError> {
        let mut tokens = line.split_whitespace();
        let cmd = tokens.next().unwrap();
        eprintln!("cmd: {}", cmd);
        cmd_arg_handler(cmd, tokens)
    }

    async fn execute_command(&self, cmd: Command) {
        match cmd {
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
            Command::ConnectSocket(_ip, _port) => {
                todo!() //TODO implement
            }
            Command::ReadSocket(_cmd) => {
                todo!() //TODO implement
            }
            Command::Shutdown(_socket, _option) => {
                todo!() //TODO implement
            }
            Command::Close(socket_descriptor) => {
                self.close_socket(socket_descriptor).await;
            }
            Command::SendFile(cmd) => {
                self.send_file(cmd).await;
            }
            Command::RecvFile(_cmd) => {
                todo!()
            }
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

    async fn print_routes(&self, file: Option<String>) {
        let rt = self.node.get_forwarding_table().await;
        match file {
            Some(file) => {
                let mut f = File::create(file).unwrap();
                f.write_all(b"dest\t\tnext\t\tcost\n").unwrap();
                for route in rt.entries() {
                    f.write_all(format!("{}\n", route).as_bytes()).unwrap();
                }
            }
            None => {
                println!("dest\t\tnext\t\tcost");
                for route in rt.entries() {
                    println!("{}", route)
                }
            }
        }
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

    async fn open_listen_socket_on(&self, port: Port) {
        if let Err(e) = self.node.listen(port).await {
            eprintln!("Failed to listen on port {}. Error: {:?}", port.0, e)
        }
    }

    async fn send_file(&self, cmd: SendFileCmd) {
        let node = self.node.clone();
        tokio::spawn(async move {
            if let Err(e) = node.send_file(cmd).await {
                eprintln!("Failed to send file. Error: {:?}", e)
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

fn cmd_arg_handler(cmd: &str, mut tokens: SplitWhitespace) -> Result<Command, ParseError> {
    match cmd {
        "li" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => Ok(Command::ListInterface(Some(arg.to_string()))),
                None => Ok(Command::ListInterface(None)),
            }
        }
        "interfaces" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => Ok(Command::ListInterface(Some(arg.to_string()))),
                None => Ok(Command::ListInterface(None)),
            }
        }
        "lr" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => Ok(Command::ListRoute(Some(arg.to_string()))),
                None => Ok(Command::ListRoute(None)),
            }
        }
        "routes" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => Ok(Command::ListRoute(Some(arg.to_string()))),
                None => Ok(Command::ListRoute(None)),
            }
        }
        "down" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => {
                    let link_no = arg.parse::<u16>();
                    match link_no {
                        Ok(link_no) => Ok(Command::InterfaceDown(link_no)),
                        Err(_) => Err(ParseDownError::InvalidLinkId.into()),
                    }
                }
                None => Err(ParseDownError::NoLinkId.into()),
            }
        }
        "up" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => {
                    let link_no = arg.parse::<u16>();
                    match link_no {
                        Ok(link_no) => Ok(Command::InterfaceUp(link_no)),
                        Err(_) => Err(ParseUpError::InvalidLinkId.into()),
                    }
                }
                None => Err(ParseUpError::NoLinkId.into()),
            }
        }
        "send" => {
            let virtual_ip = tokens.next().ok_or(ParseSendError::NoIp)?;
            let protocol = tokens.next().ok_or(ParseSendError::NoProtocol)?;

            let mut payload = String::new();
            for token in tokens {
                payload.push_str(token);
            }

            let virtual_ip = virtual_ip.parse().map_err(|_| ParseSendError::InvalidIp)?;
            let protocol =
                Protocol::try_from(protocol).map_err(|_| ParseSendError::InvalidProtocol)?;
            Ok(Command::SendIPv4Packet(IPv4SendCmd {
                virtual_ip,
                protocol,
                payload,
            }))
        }
        "ls" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => Ok(Command::ListSockets(Some(arg.to_string()))),
                None => Ok(Command::ListSockets(None)),
            }
        }
        "a" => {
            let arg = tokens.next().ok_or(ParseOpenListenSocketError::NoPort)?;
            let port = arg
                .parse::<u16>()
                .map_err(|_| ParseOpenListenSocketError::InvalidPort)?;
            Ok(Command::OpenListenSocket(Port(port)))
        }
        "c" => {
            let ip = tokens.next().ok_or(ParseConnectError::NoIp)?;
            let ip = ip.parse().map_err(|_| ParseConnectError::InvalidIp)?;
            let port = tokens.next().ok_or(ParseConnectError::NoPort)?;
            let port: u16 = port.parse().map_err(|_| ParseConnectError::InvalidPort)?;
            Ok(Command::ConnectSocket(ip, port.into()))
        }
        "s" => {
            let sid = tokens.next().ok_or(ParseTcpSendError::NoSocketDescriptor)?;
            let sid = SocketDescriptor(
                sid.parse()
                    .map_err(|_| ParseTcpSendError::InvalidSocketDescriptor)?,
            );
            let payload = tokens.next().ok_or(ParseTcpSendError::NoPayload)?;
            Ok(Command::SendTCPPacket(sid, payload.as_bytes().into()))
        }
        "r" => {
            let sid = tokens.next().ok_or(ParseTcpReadError::NoSocketDescriptor)?;
            let sid = SocketDescriptor(
                sid.parse()
                    .map_err(|_| ParseTcpReadError::InvalidSocketDescriptor)?,
            );
            let num_bytes: usize = tokens
                .next()
                .ok_or(ParseTcpReadError::NoNumBytesToRead)?
                .parse()
                .map_err(|_| ParseTcpReadError::InvalidNumBytesToRead)?;

            let maybe_blocking = tokens.next();
            let blocking = match maybe_blocking {
                Some(token) => match token {
                    "y" => true,
                    "N" => false,
                    _ => return Err(ParseTcpReadError::InvalidBlockingIndicator.into()),
                },
                None => false,
            };

            match blocking {
                true => Ok(TCPReadCmd::new_blocking(sid, num_bytes).into()),
                false => Ok(TCPReadCmd::new_nonblocking(sid, num_bytes).into()),
            }
        }
        "sd" => {
            let sid = tokens
                .next()
                .ok_or(ParseTcpShutdownError::NoSocketDescriptor)?;
            let sid = SocketDescriptor(
                sid.parse()
                    .map_err(|_| ParseTcpShutdownError::InvalidSocketDescriptor)?,
            );

            let maybe_option = tokens.next();
            let opt = match maybe_option {
                Some(token) => match token {
                    "w" | "write" => TcpShutdownKind::Write,
                    "r" | "read" => TcpShutdownKind::Read,

                    "both" => TcpShutdownKind::ReadWrite,
                    _ => {
                        return Err(ParseTcpShutdownError::InvalidShutdownType(token.into()).into())
                    }
                },
                None => TcpShutdownKind::Write,
            };

            Ok(Command::Shutdown(sid, opt))
        }
        "cl" => {
            let sid = tokens
                .next()
                .ok_or(ParseTcpShutdownError::NoSocketDescriptor)?;
            let sid = SocketDescriptor(
                sid.parse()
                    .map_err(|_| ParseTcpShutdownError::InvalidSocketDescriptor)?,
            );

            Ok(Command::Close(sid))
        }
        "sf" => {
            let filename = tokens.next().ok_or(ParseSendFileError::NoFile)?;
            let ip = tokens
                .next()
                .ok_or(ParseSendFileError::NoIp)?
                .parse()
                .map_err(|_| ParseSendFileError::InvalidIp)?;
            let port = tokens
                .next()
                .ok_or(ParseSendFileError::NoPort)?
                .parse::<u16>()
                .map_err(|_| ParseSendFileError::InvalidPort)?
                .into();

            Ok(SendFileCmd::new(filename.into(), ip, port).into())
        }
        "rf" => {
            let filename = tokens.next().ok_or(ParseRecvFileError::NoFile)?;
            let port = tokens
                .next()
                .ok_or(ParseRecvFileError::NoPort)?
                .parse::<u16>()
                .map_err(|_| ParseRecvFileError::InvalidPort)?
                .into();
            Ok(RecvFileCmd::new(filename.into(), port).into())
        }
        "q" => Ok(Command::Quit),
        _ => Err(ParseError::Unknown),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseOpenListenSocketError {
    NoPort,
    InvalidPort,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseDownError {
    InvalidLinkId,
    NoLinkId,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseUpError {
    InvalidLinkId,
    NoLinkId,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseSendError {
    NoIp,
    InvalidIp,
    NoProtocol,
    InvalidProtocol,
    NoPayload,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseConnectError {
    NoIp,
    InvalidIp,
    NoPort,
    InvalidPort,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseTcpSendError {
    NoSocketDescriptor,
    InvalidSocketDescriptor,
    NoPayload,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseTcpReadError {
    NoSocketDescriptor,
    InvalidSocketDescriptor,
    NoNumBytesToRead,
    InvalidNumBytesToRead,
    InvalidBlockingIndicator,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseTcpShutdownError {
    NoSocketDescriptor,
    InvalidSocketDescriptor,
    InvalidShutdownType(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseCloseError {
    NoSocketDescriptor,
    InvalidSocketDescriptor,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseSendFileError {
    NoFile,
    NoIp,
    InvalidIp,
    NoPort,
    InvalidPort,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseRecvFileError {
    NoFile,
    NoPort,
    InvalidPort,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    Unknown,
    Down(ParseDownError),
    Up(ParseUpError),
    Send(ParseSendError),
    OpenListenSocket(ParseOpenListenSocketError),
    Connect(ParseConnectError),
    TcpSend(ParseTcpSendError),
    TcpRead(ParseTcpReadError),
    TcpShutdown(ParseTcpShutdownError),
    TcpClose(ParseCloseError),
    SendFile(ParseSendFileError),
    RecvFile(ParseRecvFileError),
}

impl Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Unknown => write!(f, "Unknown command"),
            ParseError::Down(e) => write!(
                f,
                "Invalid down command. Usage: down <integer>. Error: {:?}",
                e
            ),
            ParseError::Up(e) => {
                write!(f, "Invalid up command. Usage: up <integer>. Error: {:?}", e)
            }
            ParseError::Send(e) => {
                write!(
                    f,
                    "Invalid send command. Usage: send <vip> <proto> <string>. Error: {:?}",
                    e
                )
            }
            ParseError::OpenListenSocket(e) => {
                write!(
                    f,
                    "Invalid open socket command. Usage: a <port>. Error: {:?}",
                    e
                )
            }
            ParseError::Connect(e) => {
                write!(
                    f,
                    "Invalid connect command. Usage: c <ip> <port>. Error: {:?}",
                    e
                )
            }
            ParseError::TcpSend(e) => {
                write!(
                    f,
                    "Invalid send command. Usage: s <socket_id> <data>. Error: {:?}",
                    e
                )
            }
            ParseError::TcpRead(e) => {
                write!(
                    f,
                    "Invalid read command. Usage: r <socket ID> <numbytes> <y|N>. Error: {:?}",
                    e
                )
            }
            ParseError::TcpShutdown(e) => {
                write!(
                    f,
                    "Invalid shutdown command. Usage: sd <socket ID> <read|write|both>. Error: {:?}",
                    e
                )
            }
            ParseError::TcpClose(e) => {
                write!(
                    f,
                    "Invalid close command. Usage: cl <socket ID>. Error: {:?}",
                    e
                )
            }
            ParseError::SendFile(e) => {
                write!(
                    f,
                    "Invalid send file command. Usage: sf <filename> <ip> <port>. Error: {:?}",
                    e
                )
            }
            ParseError::RecvFile(e) => {
                write!(
                    f,
                    "Invalid receive file command. Usage: rf <filename> <port>. Error: {:?}",
                    e
                )
            }
        }
    }
}

impl From<ParseUpError> for ParseError {
    fn from(v: ParseUpError) -> Self {
        ParseError::Up(v)
    }
}

impl From<ParseDownError> for ParseError {
    fn from(v: ParseDownError) -> Self {
        ParseError::Down(v)
    }
}

impl From<ParseSendError> for ParseError {
    fn from(v: ParseSendError) -> Self {
        ParseError::Send(v)
    }
}

impl From<ParseOpenListenSocketError> for ParseError {
    fn from(v: ParseOpenListenSocketError) -> Self {
        ParseError::OpenListenSocket(v)
    }
}

impl From<ParseConnectError> for ParseError {
    fn from(v: ParseConnectError) -> Self {
        ParseError::Connect(v)
    }
}

impl From<ParseTcpSendError> for ParseError {
    fn from(v: ParseTcpSendError) -> Self {
        ParseError::TcpSend(v)
    }
}

impl From<ParseTcpReadError> for ParseError {
    fn from(v: ParseTcpReadError) -> Self {
        ParseError::TcpRead(v)
    }
}

impl From<ParseTcpShutdownError> for ParseError {
    fn from(v: ParseTcpShutdownError) -> Self {
        ParseError::TcpShutdown(v)
    }
}

impl From<ParseSendFileError> for ParseError {
    fn from(v: ParseSendFileError) -> Self {
        ParseError::SendFile(v)
    }
}

impl From<ParseRecvFileError> for ParseError {
    fn from(v: ParseRecvFileError) -> Self {
        ParseError::RecvFile(v)
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn parse_connect() {
        assert_eq!(
            Cli::parse_command("c".into()).unwrap_err(),
            ParseConnectError::NoIp.into()
        );

        assert_eq!(
            Cli::parse_command("c 1".into()).unwrap_err(),
            ParseConnectError::InvalidIp.into()
        );

        assert_eq!(
            Cli::parse_command("c 1.2.3.4".into()).unwrap_err(),
            ParseConnectError::NoPort.into()
        );

        assert_eq!(
            Cli::parse_command("c 1.2.3.4 ss".into()).unwrap_err(),
            ParseConnectError::InvalidPort.into()
        );

        let c = Cli::parse_command("c 1.2.3.4 33".into()).unwrap();
        let expected = Command::ConnectSocket(Ipv4Addr::new(1, 2, 3, 4), 33u16.into());
        assert_eq!(c, expected);
    }

    #[test]
    fn parse_tcp_send() {
        assert_eq!(
            Cli::parse_command("s".into()).unwrap_err(),
            ParseTcpSendError::NoSocketDescriptor.into(),
        );

        assert_eq!(
            Cli::parse_command("s ssss".into()).unwrap_err(),
            ParseTcpSendError::InvalidSocketDescriptor.into(),
        );

        assert_eq!(
            Cli::parse_command("s 33".into()).unwrap_err(),
            ParseTcpSendError::NoPayload.into(),
        );

        let c = Cli::parse_command("s 33 heehee".into()).unwrap();
        let expected = Command::SendTCPPacket(
            SocketDescriptor(33),
            String::from("heehee").as_bytes().into(),
        );
        assert_eq!(c, expected);
    }

    #[test]
    fn parse_tcp_read() {
        assert_eq!(
            Cli::parse_command("r".into()).unwrap_err(),
            ParseTcpReadError::NoSocketDescriptor.into(),
        );

        assert_eq!(
            Cli::parse_command("r ssss".into()).unwrap_err(),
            ParseTcpReadError::InvalidSocketDescriptor.into(),
        );

        assert_eq!(
            Cli::parse_command("r 33".into()).unwrap_err(),
            ParseTcpReadError::NoNumBytesToRead.into(),
        );

        assert_eq!(
            Cli::parse_command("r 33 heehee".into()).unwrap_err(),
            ParseTcpReadError::InvalidNumBytesToRead.into()
        );

        assert_eq!(
            Cli::parse_command("r 33 100 n".into()).unwrap_err(),
            ParseTcpReadError::InvalidBlockingIndicator.into()
        );

        let c = Cli::parse_command("r 33 100 y".into()).unwrap();
        assert_eq!(
            c,
            TCPReadCmd::new_blocking(SocketDescriptor(33), 100).into()
        );
        let c = Cli::parse_command("r 33 100 N".into()).unwrap();
        assert_eq!(
            c,
            TCPReadCmd::new_nonblocking(SocketDescriptor(33), 100).into()
        );
        let c = Cli::parse_command("r 33 100".into()).unwrap();
        assert_eq!(
            c,
            TCPReadCmd::new_nonblocking(SocketDescriptor(33), 100).into()
        );
    }

    #[test]
    fn parse_tcp_shutdown() {
        assert_eq!(
            Cli::parse_command("sd".into()).unwrap_err(),
            ParseTcpShutdownError::NoSocketDescriptor.into(),
        );

        assert_eq!(
            Cli::parse_command("sd ss".into()).unwrap_err(),
            ParseTcpShutdownError::InvalidSocketDescriptor.into(),
        );

        assert_eq!(
            Cli::parse_command("sd 3 yes".into()).unwrap_err(),
            ParseTcpShutdownError::InvalidShutdownType("yes".into()).into(),
        );

        let c = Cli::parse_command("sd 3".into()).unwrap();
        assert_eq!(
            c,
            Command::Shutdown(SocketDescriptor(3), TcpShutdownKind::Write)
        );

        let c = Cli::parse_command("sd 3 r".into()).unwrap();
        assert_eq!(
            c,
            Command::Shutdown(SocketDescriptor(3), TcpShutdownKind::Read)
        );

        let c = Cli::parse_command("sd 3 read".into()).unwrap();
        assert_eq!(
            c,
            Command::Shutdown(SocketDescriptor(3), TcpShutdownKind::Read)
        );

        let c = Cli::parse_command("sd 3 w".into()).unwrap();
        assert_eq!(
            c,
            Command::Shutdown(SocketDescriptor(3), TcpShutdownKind::Write)
        );

        let c = Cli::parse_command("sd 3 write".into()).unwrap();
        assert_eq!(
            c,
            Command::Shutdown(SocketDescriptor(3), TcpShutdownKind::Write)
        );

        let c = Cli::parse_command("sd 3 both".into()).unwrap();
        assert_eq!(
            c,
            Command::Shutdown(SocketDescriptor(3), TcpShutdownKind::ReadWrite)
        );
    }

    #[test]
    fn parse_close_socket() {
        assert_eq!(
            Cli::parse_command("cl".into()).unwrap_err(),
            ParseTcpShutdownError::NoSocketDescriptor.into(),
        );

        assert_eq!(
            Cli::parse_command("cl xx".into()).unwrap_err(),
            ParseTcpShutdownError::InvalidSocketDescriptor.into(),
        );

        let c = Cli::parse_command("cl 33".into()).unwrap();
        assert_eq!(c, Command::Close(SocketDescriptor(33)));
    }

    #[test]
    fn parse_send_file() {
        assert_eq!(
            Cli::parse_command("sf".into()).unwrap_err(),
            ParseSendFileError::NoFile.into(),
        );

        assert_eq!(
            Cli::parse_command("sf hello_world".into()).unwrap_err(),
            ParseSendFileError::NoIp.into(),
        );

        assert_eq!(
            Cli::parse_command("sf hello_world 121".into()).unwrap_err(),
            ParseSendFileError::InvalidIp.into(),
        );

        assert_eq!(
            Cli::parse_command("sf hello_world 1.2.3.4".into()).unwrap_err(),
            ParseSendFileError::NoPort.into(),
        );

        assert_eq!(
            Cli::parse_command("sf hello_world 1.2.3.4 xxx".into()).unwrap_err(),
            ParseSendFileError::InvalidPort.into(),
        );

        let c = Cli::parse_command("sf hello 1.2.3.4 3434".into()).unwrap();
        assert_eq!(
            c,
            Command::SendFile(SendFileCmd::new(
                "hello".into(),
                Ipv4Addr::new(1, 2, 3, 4),
                Port(3434)
            ))
        );
    }

    #[test]
    fn parse_recv_file() {
        assert_eq!(
            Cli::parse_command("rf".into()).unwrap_err(),
            ParseRecvFileError::NoFile.into()
        );

        assert_eq!(
            Cli::parse_command("rf hello".into()).unwrap_err(),
            ParseRecvFileError::NoPort.into()
        );

        assert_eq!(
            Cli::parse_command("rf hello xx".into()).unwrap_err(),
            ParseRecvFileError::InvalidPort.into()
        );

        let c = Cli::parse_command("rf hello 5000".into()).unwrap();
        assert_eq!(
            c,
            Command::RecvFile(RecvFileCmd::new(String::from("hello"), Port(5000)))
        );
    }
}
