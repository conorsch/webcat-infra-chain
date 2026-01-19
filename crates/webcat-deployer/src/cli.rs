//! CLI command definitions for webcat-deployer.

use clap::Parser;
use std::future::Future;

mod create_network;
mod run_network;

/// CLI options for webcat-deployer.
#[derive(Parser)]
#[command(name = "webcat-deployer")]
#[command(about = "Orchestrate felidae and cometbft nodes for integration testing")]
pub enum Options {
    /// Create a new webcat network.
    CreateNetwork(create_network::CreateNetwork),
    /// Run a webcat network from a directory.
    RunNetwork(run_network::RunNetwork),
}

/// Trait for running CLI commands.
pub trait Run {
    fn run(self) -> impl Future<Output = color_eyre::Result<()>> + Send;
}

impl Run for Options {
    async fn run(self) -> color_eyre::Result<()> {
        match self {
            Self::CreateNetwork(cmd) => cmd.run().await,
            Self::RunNetwork(cmd) => cmd.run().await,
        }
    }
}
