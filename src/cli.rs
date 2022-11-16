use crate::node::Node;
use crate::protocol::tcp::{Port, SocketDescriptor};
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
    SendTCPPacket(TCPSendCmd),
    OpenSocket(u16),
    ConnectSocket(Ipv4Addr, Port),
    ReadSocket(TCPReadCmd),
    Shutdown(u16, u16),
    Close(u16),
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
pub struct TCPSendCmd {}

#[derive(Debug, PartialEq, Eq)]
pub struct TCPReadCmd {}

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
            Command::SendTCPPacket(_cmd) => {
                todo!() //TODO implement
            }
            Command::OpenSocket(_port) => {
                todo!()
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
        // TODO fetch socket table from TCP API
        let sockets = "test";
        match file {
            Some(file) => {
                let mut f = File::create(file).unwrap();
                f.write_all(b"id\t\tstate\t\tlocal window size\t\tremote window size\n")
                    .unwrap();
                f.write_all(format!("{}\n", sockets).as_bytes()).unwrap();
            }
            None => {
                println!("id\t\tstate\t\tlocal window size\t\tremote window size\n");
                println!("{}", sockets)
            }
        }
    }

    async fn close_socket(&self, socket_descriptor: u16) {
        if self
            .node
            .close_socket(socket_descriptor.into())
            .await
            .is_err()
        {
            eprintln!(
                "Failed to close socket: socket {} does not exist",
                socket_descriptor
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
            let arg = tokens.next().ok_or(ParseOpenSocketError::NoPort)?;
            let port = arg
                .parse::<u16>()
                .map_err(|_| ParseOpenSocketError::InvalidPort)?;
            Ok(Command::OpenSocket(port))
        }
        "c" => {
            let ip = tokens.next().ok_or(ParseConnectError::NoIp)?;
            let ip = ip.parse().map_err(|_| ParseConnectError::InvalidIp)?;
            let port = tokens.next().ok_or(ParseConnectError::NoPort)?;
            let port: u16 = port.parse().map_err(|_| ParseConnectError::InvalidPort)?;
            Ok(Command::ConnectSocket(ip, port.into()))
        }
        "s" => {
            todo!() //TODO implement
        }
        "r" => {
            todo!() //TODO implement
        }
        "sd" => {
            todo!() //TODO implement
        }
        "cl" => {
            todo!() //TODO implement
        }
        "sf" => {
            todo!() //TODO implement
        }
        "rf" => {
            todo!() //TODO implement
        }
        "q" => Ok(Command::Quit),
        _ => Err(ParseError::Unknown),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseOpenSocketError {
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
pub enum ParseError {
    Unknown,
    Down(ParseDownError),
    Up(ParseUpError),
    Send(ParseSendError),
    OpenSocket(ParseOpenSocketError),
    Connect(ParseConnectError),
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
            ParseError::OpenSocket(e) => {
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

impl From<ParseOpenSocketError> for ParseError {
    fn from(v: ParseOpenSocketError) -> Self {
        ParseError::OpenSocket(v)
    }
}

impl From<ParseConnectError> for ParseError {
    fn from(v: ParseConnectError) -> Self {
        ParseError::Connect(v)
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
}
