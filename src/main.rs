mod config;
mod daemon;
mod lru;
mod model;
mod podman;
mod state;

use std::io::IsTerminal;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_ansi(std::io::stdout().is_terminal())
        .init();
    let cfg = config::Config::parse();
    daemon::Daemon::new(cfg).run().await
}
