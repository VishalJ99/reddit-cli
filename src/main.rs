use clap::Parser;
use reddit_cli::{Cli, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(Cli::parse()).await
}
