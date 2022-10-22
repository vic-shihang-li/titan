use crate::net::{self, get_interfaces};
use crate::protocol::ProtocolPayload;
use crate::route::get_routing_table;
use rustyline::{error::ReadlineError, Editor};
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
    protocol: u16,
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
                        Some(cmd) => {
                            self.execute_command(cmd).await;
                        }
                        None => {
                            eprintln!("Invalid command");
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

    fn parse_command(&self, line: String) -> Option<Command> {
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
                    "Sending packet {} with protocol {} to {}",
                    cmd.payload, cmd.protocol, cmd.virtual_ip
                );
                // TODO: assume test protocol for now
                let payload = ProtocolPayload::Test(cmd.payload);
                if let Err(e) = net::send(payload, cmd.virtual_ip).await {
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
        let rt = get_routing_table().await;
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

fn cmd_arg_handler(cmd: &str, mut tokens: SplitWhitespace) -> Option<Command> {
    match cmd {
        "li" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => Some(Command::ListInterface(Some(arg.to_string()))),
                None => Some(Command::ListInterface(None)),
            }
        }
        "interfaces" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => Some(Command::ListInterface(Some(arg.to_string()))),
                None => Some(Command::ListInterface(None)),
            }
        }
        "lr" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => Some(Command::ListRoute(Some(arg.to_string()))),
                None => Some(Command::ListRoute(None)),
            }
        }
        "routes" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => Some(Command::ListRoute(Some(arg.to_string()))),
                None => Some(Command::ListRoute(None)),
            }
        }
        "down" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => {
                    let link_no = arg.parse::<u16>();
                    match link_no {
                        Ok(link_no) => Some(Command::InterfaceDown(link_no)),
                        Err(_) => None, // TODO replace with error
                    }
                }
                None => None, //TODO replace with error
            }
        }
        "up" => {
            let arg = tokens.next();
            match arg {
                Some(arg) => {
                    let link_no = arg.parse::<u16>();
                    match link_no {
                        Ok(link_no) => Some(Command::InterfaceUp(link_no)),
                        Err(_) => None, // TODO replace with error
                    }
                }
                None => None, //TODO replace with error
            }
        }
        "send" => {
            let virtual_ip = tokens.next();
            let protocol = tokens.next();
            let payload = tokens.next();
            match (virtual_ip, protocol, payload) {
                (Some(virtual_ip), Some(protocol), Some(payload)) => {
                    let virtual_ip = virtual_ip.parse();
                    let protocol = protocol.parse::<u16>();

                    match (virtual_ip, protocol) {
                        (Ok(virtual_ip), Ok(protocol)) => Some(Command::Send(SendCmd {
                            virtual_ip,
                            protocol,
                            payload: payload.to_string(),
                        })),
                        _ => None, // TODO replace with error
                    }
                }
                _ => None, // TODO replace with error
            }
        }
        "q" => Some(Command::Quit),
        _ => None,
    }
}
