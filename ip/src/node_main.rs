mod cli;

#[tokio::main]
async fn main() {
    let cli = cli::Cli::new();
    cli.run().await;
}
