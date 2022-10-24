use std::time::Duration;

use ip::cli;
use ip::net;
use ip::route;
use ip::Args;

use ip::protocol::{rip::RipHandler, test::TestHandler, Protocol};
use ip::route::Router;

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

    net::bootstrap(&args).await;

    route::bootstrap(
        route::BootstrapArgs::new(&args)
            .with_rip_interval(RIP_UPDATE_INTERVAL)
            .with_entry_max_age(ROUTING_ENTRY_MAX_AGE),
    )
    .await;

    let mut router = Router::new(&args.get_my_interface_ips());
    router.register_handler(Protocol::Rip, RipHandler::default());
    router.register_handler(Protocol::Test, TestHandler::default());

    tokio::spawn(async move {
        router.run().await;
    });

    let cli = cli::Cli::new();
    cli.run().await;
}
