use std::sync::Arc;
use std::time::Duration;

use ip::node::NodeBuilder;
use ip::protocol::tcp::TcpHandler;
use ip::Args;
use ip::{cli, protocol::tcp::Tcp};

use cli::Cli;
use ip::protocol::{rip::RipHandler, test::TestHandler, Protocol};

const RIP_UPDATE_INTERVAL: Duration = Duration::from_secs(1);
const ROUTING_ENTRY_MAX_AGE: Duration = Duration::from_secs(2);

#[tokio::main]
async fn main() {
    env_logger::init();

    let args = match Args::try_from(std::env::args()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {:?}", e);
            eprintln!("Usage: ./node <lnx-file>");
            std::process::exit(1);
        }
    };

    let node = Arc::new(
        NodeBuilder::new(&args)
            .with_rip_interval(RIP_UPDATE_INTERVAL)
            .with_entry_max_age(ROUTING_ENTRY_MAX_AGE)
            .with_protocol_handler(Protocol::Rip, RipHandler::default())
            .with_protocol_handler(Protocol::Test, TestHandler::default())
            .build()
            .await,
    );

    let cli_node = node.clone();
    tokio::spawn(async move {
        Cli::new(cli_node).run().await;
    });

    node.run().await;
}
