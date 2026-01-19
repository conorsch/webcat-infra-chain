//! Run network command implementation.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use color_eyre::eyre::{Context, Result};
use tracing::{error, info, warn};

use super::Run;
use webcat_deployer::Network;

/// Run a webcat network from a directory.
#[derive(Parser)]
pub struct RunNetwork {
    /// Directory containing the network configuration (network.json).
    #[arg(long)]
    pub directory: PathBuf,

    /// Use process-compose to manage processes (generates config and runs).
    #[arg(long, default_value = "false")]
    pub process_compose: bool,

    /// Generate process-compose.yaml without running (implies --process-compose).
    #[arg(long, default_value = "false")]
    pub generate_only: bool,

    /// Path to the felidae binary.
    #[arg(long, default_value = "felidae")]
    pub felidae_bin: String,

    /// Path to the cometbft binary.
    #[arg(long, default_value = "cometbft")]
    pub cometbft_bin: String,

    /// Run oracle servers for validators.
    #[arg(long, default_value = "false")]
    pub with_oracle: bool,
}

impl Run for RunNetwork {
    async fn run(self) -> Result<()> {
        // Load network configuration
        let network_path = self.directory.join("network.json");
        let network_json = std::fs::read_to_string(&network_path)
            .wrap_err_with(|| format!("failed to read network.json from {:?}", network_path))?;
        let network: Network =
            serde_json::from_str(&network_json).wrap_err("failed to parse network.json")?;

        info!(
            "Loaded network with {} nodes from {:?}",
            network.nodes.len(),
            self.directory
        );

        if self.generate_only || self.process_compose {
            // Generate process-compose.yaml
            let config = generate_process_compose_config(&network, &self)?;
            let config_path = self.directory.join("process-compose.yaml");
            std::fs::write(&config_path, &config)
                .wrap_err_with(|| format!("failed to write {:?}", config_path))?;
            info!("Generated process-compose config at {:?}", config_path);

            if self.generate_only {
                println!(
                    "Process-compose config written to: {}",
                    config_path.display()
                );
                println!("\nTo run with process-compose:");
                println!("  cd {} && process-compose up", self.directory.display());
                return Ok(());
            }

            // Run process-compose
            info!("Starting network with process-compose...");
            let status = Command::new("process-compose")
                .arg("up")
                .current_dir(&self.directory)
                .status()
                .wrap_err("failed to run process-compose")?;

            if !status.success() {
                return Err(color_eyre::eyre::eyre!(
                    "process-compose exited with status: {}",
                    status
                ));
            }
        } else {
            // Run processes directly with prefixed output
            run_processes_directly(&network, &self).await?;
        }

        Ok(())
    }
}

/// Generate process-compose.yaml content.
fn generate_process_compose_config(network: &Network, args: &RunNetwork) -> Result<String> {
    let mut processes = Vec::new();

    for node in &network.nodes {
        // CometBFT process
        let cometbft_name = format!("{}-cometbft", node.name);
        let cometbft_home = node.cometbft_home();
        processes.push(format!(
            r#"  {name}:
    command: "{bin} start --home {home}"
    readiness_probe:
      http_get:
        host: {bind}
        port: {port}
        path: /status
      initial_delay_seconds: 2
      period_seconds: 5"#,
            name = cometbft_name,
            bin = args.cometbft_bin,
            home = cometbft_home.display(),
            bind = node.bind_address,
            port = node.ports.cometbft_rpc,
        ));

        // Felidae process (depends on CometBFT)
        let felidae_name = format!("{}-felidae", node.name);
        let felidae_home = node.felidae_home();
        let query_bind = format!("{}:{}", node.bind_address, node.ports.felidae_query);
        processes.push(format!(
            r#"  {name}:
    command: "{bin} start --abci-bind {abci_bind} --query-bind {query_bind} --homedir {home}"
    depends_on:
      {cometbft_dep}:
        condition: process_healthy"#,
            name = felidae_name,
            bin = args.felidae_bin,
            abci_bind = node.abci_address(),
            query_bind = query_bind,
            home = felidae_home.display(),
            cometbft_dep = cometbft_name,
        ));

        // Oracle server for validators (optional)
        if args.with_oracle && node.role.is_validator() {
            let oracle_name = format!("{}-oracle", node.name);
            let oracle_bind = format!("{}:{}", node.bind_address, node.ports.felidae_oracle);
            processes.push(format!(
                r#"  {name}:
    command: "{bin} oracle server --bind {bind} --node http://{rpc_host}:{rpc_port} --homedir {home}"
    depends_on:
      {felidae_dep}:
        condition: process_started"#,
                name = oracle_name,
                bin = args.felidae_bin,
                bind = oracle_bind,
                rpc_host = node.bind_address,
                rpc_port = node.ports.cometbft_rpc,
                home = felidae_home.display(),
                felidae_dep = felidae_name,
            ));
        }
    }

    let config = format!(
        r#"version: "0.5"

log_location: ./logs
log_level: info

processes:
{processes}
"#,
        processes = processes.join("\n\n")
    );

    Ok(config)
}

