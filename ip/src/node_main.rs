mod cli;
mod route;
mod net;

#[tokio::main]
async fn main() {
    let cli = cli::Cli::new();
    cli.run().await;
}
