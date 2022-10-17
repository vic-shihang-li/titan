use std::net::Ipv4Addr;
use rustyline::{Editor, error::ReadlineError, Result};

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
        loop {
            let readline = rl.readline(">> ");
            match readline {
                Ok(line) => {
                    rl.add_history_entry(line.as_str());
                    eprintln!("Line: {}", line);
                    if line == "" {
                        continue;
                    }
                    let cmd = self.parse_command(line);
                    self.execute_command(cmd).await;
                }
                Err(ReadlineError::Interrupted) => {
                    println!("CTRL-C");
                    break;
                }
                Err(ReadlineError::Eof) => {
                    println!("CTRL-D");
                    break;
                }
                Err(err) => {
                    println!("Error: {:?}", err);
                    break;
                }
            }
        }
    }

    fn parse_command(&self, line: String) -> Command {
        eprintln!("Parsing command: {}", line);
        return Command::InterfaceDown(1u16);
    }

    async fn execute_command(&self, cmd: Command)  {
        match cmd {
            Command::ListInterface(None) => {
                eprintln!("Listing all interfaces");
            }
            Command::ListInterface(Some(interface)) => {
                eprintln!("Listing interface {}", interface);
            }
            Command::ListRoute(None) => {
                eprintln!("Listing all routes");
            }
            Command::ListRoute(Some(route)) => {
                eprintln!("Listing route {}", route);
            }
            Command::InterfaceDown(interface) => {
                eprintln!("Turning down interface {}", interface);
            }
            Command::InterfaceUp(interface) => {
                eprintln!("Turning up interface {}", interface);
            }
            Command::Send(cmd) => {
                eprintln!("Sending packet {} to {}", cmd.payload, cmd.virtual_ip);
            }
            Command::Quit => {
                eprintln!("Quitting");
            }
        }
    }
}