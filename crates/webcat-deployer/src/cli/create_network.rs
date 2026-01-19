//! Create network command implementation.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use tracing::info;

use super::Run;
use webcat_deployer::{Network, NetworkConfig, Platform};

/// Platform options for the CLI.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliPlatform {
    /// Local deployment (localhost).
    Local,
    /// Docker/container deployment.
    Docker,
    /// Kubernetes deployment.
    Kubernetes,
}

impl From<CliPlatform> for Platform {
    fn from(p: CliPlatform) -> Self {
        match p {
            CliPlatform::Local => Platform::Local,
            CliPlatform::Docker => Platform::Docker,
            CliPlatform::Kubernetes => Platform::Kubernetes,
        }
    }
}

/// Create a new webcat network.
#[derive(Parser)]
pub struct CreateNetwork {
    /// Deployment platform.
    #[arg(long, default_value = "local")]
    pub platform: CliPlatform,

    /// Number of validator nodes.
    #[arg(long, default_value = "1")]
    pub num_validators: usize,

    /// Create sentry nodes for each validator.
    #[arg(long, default_value = "false")]
    pub use_sentries: bool,

    /// Output directory for the network.
    #[arg(long)]
    pub directory: PathBuf,

    /// Chain ID for the network.
    #[arg(long, default_value = "webcat-test")]
    pub chain_id: String,
}

impl Run for CreateNetwork {
    async fn run(self) -> color_eyre::Result<()> {
        info!(
            "Creating network with {} validators in {:?}",
            self.num_validators, self.directory
        );

        let config = NetworkConfig {
            chain_id: self.chain_id,
            num_validators: self.num_validators,
            use_sentries: self.use_sentries,
            platform: self.platform.into(),
            directory: self.directory.clone(),
            ..Default::default()
        };

        let mut network = Network::new(config);

        info!("Initializing {} nodes...", network.nodes.len());
        network.initialize()?;

        info!("Network created successfully!");
        info!("Output directory: {:?}", self.directory);
        info!(
            "Network metadata: {:?}",
            self.directory.join("network.json")
        );

        for node in &network.nodes {
            info!(
                "  {} ({}): P2P={}, RPC={}, ABCI={}",
                node.name,
                node.node_id.as_deref().unwrap_or("unknown"),
                node.ports.cometbft_p2p,
                node.ports.cometbft_rpc,
                node.ports.felidae_abci
            );
        }

        Ok(())
    }
}
