//! Network configuration and management for webcat deployments.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use color_eyre::eyre::{Result, WrapErr};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::node::{NodeRole, WebcatNode};
use crate::ports::PortAllocationStrategy;

/// The deployment platform for the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Local deployment (localhost).
    #[default]
    Local,
    /// Docker/container deployment.
    Docker,
    /// Kubernetes deployment.
    Kubernetes,
}

/// Configuration for creating a network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// The chain ID for the network.
    pub chain_id: String,
    /// Number of validator nodes.
    pub num_validators: usize,
    /// Whether to create sentry nodes for validators.
    pub use_sentries: bool,
    /// The deployment platform.
    pub platform: Platform,
    /// The base directory for the network.
    pub directory: PathBuf,
    /// Port allocation strategy.
    #[serde(default)]
    pub port_strategy: PortAllocationStrategy,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            chain_id: "webcat-test".to_string(),
            num_validators: 1,
            use_sentries: false,
            platform: Platform::Local,
            directory: PathBuf::from("/tmp/webcat-network"),
            port_strategy: PortAllocationStrategy::default(),
        }
    }
}

/// A webcat network consisting of multiple nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    /// The network configuration.
    pub config: NetworkConfig,
    /// The nodes in this network.
    pub nodes: Vec<WebcatNode>,
}

impl Network {
    /// Create a new network from configuration.
    pub fn new(config: NetworkConfig) -> Self {
        let mut nodes = Vec::new();
        let mut node_index = 0;

        // Create validator nodes
        for i in 0..config.num_validators {
            let name = format!("validator-{}", i);
            let ports = config.port_strategy.allocate(node_index);
            let home_dir = config.directory.join(&name);
            nodes.push(WebcatNode::new(name, NodeRole::Validator, ports, home_dir));
            node_index += 1;
        }

        // Create sentry nodes if requested
        if config.use_sentries {
            for i in 0..config.num_validators {
                let name = format!("sentry-{}", i);
                let ports = config.port_strategy.allocate(node_index);
                let home_dir = config.directory.join(&name);
                nodes.push(WebcatNode::new(name, NodeRole::Sentry, ports, home_dir));
                node_index += 1;
            }
        }

        Self { config, nodes }
    }

    /// Get all validator nodes.
    pub fn validators(&self) -> impl Iterator<Item = &WebcatNode> {
        self.nodes.iter().filter(|n| n.role == NodeRole::Validator)
    }

    /// Get all sentry nodes.
    pub fn sentries(&self) -> impl Iterator<Item = &WebcatNode> {
        self.nodes.iter().filter(|n| n.role == NodeRole::Sentry)
    }

    /// Get all full nodes.
    pub fn full_nodes(&self) -> impl Iterator<Item = &WebcatNode> {
        self.nodes.iter().filter(|n| n.role == NodeRole::FullNode)
    }

    /// Initialize the network by creating all necessary directories and files.
    pub fn initialize(&mut self) -> Result<()> {
        // Create base directory
        fs::create_dir_all(&self.config.directory)
            .wrap_err_with(|| format!("failed to create directory: {:?}", self.config.directory))?;

        // Initialize each node
        for i in 0..self.nodes.len() {
            initialize_node(&mut self.nodes[i])?;
        }

        // Generate shared genesis
        let genesis = self.generate_genesis()?;
        let genesis_path = self.config.directory.join("genesis.json");
        let mut file = fs::File::create(&genesis_path)
            .wrap_err_with(|| format!("failed to create genesis file: {:?}", genesis_path))?;
        file.write_all(genesis.as_bytes())?;

        // Copy genesis to each node
        for node in &self.nodes {
            let node_genesis = node.genesis_path();
            fs::copy(&genesis_path, &node_genesis)
                .wrap_err_with(|| format!("failed to copy genesis to {:?}", node_genesis))?;
        }

        // Generate config.toml for each node with persistent_peers
        self.generate_configs()?;

        // Save network metadata
        let network_json = serde_json::to_string_pretty(&self)?;
        let network_path = self.config.directory.join("network.json");
        let mut file = fs::File::create(&network_path)
            .wrap_err_with(|| format!("failed to create network.json: {:?}", network_path))?;
        file.write_all(network_json.as_bytes())?;

        Ok(())
    }

