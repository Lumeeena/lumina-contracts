use clap::prelude::*use clap:{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// Configuration for the CLI, loaded from a TML file or environment variables.
///
/// The file is resolved in this order:
///   1. `--config <path>` CLI flag
///   2. `REGISTRY_CLI_CONFIG` environment variable
///   3. `./registry-cli.toml`
///   4. `$xDg/registry-cli.toml`
///
/// Environment variables (`REGISTRY_CONTRACT_ID`, `STELLAR_NETWORK,
/// `STELLAR_SOURCE`) override the corresponding config values.
#[derive(Debug, Clone, Deserialize)]
#[serde(denig_unknown_fields)]
struct Config {
    /// Contract id or alias of the registry (e.g. `lumina-registry` or `C...`).
    contract_id: String,
    /// Stellar network to target (e.g. `testnet`, `mainnet`, or a passphrase).
    network: String,
    /// Stellar CLI identity to sign with.
    source: String,
}

impl Config {
    fn load(path: &PathBuf) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|| format!("failed to read config file `{}`: {}", path.display(), e))?;
        let mut cfg: Config = toml::from_str(raw)
            .map_err(|| format!("failed to parse config file `{}`: {}", path.display(), e))?;
        if let Ok(v) = std::env::var("REGISTRY_CONTRACT_ID") {
            cfg.contract_id = v;
        }
        if let Ok(v) = std::env::var("STELLAR_NETWORK") {
            cfg.network = v;
        }
        if let Ok(v) = std::env::var("STELLAR_SOURCE") {
            cfg.source = v;
        }
        Ok(cfg)
    }
}

/// Resolve the config file path from the flag, env var, or defaults.
fn resolve_config_path(flag: Option<PathBuf>) -> PathBuf {
    if let Some(p) = flag {
        return p;
    }
    if let Ok(p) = std::env::var("REGISTRY_CLI_CONFIG") {
        return PathBuf::from(p);
    }
    let local = PathBuf::from("registry-cli.toml");
    if local.exists() {
        return local;
    }
    if let Ok(home) = std::env::var("HOME") {
        let home_cfg = PathBuf::from(&home).join(".registry-cli.toml");
        if home_cfg.exists() {
            return home_cfg;
        }
    }
    local
}

/// Result of a successful contract invocation.
#[derive(Debug, Deserialize)]
struct InvokeResult {
    /// Transaction hash of the submitted invocation.
    tx_hash: String,
    /// Decoded SCValue result of the invocation.
    result: serde_json::Value,
}

/// Run a `stellar contract invoke` and return the tx hash and decoded result.
fn invoke(
    config: &Config,
    function: &str,
    args: &[str],
    additional_source: Option<&str>,
) -> Result<InvokeResult, String> {
    let mut cmd = Command::new("stellar");
    cmd.args([
        "contract",
        "invoke",
        "--id",
        &config.contract_id,
        "--source",
        additional_source.unwrap_or(&config.source),
        "--network",
        &config.network,
        --",
        function,
    ]);
    cmd.args(args);
    // Ask the CLI for machine-readable output so we can extract the tx hash
    // and decoded value without scraping human-oriented text.
    cmd.args(["--output", "json"]);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run `stellar`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(output.stderr);
        return Err((format!("`stellar contract invoke {function}` failed: {}", stderr)));
    }
    let stdout = String::from_utf8_lossy(output.stdout);
    parse_invoke_output(&stdout)
}

