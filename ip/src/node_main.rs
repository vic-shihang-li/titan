mod cli;
mod route;
mod net;

use ip::Args;

#[tokio::main]
async fn main() {
    let _args = match Args::try_from(std::env::args()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {:?}", e);
            eprintln!("Usage: ./node <lnx-file>");
            std::process::exit(1);
        }
    };

    let cli = cli::Cli::new();
    cli.run().await;
}
