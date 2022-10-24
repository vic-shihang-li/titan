use crate::net::{self, get_interfaces};
use crate::protocol::Protocol;
use crate::route;
use crate::route::get_forwarding_table;
use rustyline::{error::ReadlineError, Editor};
use std::fmt::Display;
use std::fs::File;
use std::io::Write;
use std::net::Ipv4Addr;
use std::str::SplitWhitespace;

pub enum Command {
    ListInterface(Option<String>),
    ListRoute(Option<String>),
    InterfaceDown(u16),
    InterfaceUp(u16),
    Send(SendCmd),
    Quit,
}

pub struct SendCmd {
    virtual_ip: Ipv4Addr,
    protocol: Protocol,
    payload: String,
}

pub struct Cli {}

impl Default for Cli {
    fn default() -> Self {
        Self::new()
    }
}

impl Cli {
    pub fn new() -> Self {
        eprintln!("Starting CLI");
        Self {}
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
                    match self.parse_command(line) {
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

    fn parse_command(&self, line: String) -> Result<Command, ParseError> {
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
            Command::InterfaceDown(interface) => {
                eprintln!("Turning down interface {}", interface);
                if let Err(e) = net::deactivate(interface).await {
                    eprintln!("Failed to turn interface {} down: {:?}", interface, e);
                }
            }
            Command::InterfaceUp(interface) => {
                eprintln!("Turning up interface {}", interface);
                if let Err(e) = net::activate(interface).await {
                    eprintln!("Failed to turn interface {} up: {:?}", interface, e);
                }
            }
            Command::Send(cmd) => {
                eprintln!(
                    "Sending packet {} with protocol {:?} to {}",
                    cmd.payload, cmd.protocol, cmd.virtual_ip
                );
                if let Err(e) =
                    route::send(cmd.payload.as_bytes(), cmd.protocol, cmd.virtual_ip).await
                {
                    eprintln!("Failed to send packet: {:?}", e);
                }
            }
            Command::Quit => {
                eprintln!("Quitting");
            }
        }
    }

    async fn print_interfaces(&self, file: Option<String>) {
        let li = get_interfaces().await;
        match file {
            Some(file) => {
                let mut f = File::create(file).unwrap();
                f.write_all(b"id\tstate\tlocal\t\tremote\tport\n").unwrap();
                for x in 0..li.len() {
                    f.write_all(format!("{}\t{}\n", x, li[x]).as_bytes())
                        .unwrap();
                }
            }
            None => {
                println!("id\tstate\tlocal\t\tremote\t        port");
                for x in 0..li.len() {
                    println!("{}\t{}", x, li[x])
                }
            }
        }
    }

    async fn print_routes(&self, file: Option<String>) {
        let rt = get_forwarding_table().await;
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
}

#[derive(Debug)]
pub enum ParseDownError {
    InvalidLinkId,
    NoLinkId,
}

#[derive(Debug)]
pub enum ParseUpError {
    InvalidLinkId,
    NoLinkId,
}

#[derive(Debug)]
pub enum ParseSendError {
    NoIp,
    InvalidIp,
    NoProtocol,
    InvalidProtocol,
    NoPayload,
}

#[derive(Debug)]
pub enum ParseError {
    Unknown,
    Down(ParseDownError),
    Up(ParseUpError),
    Send(ParseSendError),
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
                payload.push_str(" ");
            }

            let virtual_ip = virtual_ip.parse().map_err(|_| ParseSendError::InvalidIp)?;
            let protocol =
                Protocol::try_from(protocol).map_err(|_| ParseSendError::InvalidProtocol)?;
            Ok(Command::Send(SendCmd {
                virtual_ip,
                protocol,
                payload,
            }))
        }
        "q" => Ok(Command::Quit),
        _ => Err(ParseError::Unknown),
    }
}