    fn generate_genesis(&self) -> Result<String> {
        let validators: Vec<_> = self
            .nodes
            .iter()
            .filter(|n| n.role.is_validator())
            .collect();

        let mut validator_entries = Vec::new();
        for (i, node) in validators.iter().enumerate() {
            // Read the validator's public key from priv_validator_key.json
            let priv_val_key_path = node.priv_validator_key_path();
            let priv_val_key_content = fs::read_to_string(&priv_val_key_path)
                .wrap_err_with(|| format!("failed to read {:?}", priv_val_key_path))?;
            let priv_val_key: serde_json::Value = serde_json::from_str(&priv_val_key_content)?;

            let pub_key = &priv_val_key["pub_key"];

            validator_entries.push(serde_json::json!({
                "address": priv_val_key["address"],
                "pub_key": pub_key,
                "power": "10",
                "name": format!("validator-{}", i)
            }));
        }

        let genesis = serde_json::json!({
            "genesis_time": "2024-01-01T00:00:00.000000000Z",
            "chain_id": self.config.chain_id,
            "initial_height": "1",
            "consensus_params": {
                "block": {
                    "max_bytes": "22020096",
                    "max_gas": "-1",
                    "time_iota_ms": "1000"
                },
                "evidence": {
                    "max_age_num_blocks": "100000",
                    "max_age_duration": "172800000000000",
                    "max_bytes": "1048576"
                },
                "validator": {
                    "pub_key_types": ["ed25519"]
                },
                "version": {}
            },
            "validators": validator_entries,
            "app_hash": ""
        });

        Ok(serde_json::to_string_pretty(&genesis)?)
    }

    fn generate_configs(&self) -> Result<()> {
        // Build persistent_peers string
        let persistent_peers: Vec<String> = self
            .nodes
            .iter()
            .filter_map(|n| n.persistent_peer_address())
            .collect();

        for node in &self.nodes {
            // Generate config.toml for this node
            // Filter out this node from persistent_peers
            let peers: Vec<_> = persistent_peers
                .iter()
                .filter(|p| {
                    if let Some(ref id) = node.node_id {
                        !p.starts_with(id)
                    } else {
                        true
                    }
                })
                .cloned()
                .collect();

            let config = generate_config_toml(node, &peers.join(","))?;
            let mut file = fs::File::create(node.config_toml_path())?;
            file.write_all(config.as_bytes())?;
        }

        Ok(())
    }
}

/// Initialize a single node by creating directories and generating keys.
fn initialize_node(node: &mut WebcatNode) -> Result<()> {
    // Create directories
    fs::create_dir_all(node.cometbft_config_dir())?;
    fs::create_dir_all(node.cometbft_data_dir())?;
    fs::create_dir_all(node.felidae_home())?;

    // Generate node key and get node ID
    let (node_key_json, node_id) = generate_node_key()?;
    node.node_id = Some(node_id);

    let mut file = fs::File::create(node.node_key_path())?;
    file.write_all(node_key_json.as_bytes())?;

    // For validators, generate priv_validator_key
    if node.role.is_validator() {
        let priv_validator_key = generate_priv_validator_key()?;
        let mut file = fs::File::create(node.priv_validator_key_path())?;
        file.write_all(priv_validator_key.as_bytes())?;

        // Initialize priv_validator_state
        let priv_validator_state = r#"{
  "height": "0",
  "round": 0,
  "step": 0
}"#;
        let mut file = fs::File::create(node.priv_validator_state_path())?;
        file.write_all(priv_validator_state.as_bytes())?;

        // Generate felidae keys
        generate_felidae_keys(node)?;
    }

    Ok(())
}

/// Generate a CometBFT node_key.json and return (json_content, node_id).
fn generate_node_key() -> Result<(String, String)> {
    let secret_bytes: [u8; 32] = rand::random();
    let signing_key = SigningKey::from_bytes(&secret_bytes);
    let verifying_key = signing_key.verifying_key();

    // Node ID is the first 20 bytes of SHA256(pubkey), hex-encoded
    let mut hasher = Sha256::new();
    hasher.update(verifying_key.as_bytes());
    let hash = hasher.finalize();
    let node_id = hex::encode(&hash[..20]);

    // CometBFT uses a specific JSON format with amino encoding
    let priv_key_bytes = signing_key.to_bytes();
    let pub_key_bytes = verifying_key.to_bytes();

    // Combine private and public key bytes (ed25519 convention)
    let mut full_key = Vec::with_capacity(64);
    full_key.extend_from_slice(&priv_key_bytes);
    full_key.extend_from_slice(&pub_key_bytes);

    let node_key = serde_json::json!({
        "priv_key": {
            "type": "tendermint/PrivKeyEd25519",
            "value": base64_encode(&full_key)
        }
    });

    Ok((serde_json::to_string_pretty(&node_key)?, node_id))
}

/// Generate a CometBFT priv_validator_key.json.
fn generate_priv_validator_key() -> Result<String> {
    let secret_bytes: [u8; 32] = rand::random();
    let signing_key = SigningKey::from_bytes(&secret_bytes);
    let verifying_key = signing_key.verifying_key();

    // Address is the first 20 bytes of SHA256(pubkey), hex-encoded uppercase
    let mut hasher = Sha256::new();
    hasher.update(verifying_key.as_bytes());
    let hash = hasher.finalize();
    let address = hex::encode_upper(&hash[..20]);

    let priv_key_bytes = signing_key.to_bytes();
    let pub_key_bytes = verifying_key.to_bytes();

    let mut full_key = Vec::with_capacity(64);
    full_key.extend_from_slice(&priv_key_bytes);
    full_key.extend_from_slice(&pub_key_bytes);

    let priv_validator_key = serde_json::json!({
        "address": address,
        "pub_key": {
            "type": "tendermint/PubKeyEd25519",
            "value": base64_encode(&pub_key_bytes)
        },
        "priv_key": {
            "type": "tendermint/PrivKeyEd25519",
            "value": base64_encode(&full_key)
        }
    });

    Ok(serde_json::to_string_pretty(&priv_validator_key)?)
}

