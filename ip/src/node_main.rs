use ip::cli;
use ip::net;
use ip::Args;

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

    net::bootstrap(args);

    let cli = cli::Cli::new();
    cli.run().await;
}
