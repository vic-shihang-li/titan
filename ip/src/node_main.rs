use ip::cli;
use ip::net;
use ip::route;
use ip::Args;

use ip::protocol::{rip::RipHandler, test::TestHandler, Protocol};
use ip::route::Router;

#[tokio::main]
async fn main() {
    let args = match Args::try_from(std::env::args()) {
        Ok(a) => {
            eprintln!("Args: {}", a);
            a
        }
        Err(e) => {
            eprintln!("Error: {:?}", e);
            eprintln!("Usage: ./node <lnx-file>");
            std::process::exit(1);
        }
    };

    net::bootstrap(&args).await;
    route::bootstrap(&args).await;

    let mut router = Router::new(&args.get_ip_addrs());
    router.register_handler(Protocol::Rip, RipHandler::default());
    router.register_handler(Protocol::Test, TestHandler::default());

    tokio::spawn(async move {
        router.run().await;
    });

    let cli = cli::Cli::new();
    cli.run().await;
}
