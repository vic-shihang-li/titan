use crate::route::{bootstrap_interface_table, bootstrap_routing_table};
use rustyline::{error::ReadlineError, Editor};
use std::net::Ipv4Addr;

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
                    if line == "" {
                        continue;
                    }
                    if line == "q" {
                        eprintln!("Commencing Graceful Shutdown");
                        shutdown_flag = true;
                    }
                    match self.parse_command(line) {
                        Some(cmd) => {
                            self.execute_command(cmd);
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
        return match cmd {
            "li" => {
                eprintln!("matched li");
                let arg = tokens.next();
                Some(Command::ListInterface(arg.map(|s| s.to_string())))
            }
            "interfaces" => {
                let arg = tokens.next();
                Some(Command::ListInterface(arg.map(|s| s.to_string())))
            }
            "lr" => {
                let arg = tokens.next();
                Some(Command::ListRoute(arg.map(|s| s.to_string())))
            }
            "routes" => {
                let arg = tokens.next();
                Some(Command::ListRoute(arg.map(|s| s.to_string())))
            }
            "down" => {
                let arg = tokens.next().unwrap();
                let link_no = arg.parse::<u16>().unwrap();
                Some(Command::InterfaceDown(link_no))
            }
            "up" => {
                let arg = tokens.next().unwrap();
                let link_no = arg.parse::<u16>().unwrap();
                Some(Command::InterfaceUp(link_no))
            }
            "send" => {
                let virtual_ip = tokens.next().unwrap();
                let protocol = tokens.next().unwrap();
                let payload = tokens.next().unwrap();
                Some(Command::Send(SendCmd {
                    virtual_ip: virtual_ip.parse().unwrap(),
                    protocol: protocol.parse().unwrap(),
                    payload: payload.to_string(),
                }))
            }
            "q" => Some(Command::Quit),
            _ => None,
        };
    }

    fn execute_command(&self, cmd: Command) {
        match cmd {
            Command::ListInterface(op) => {
                self.print_interfaces(op);
            }
            Command::ListRoute(op) => {
                self.print_routes(op);
            }
            Command::InterfaceDown(interface) => {
                eprintln!("Turning down interface {}", interface);
            }
            Command::InterfaceUp(interface) => {
                eprintln!("Turning up interface {}", interface);
            }
            Command::Send(cmd) => {
                eprintln!(
                    "Sending packet {} with protocol {} to {}",
                    cmd.payload, cmd.protocol, cmd.virtual_ip
                );
            }
            Command::Quit => {
                eprintln!("Quitting");
            }
        }
    }

    fn print_interfaces(&self, file: Option<String>) {
        let it = bootstrap_interface_table();
        match file {
            Some(file) => {
                eprintln!("Writing interface information to file {}", file);
                // TODO: fetch and iterate through interfaces and print, optionally write to file.
            }
            None => {
                println!("id\tstate\tlocal\t\tremote\t        port");
                println!("{}", it);
            }
        }
    }

    fn print_routes(&self, file: Option<String>) {
        let rt = bootstrap_routing_table();
        match file {
            Some(file) => {
                eprintln!("Writing route information to file {}", file);
                // TODO fetch and iterate through routes and print, optionally write to file.
            }
            None => {
                println!("dest\t\tnext\t\tcost");
                println!("{}", rt);
            }
        }
    }
}