/// Run all processes directly with prefixed log output.
async fn run_processes_directly(network: &Network, args: &RunNetwork) -> Result<()> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    // Set up Ctrl+C handler
    ctrlc::set_handler(move || {
        warn!("Received Ctrl+C, shutting down...");
        shutdown_clone.store(true, Ordering::SeqCst);
    })
    .wrap_err("failed to set Ctrl+C handler")?;

    let mut children: HashMap<String, Child> = HashMap::new();
    let mut handles = Vec::new();

    // Start all processes
    for node in &network.nodes {
        // Start CometBFT
        let cometbft_name = format!("{}-cometbft", node.name);
        let child = start_process_with_prefix(
            &cometbft_name,
            &args.cometbft_bin,
            &["start", "--home", &node.cometbft_home().to_string_lossy()],
            shutdown.clone(),
        )?;
        if let Some((c, h)) = child {
            children.insert(cometbft_name, c);
            handles.push(h);
        }

        // Start Felidae
        let felidae_name = format!("{}-felidae", node.name);
        let child = start_process_with_prefix(
            &felidae_name,
            &args.felidae_bin,
            &[
                "start",
                "--abci-bind",
                &node.abci_address(),
                "--query-bind",
                &format!("{}:{}", node.bind_address, node.ports.felidae_query),
                "--homedir",
                &node.felidae_home().to_string_lossy(),
            ],
            shutdown.clone(),
        )?;
        if let Some((c, h)) = child {
            children.insert(felidae_name, c);
            handles.push(h);
        }

        // Start Oracle server for validators (optional)
        if args.with_oracle && node.role.is_validator() {
            let oracle_name = format!("{}-oracle", node.name);
            let child = start_process_with_prefix(
                &oracle_name,
                &args.felidae_bin,
                &[
                    "oracle",
                    "server",
                    "--bind",
                    &format!("{}:{}", node.bind_address, node.ports.felidae_oracle),
                    "--node",
                    &format!("http://{}:{}", node.bind_address, node.ports.cometbft_rpc),
                    "--homedir",
                    &node.felidae_home().to_string_lossy(),
                ],
                shutdown.clone(),
            )?;
            if let Some((c, h)) = child {
                children.insert(oracle_name, c);
                handles.push(h);
            }
        }
    }

    info!("Started {} processes", children.len());
    print_node_info(network);

    // Wait for shutdown signal
    while !shutdown.load(Ordering::SeqCst) {
        // Check if any process has exited
        let mut exited = Vec::new();
        for (name, child) in children.iter_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        info!("{} exited successfully", name);
                    } else {
                        error!("{} exited with status: {}", name, status);
                    }
                    exited.push(name.clone());
                }
                Ok(None) => {} // Still running
                Err(e) => {
                    error!("Error checking {} status: {}", name, e);
                }
            }
        }

        // If any critical process exited, shut down everything
        if !exited.is_empty() {
            warn!("Processes exited: {:?}, initiating shutdown", exited);
            shutdown.store(true, Ordering::SeqCst);
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Kill all remaining processes
    info!("Terminating all processes...");
    for (name, mut child) in children {
        if let Err(e) = child.kill() {
            // Process may have already exited
            if e.kind() != std::io::ErrorKind::InvalidInput {
                warn!("Failed to kill {}: {}", name, e);
            }
        }
        let _ = child.wait();
    }

    // Wait for output threads to finish
    for handle in handles {
        let _ = handle.join();
    }

    info!("All processes terminated");
    Ok(())
}

/// Start a process and spawn a thread to prefix its output.
fn start_process_with_prefix(
    name: &str,
    bin: &str,
    args: &[&str],
    shutdown: Arc<AtomicBool>,
) -> Result<Option<(Child, std::thread::JoinHandle<()>)>> {
    info!("Starting {}: {} {}", name, bin, args.join(" "));

    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err_with(|| format!("failed to start {}", name))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let name_owned = name.to_string();

    let handle = std::thread::spawn(move || {
        let mut handles = Vec::new();

        if let Some(stdout) = stdout {
            let name = name_owned.clone();
            let shutdown = shutdown.clone();
            handles.push(std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(line) = line {
                        println!("[{}] {}", name, line);
                    }
                }
            }));
        }

        if let Some(stderr) = stderr {
            let name = name_owned;
            handles.push(std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(line) = line {
                        eprintln!("[{}] {}", name, line);
                    }
                }
            }));
        }

        for handle in handles {
            let _ = handle.join();
        }
    });

    Ok(Some((child, handle)))
}

/// Print information about all nodes in the network.
fn print_node_info(network: &Network) {
    println!("\n=== Network Nodes ===");
    for node in &network.nodes {
        println!(
            "{} ({})",
            node.name,
            match node.role {
                webcat_deployer::NodeRole::Validator => "validator",
                webcat_deployer::NodeRole::Sentry => "sentry",
                webcat_deployer::NodeRole::FullNode => "full node",
            }
        );
        println!(
            "  CometBFT P2P:  {}:{}",
            node.bind_address, node.ports.cometbft_p2p
        );
        println!(
            "  CometBFT RPC:  {}:{}",
            node.bind_address, node.ports.cometbft_rpc
        );
        println!("  Felidae ABCI:  {}", node.abci_address());
        println!(
            "  Felidae Query: {}:{}",
            node.bind_address, node.ports.felidae_query
        );
        if node.role.is_validator() {
            println!(
                "  Oracle:        {}:{}",
                node.bind_address, node.ports.felidae_oracle
            );
        }
    }
    println!("=====================\n");
}