/// Generate felidae admin and oracle keys.
fn generate_felidae_keys(node: &WebcatNode) -> Result<()> {
    // Generate random keys and save as hex
    let admin_key: [u8; 32] = rand::random();
    let oracle_key: [u8; 32] = rand::random();

    let mut file = fs::File::create(node.admin_key_path())?;
    file.write_all(hex::encode(admin_key).as_bytes())?;

    let mut file = fs::File::create(node.oracle_key_path())?;
    file.write_all(hex::encode(oracle_key).as_bytes())?;

    Ok(())
}

/// Generate config.toml for a node.
fn generate_config_toml(node: &WebcatNode, persistent_peers: &str) -> Result<String> {
    let config = format!(
        r#"# This is a TOML config file.
# For more information, see https://github.com/toml-lang/toml

proxy_app = "tcp://{abci_address}"
moniker = "{moniker}"
fast_sync = true
db_backend = "goleveldb"
db_dir = "data"
log_level = "info"
log_format = "plain"
genesis_file = "config/genesis.json"
priv_validator_key_file = "config/priv_validator_key.json"
priv_validator_state_file = "data/priv_validator_state.json"
priv_validator_laddr = ""
node_key_file = "config/node_key.json"
abci = "socket"
filter_peers = false

[rpc]
laddr = "{rpc_address}"
cors_allowed_origins = []
cors_allowed_methods = ["HEAD", "GET", "POST"]
cors_allowed_headers = ["Origin", "Accept", "Content-Type", "X-Requested-With", "X-Server-Time"]
grpc_laddr = ""
grpc_max_open_connections = 900
unsafe = false
max_open_connections = 900
max_subscription_clients = 100
max_subscriptions_per_client = 5
experimental_subscription_buffer_size = 200
experimental_websocket_write_buffer_size = 200
experimental_close_on_slow_client = false
timeout_broadcast_tx_commit = "10s"
max_body_bytes = 1000000
max_header_bytes = 1048576
tls_cert_file = ""
tls_key_file = ""
pprof_laddr = ""

[p2p]
laddr = "{p2p_address}"
external_address = ""
seeds = ""
persistent_peers = "{persistent_peers}"
upnp = false
addr_book_file = "config/addrbook.json"
addr_book_strict = false
max_num_inbound_peers = 40
max_num_outbound_peers = 10
unconditional_peer_ids = ""
persistent_peers_max_dial_period = "0s"
flush_throttle_timeout = "100ms"
max_packet_msg_payload_size = 1024
send_rate = 5120000
recv_rate = 5120000
pex = true
seed_mode = false
private_peer_ids = ""
allow_duplicate_ip = true
handshake_timeout = "20s"
dial_timeout = "3s"

[mempool]
version = "v0"
recheck = true
broadcast = true
wal_dir = ""
size = 5000
max_txs_bytes = 1073741824
cache_size = 10000
keep-invalid-txs-in-cache = false
max_tx_bytes = 1048576
max_batch_bytes = 0

[statesync]
enable = false
rpc_servers = ""
trust_height = 0
trust_hash = ""
trust_period = "168h0m0s"
discovery_time = "15s"
temp_dir = ""
chunk_request_timeout = "10s"
chunk_fetchers = "4"

[consensus]
wal_file = "data/cs.wal/wal"
timeout_propose = "3s"
timeout_propose_delta = "500ms"
timeout_prevote = "1s"
timeout_prevote_delta = "500ms"
timeout_precommit = "1s"
timeout_precommit_delta = "500ms"
timeout_commit = "1s"
double_sign_check_height = 0
skip_timeout_commit = false
create_empty_blocks = true
create_empty_blocks_interval = "0s"
peer_gossip_sleep_duration = "100ms"
peer_query_maj23_sleep_duration = "2s"

[storage]
discard_abci_responses = false

[tx_index]
indexer = "kv"
psql-conn = ""

[instrumentation]
prometheus = false
prometheus_listen_addr = ":26660"
max_open_connections = 3
namespace = "cometbft"
"#,
        abci_address = node.abci_address(),
        moniker = node.name,
        rpc_address = node.rpc_listen_address(),
        p2p_address = node.p2p_listen_address(),
        persistent_peers = persistent_peers,
    );

    Ok(config)
}

fn base64_encode(data: &[u8]) -> String {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut encoder =
            base64::write::EncoderWriter::new(&mut buf, &base64::engine::general_purpose::STANDARD);
        encoder.write_all(data).unwrap();
    }
    String::from_utf8(buf).unwrap()
}