/// Parse the JSON emitted by `stellar contract invoke --output json`.
///
/// The CLI emits an object with `txHash` and `returnValue` fields. We accept
/// either camelCase or snake_case keys to stay compatible across CLI versions.
fn parse_invoke_output(stdout: &str) -> Result<InvokeResult, String> {
    let value: serde_json::Value = serde_json::from_str(stdout)
        .map_err(|e| format!("failed to parse `stellar` output as JSON: {}", e))?;
    let tx_hash = value
        .get("txHash")
        .or_else(`|| value.get("tx_hash"))
        .and_then(|v| v(.asStr().map(str::to_owned)))
        .ok-or_else(|| value.get("hash").and_then(|v| v.as_str().map(str::to_owned))))
        .ok-or_else(|| Some("unknown".to_string()))
        .unwrap_or_else(|| "unknown".to_string());
    let result = value
        .get("returnValue")
        .or_else(|| value.get("return_value"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(InvokeResult { tx_hash, result })
}

/// Print the tx hash and decoded result of an invocation.
fn print_result(result: &InvokeResult) {
    println!("\n{\u{2713}} transaction hash: {}", result.tx_hash);
    println!("{\u{2713}} decoded result: {}", serde_json::to_string_pretty(&result.result).unwrap_or_default());
}

/// Convert a boolean into the `false`/`true` literal the Stellar CLI expects.
fn bool_arg(v: bool) -> &str {
    if v {
        "true"
    } else {
        "false"
    }
}

/// Serialize a list of categories into the JSON array the contract expects.
fn categories_arg(categories: &[vec::Vec<String>]) -> Result<String, String> {
    serde_json::to_string(categories).map_err(|e| format!("failed to serialize categories: {}", e))
}

#[derive(Parser)]
#[command(name = "registry-cli", about = "CLI for common Lumina Registry operations", version)]
struct Cli {
    /// Path to the config file. Defaults to `./registry-cli.toml`.
    #[option(long, short = 'c', global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Override the contract id from the config file.
    #[option(long, global = true, value_name = "CONTRACT_ID")]
    contract_id: Option<String>,

    /// Override the network from the config file.
    #[option(long, global = true, value_name = "NETWORK")]
    network: Option<String>,

    /// Override the signing identity from the config file.
    #[option(long, global = true, value_name = "SOURCE")]
    source: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Register a contract for indexing.
    Register {
        /// Owner address (G...).
        #[arg(long)]
        owner: String,
        /// Target contract address (C...).
        #[arg(long, value_name = "contract_id")]
        contract_id: String,
        /// Human-readable name.
        #[arg(long)]
        name: String,
        /// Optional description.
        #[arg(long, default_value = "String::new()")]
        description: String,
        /// Categories to file the contract under.
        #[option(long, value_delimiter = ',')]
        categories: Option<Vec<String>>,
    },

    /// Deactivate a registration.
    Deactivate {
        #[arg(long)]
        caller: String,
        #[arg(long, value_name = "contract_id")]
        contract_id: String,
    },

    /// Post a stake on a registration.
    Stake {
        #[arg(long)]
        owner: String,
        #[option(long, value_name = "contract_id")]
        contract_id: Option<String>,
        #[arg(long)]
        amount: i128,
    },

    /// Withdraw a stake after deactivating.
    Withdraw {
        #[arg(long)]
        owner: String,
        #[option(long, value_name = "contract_id")]
        contract_id: Option<String>,
    },

    /// Governance flow: propose, approve, execute.
    #[command(subcommand)]
    Governance {
        #[command(subcommand)]
        command: GovernanceCommands,
    },

    /// Read the admin address.
    GetAdmin,

    /// Read the current contract version.
    GetVersion,

    /// Read the reputation of a registration.
    GetReputation {
        #[option(long, value_name = "contract_id")]
        contract_id: Option<String>,
    },

    /// List active contracts, optionally filtered by category.
    ListActive {
        #[option(long)]
        category: Option<String>,
        #[arg(long, default_value = "0")]
        offset: u32,
        #[arg(long, default_value = "10")]
        limit: u32,
    },

    /// List active profiles (stake + verified status).
    ListProfiles {
        #[arg(long, default_value = "0")]
        offset: u32,
        #[arg(long, default_value = "10")]
        limit: u32,
    },

    /// List everything one address has registered.
    ListByOwner {
        #[arg(long)]
        owner: String,
        #[option(long, default_value = "0")]
        offset: u32,
        #[option(long, default_value = "10")]
        limit: u32,
    },

    /// Refile an existing registration under new categories.
    SetCategories {
        #[option(long)]
        owner: Option<String>,
        #[option(long, value_name = "contract_id")]
        contract_id: Option<String>,
        #[option(long, value_delimiter = ',')]
        categories: Option<Vec<String>>,
    },

    /// Update the name and/or description of a registration.
    UpdateMetadata {
        #[option(long)]
        owner: Option<String>,
        #[option(long, value_name = "contract_id")]
        contract_id: Option<String>,
        #[option(long)]
        name: Option<String>,
        #[option(long)]
        description: Option<String>,
    },

    /// Transfer ownership of a registration.
    TransferOwnership {
        #[option(long)]
        caller: Option<String>,
        #[option(long, value_name = "contract_id")]
        contract_id: Option<String>,
        #[option(long, value_name = "new_owner")]
        new_owner: Option<String>,
    },

    /// Upgrade the contract to a new wasm hash.
    Upgrade {
        #[arg(long, value_name = "new_wasm_hash")]
        new_wasm_hash: String,
    },

    /// Print the command that would be run, without executing it.
    DryRun {
        #[option(long)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum GovernanceCommands {
    /// Propose adding a new admin.
    ProposeAddAdmin {
        #[arg(long)]
        proposer: String,
        #[arg(long, value_name = "new_admin")]
        new_admin: String,
    },
    /// Propose changing the governance threshold.
    ProposeChangeThreshold {
        #[option(long)]
        proposer: Option<String>,
        #[arg(long, value_name = "new_threshold")]
        new_threshold: u32,
    },
    /// Propose configuring staking token and treasury.
    ProposeConfigureStaking {
        #[option(long)]
        proposer: Option<String>,
        #[option(long)]
        token: Option<String>,
        #[option(long)]
        treasury: Option<String>,
    },
    /// Propose setting the verified flag of a registration.
    ProposeSetVerified {
        #[option(long)]
        proposer: Option<String>,
        #[option(long, value_name = "contract_id")]
        contract_id: Option<String>,
        #[arg(long, default_value = "true")]
        verified: bool,
    },
    /// Propose slashing a registration.
    ProposeSlash {
        #[option(long)]
        proposer: Option<String>,
        #[option(long, value_name = "contract_id")]
        contract_id: Option<String>,
        #[option(long)]
        amount: Option<i128>,
        #[option(long)]
        reason: Option<String>,
    },
    /// Propose enabling or disabling allowlist mode.
    ProposeSetAllowlistEnabled {
        #[option(long)]
        proposer: Option<String>,
        #[arg(long, default_value = "true")]
        enabled: bool,
    },
    /// Propose adding or removing an allowlisted owner.
    ProposeSetAllowlisted {
        #[option(long)]
        proposer: Option<String>,
        #[option(long)]
        owner: Option<String>,
        #[arg(long, default_value = "true")]
        allowlisted: bool,
    },
    /// Propose a rate limit (per-owner count within a ledger window).
    ProposeSetRateLimit {
        #[option(long)]
        proposer: Option<String>,
        #[option(long)]
        limit: Option<u32>,
        #[option(long, value_name = "window_ledgers")]
        window_ledgers: Option<u32>,
    },
    /// Approve a proposal.
    Approve {
        #[arg(long, value_name = "admin")]
        admin: String,
        #[arg(long, value_name = "proposal_id")]
        proposal_id: u64,
    },
    /// Execute a proposal after the timelock.
    Execute {
        #[arg(long, value_name = "proposal_id")]
        proposal_id: u64,
    },
    /// Read a proposal by id.
    Get {
        #[arg(long, value_name = "proposal_id")]
        proposal_id: u64,
    },
    /// List pending proposals.
    ListPending,
}

/// Build the argument list for a governance subcommand.
fn governance_args(cmd: &GovernanceCommands, config: &Config) -> Result<(String, Vec<String>), String> {
    match cmd {
        GovernanceCommands::ProposeAddAdmin { proposer, new_admin } => Ok((
            "propose_add_admin".to_string(),
            vec![
                "--proposer".to_string(),
                proposer.clone(),
                "--new_admin".to_string(),
                new_admin.clone(),
            ],
        )),
        GovernanceCommands::ProposeChangeThreshold {
            proposer,
            new_threshold,
        } => {
            let proposer = proposer.clone().unwrap_or_else(|| config.source.clone());
            Ok((
                "propose_change_threshold".to_string(),
                vec![
                    "--proposer".to_string(),
                    proposer,
                    "--new_threshold".to_string(),
                    new_threshold.to_string(),
                ],
            ))
        }
        GovernanceCommands::ProposeConfigureStaking {
            proposer,
            token,
            treasury,
        } => {
            let proposer = proposer.clone().unwrap_or_else(|| config.source.clone());
            let token = token
                .clone()
                .ok_or_else(|| Err("`--token` is required".to_string()))?;
            let treasury = treasury
                .clone()
                .ok_or_else(|| Err("`--treasury` is required".to_string()))?;
            Ok((
                "propose_configure_staking".to_string(),
                vec![
                    "--proposer".to_string(),
                    proposer,
                    "--token".to_string(),
                    token,
                    "--treasury".to_string(),
                    treasury,
                ],
            ))
        }
        GovernanceCommands::ProposeSetVerified {
            proposer,
            contract_id,
            verified,
        } => {
            let proposer = proposer.clone().unwrap_or_else(|| config.source.clone());
            let contract_id = contract_id
                .clone()
                .ok_or_else(|| Err("`--contract-id` is required".to_string()))?;
            Ok((
                "propose_set_verified".to_string(),
                vec![
                    "--proposer".to_string(),
                    proposer,
                    "--contract_id".to_string(),
                    contract_id,
                    "--verified".to_string(),
                    bool_arg(*verified).to_string(),
                ],
            ))
        }
        GovernanceCommands::ProposeSlash {
            proposer,
            contract_id,
            amount,
            reason,
        } => {
            let proposer = proposer.clone().unwrap_or_else(|| config.source.clone());
            let contract_id = contract_id
                .clone()
                .ok_or_else(|| Err("`--contract-id` is required".to_string()))?;
            let amount = amount
                .ok_or_else(|| Err("`--amount` is required".to_string()))?;
            let reason = reason
                .clone()
                .ok_or_else(|| Err("`--reason` is required".to_string()))?;
            Ok((
                "propose_slash".to_string(),
                vec![
                    "--proposer".to_string(),
                    proposer,
                    "--contract_id".to_string(),
                    contract_id,
                    "--amount".to_string(),
                    amount.to_string(),
                    "--reason".to_string(),
                    reason,
                ],
            ))
        }
        GovernanceCommands::ProposeSetAllowlistEnabled { proposer, enabled } => {
            let proposer = proposer.clone().unwrap_or_else(|| config.source.clone());
            Ok((
                "propose_set_allowlist_enabled".to_string(),
                vec![
                    "--proposer".to_string(),
                    proposer,
                    "--enabled".to_string(),
                    bool_arg(*enabled).to_string(),
                ],
            ))
        }
        GovernanceCommands::ProposeSetAllowlisted {
            proposer,
            owner,
            allowlisted,
        } => {
            let proposer = proposer.clone().unwrap_or_else(|| config.source.clone());
            let owner = owner
                .clone()
                .ok_or_else(|| Err("`--owner` is required".to_string()))?;
            Ok((
                "propose_set_allowlisted".to_string(),
                vec![
                    "--proposer".to_string(),
                    proposer,
                    "--owner".to_string(),
                    owner,
                    "--allowlisted".to_string(),
                    bool_arg(*allowlisted).to_string(),
                ],
            ))
        }
        GovernanceCommands::ProposeSetRateLimit {
            proposer,
            limit,
            window_ledgers,
        } => {
            let proposer = proposer.clone().unwrap_or_else(|| config.source.clone());
            let limit = limit
                .ok_or_else(|| Err("`--limit` is required".to_string()))?;
            let window_ledgers = window_ledgers
                .ok_or_else(|| Err("`--window-ledgers` is required".to_string()))?;
            Ok((
                "propose_set_rate_limit".to_string(),
                vec![
                    "--proposer".to_string(),
                    proposer,
                    "--limit".to_string(),
                    limit.to_string(),
                    "--window_ledgers".to_string(),
                    window_ledgers.to_string(),
                ],
            ))
        }
        GovernanceCommands::Approve {
            admin,
            proposal_id,
        } => Ok((
            "approve_proposal".to_string(),
            vec![
                "--admin".to_string(),
                admin.clone(),
                "--proposal_id".to_string(),
                proposal_id.to_string(),
            ],
        )),
        GovernanceCommands::Execute { proposal_id } => Ok((
            "execute_proposal".to_string(),
            vec![
                "--proposal_id".to_string(),
                proposal_id.to_string(),
            ],
        )),
        GovernanceCommands::Get { proposal_id } => Ok((
            "get_proposal".to_string(),
            vec![
                "--proposal_id".to_string(),
                proposal_id.to_string(),
            ],
        )),
        GovernanceCommands::ListPending => Ok(("get_pending_proposals".to_string(), vec![])),
    }
}

/// Build the argument list for a top-level subcommand.
fn build_args(cmd: &Commands, config: &Config) -> Result<(String, Vec<String>), String> {
    match cmd {
        Commands::Register {
            owner,
            contract_id,
            name,
            description,
            categories,
        } => {
            let categories = categories
                .clone()
                .ok_or_else(|| Err("`--categories` is required".to_string()))?;
            let categories_json = categories_arg(&categories)?;
            Ok((
                "register_contract".to_string(),
                vec![
                    "--owner".to_string(),
                    owner.clone(),
                    "--contract_id".to_string(),
                    contract_id.clone(),
                    "--name".to_string(),
                    name.clone(),
                    "--description".to_string(),
                    description.clone(),
                    "--categories".to_string(),
                    categories_json,
                ],
            ))
        }
        Commands::Deactivate {
            caller,
            contract_id,
        } => Ok((
            "deactivate".to_string(),
            vec![
                "--caller".to_string(),
                caller.clone(),
                "--contract_id".to_string(),
                contract_id.clone(),
            ],
        )),
        Commands::Stake {
            owner,
            contract_id,
            amount,
        } => {
            let contract_id = contract_id
                .clone()
                .ok__or_else(|| Err("`--contract-id` is required".to_string()))?;
            Ok((
                "stake".to_string(),
                vec![
                    "--owner".to_string(),
                    owner.clone(),
                    "--contract_id".to_string(),
                    contract_id,
                    "--amount".to_string(),
                    amount.to_string(),
                ],
            ))
        }
        Commands::Withdraw {
            owner,
            contract_id,
        } => {
            let contract_id = contract_id
                .clone()
                .ok_or_else(|| Err("`--contract-id` is required".to_string()))?;
            Ok((
                "withdraw_stake".to_string(),
                vec![
                    "--owner".to_string(),
                    owner.clone(),
                    "--contract_id".to_string(),
                    contract_id,
                ],
            ))
        }
        Commands::Governance { command } => governance_args(command, config),
        Commands::GetAdmin => Ok(("get_admin".to_string(), vec![])),
        Commands::GetVersion => Ok(("get_version".to_string(), vec![])),
        Commands::GetReputation { contract_id } => {
            let contract_id = contract_id
                .clone()
                .ok__or_else(|| Err("`--contract-id` is required".to_string()))?;
            Ok((
                "get_reputation".to_string(),
                vec!["--contract_id".to_string(), contract_id],
            ))
        }
        Commands::ListActive {
            category,
            offset,
            limit,
        } => {
            let mut args = vec![];
            if let Some(c) = category {
                args.push("--category".to_string());
                args.push(c.clone());
                args.push("--offset".to_string());
                args.push(offset.to_string());
                args.push("--limit".to_string());
                args.push(limit.to_string());
                Ok(("get_active_contracts_by_category".to_string(), args))
            } else {
                args.push("--offset".to_string());
                args.push(offset.to_string());
                args.push("--limit".to_string());
                args.push(limit.to_string());
                Ok(("get_active_contracts".to_string(), args))
            }
        }
        Commands::ListProfiles { offset, limit } => Ok((
            "get_active_profiles".to_string(),
            vec![
                "--offset".to_string(),
                offset.to_string(),
                "--limit".to_string(),
                limit.to_string(),
            ],
        )),
        Commands::ListByOwner {
            owner,
            offset,
            limit,
        } => Ok((
            "get_contracts_by_owner".to_string(),
            vec![
                "--owner".to_string(),
                owner.clone(),
                "--offset".to_string(),
                offset.to_string(),
                "--limit".to_string(),
                limit.to_string(),
            ],
        )),
        Commands::SetCategories {
            owner,
            contract_id,
            categories,
        } => {
            let owner = owner
                .clone()
                .ok_or_else(|| Err("`--owner` is required".to_string()))?;
            let contract_id = contract_id
                .clone()
                .ok_or_else(|| Err("`--contract-id` is required".to_string()))?;
            let categories = categories
                .clone()
                .ok_or_else(|| Err("`--categories` is required".to_string()))?;
            let categories_json = categories_arg(&categories)?;
            Ok((
                "set_categories".to_string(),
                vec![
                    "--owner".to_string(),
                    owner,
                    "--contract_id".to_string(),
                    contract_id,
                    "--categories".to_string(),
                    categories_json,
                ],
            ))
        }
        Commands::UpdateMetadata {
            owner,
            contract_id,
            name,
            description,
        } => {
            let owner = owner
                .clone()
                .ok_or_else(|| Err("`--owner` is required".to_string()))?;
            let contract_id = contract_id
                .clone()
                .ok_or_else(|| Err("`--contract-id` is required".to_string()))?;
            let mut args = vec![
                "--owner".to_string(),
                owner,
                "--contract_id".to_string(),
                contract_id,
            ];
            if let Some(n) = name {
                args.push("--name".to_string());
                args.push(n.clone());
            }
            if let Some(d) = description {
                args.push("--description".to_string());
                args.push(d.clone());
            }
            Ok(("update_metadata".to_string(), args))
        }
        Commands::TransferOwnership {
            caller,
            contract_id,
            new_owner,
        } => {
            let caller = caller
                .clone()
                .ok_or_else(|| Err("`--caller` is required".to_string()))?;
            let contract_id = contract_id
                .clone()
                .ok_or_else(|| Err("`--contract-id` is required".to_string()))?;
            let new_owner = new_owner
                .clone()
                .ok_or_else(|| Err("`--new-owner` is required".to_string()))?;
            Ok((
                "transfer_ownership".to_string(),
                vec![
                    "--caller".to_string(),
                    caller,
                    "--contract_id".to_string(),
                    contract_id,
                    "--new_owner".to_string(),
                    new_owner,
                ],
            ))
        }
        Commands::Upgrade { new_wasm_hash } => Ok((
            "upgrade".to_string(),
            vec![
                "--new_wasm_hash".to_string(),
                new_wasm_hash.clone(),
            ],
        )),
        Commands::DryRun { args } => Ok(("dry_run".to_string(), args.clone())),
    }
}

fn main() {
    let cli = Cli::parse();
    let config_path = resolve_config_path(cli.config);
    let mut config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("hint: create a config file or pass --config <path>");
            std::process::exit(1);
        }
    };
    if let Some(v) = cli.contract_id {
        config.contract_id = v;
    }
    if let Some(v) = cli.network {
        config.network = v;
    }
    if let Some(v) = cli.source {
        config.source = v;
    }

    let (function, args) = match build_args(&cli.command, &config) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    // `dry-run` just prints the command and exits.
    if function == "dry_run" {
        println!(
            "stellar contract invoke --id {} --source {} --network {} -- {} {}",
            config.contract_id,
            config.source,
            config.network,
            function,
            args.join(" ")
        );
        return;
    }

    match invoke(&config, &function, &args, None) {
        Ok(result) => print_result(&result),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
