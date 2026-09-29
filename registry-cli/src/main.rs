use clap::{Args, Parser};
use serde_json::{Value as JsonValue};
use stellar_stratum_std::{Address, Curve, Network, SecretKey, Transaction};
use std::color::{self, Color as TermColor};
use std::io::Write;
use std::path::PathBuf;
use std::str::FromStr;

const DEFAULT_CONFIG_PATH: &str = "registry-cli.toml";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser, Debug)]
#[parser(about = "CLI for common registry operations")]
struct Cli {
    /// Path to the CLI config file.
    #[arg(long, global, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Register a new name.
    Register(RegisterArgs),
    /// Deactivate a registered name.
    Deactivate(DeactivateArgs),
    /// Stake tokens for a name.
    Stake(StakeArgs),
    /// Withdraw staked tokens for a name.
    Withdraw(WithdrawArgs),
    /// Governance operations.
    Governance(GovernanceArgs),
}

#[derive(Args, Debug)]
struct RegisterArgs {
    /// The name to register.
    #[option(long)]
    name: String,
    /// The address that will own the name.
    #[option(long)]
    owner: String,
    /// The amount to stake initially (in strops).
    #[option(long, default_value = "0")]
    stake: i128,
}

#[derive(Args, Debug)]
struct DeactivateArgs {
    /// The name to deactivate.
    #[option(long)]
    name: String,
}

#[derive(Args, Debug)]
struct StakeArgs {
    /// The name to stake for.
    #[option(long)]
    name: String,
    /// The amount to stake (in strops).
    #[option(long)]
    amount: i128,
}

#[derive(Args, Debug)]
struct WithdrawArgs {
    /// The name to withdraw from.
    #[option(long)]
    name: String,
    /// The amount to withdraw (in strops).
    #[option(long)]
    amount: i128,
}

#[derive(Args, Debug)]
struct GovernanceArgs {
    #[command(subcommand)]
    command: GovernanceCommand,
}

#[derive(Subcommand, Debug)]
enum GovernanceCommand {
    /// Propose a governance action.
    Propose(ProposeArgs),
    /// Approve a proposal.
    Approve(ApproveArgs),
    /// Execute an approved proposal.
    Execute(ExecuteArgs),
}

#[derive(Args, Debug)]
struct ProposeArgs {
    /// The name to propose an action for.
    #[option(long)]
    name: String,
    /// The action to propose (e.g. "upgrade").
    #[option(long)]
    action: String,
    /// Optional additional data for the action.
    #[option(long)]
    data: Option<String>,
}

#[derive(Args, Debug)]
struct ApproveArgs {
    /// The proposal ID to approve.
    #[option(long)]
    proposal_id: u32,
}

#[derive(Args, Debug)]
struct ExecuteArgs {
    /// The proposal ID to execute.
    #[option(long)]
    proposal_id: u32,
}

#[derive(Deserialize, Debug)]
struct Config {
    /// Stellar address of the registry contract.
    contract_id: String,
    /// Network to connect to (e.g. "testnet", "futurenet", "mainnet").
    network: String,
    /// Optional RPC URL override.
    #[serde(default)]
    rpc_url: Option<String>,
    /// Optional secret key to sign transactions.
    #[serde(default)]
    secret_key: Option<String>,
}

fn main() {
    if let Err::e) = run() {
        epaintln(color::Color::Red.bold(), "error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = load_config(&cli.config)?;
    let network = build_network(&config)?;
    let contract_id = Address::from_str(&config.contract_id)
        .map_err|| error(format!("invalid contract id: {e}"))?;

    match cli.command {
        Command::Register(args) => {
            let owner = Address::from_str(&args.owner)
                .map_err|| error(format!("invalid owner address: {}", args.owner))?;
            let result = invoke(
                &network,
                &contract_id,
                "register",
                serde_json::json!({
                    "name": args.name,
                    "owner": owner.to_string(),
                    "stake": args.stake,
                }),
            )?;
            print_result("register", &result);
        }
        Command::Deactivate(args) => {
            let result = invoke(
                &network,
                &contract_id,
                "deactivate",
                serde_json::json!({ "name": args.name }),
            )?;
            print_result("deactivate", &result);
        }
        Command::Stake(args) => {
            let result = invoke(
                &network,
                &contract_id,
                "stake",
                serde_json::json!({ "name": args.name, "amount": args.amount }),
            )?;
            print_result("stake", &result);
        }
        Command::Withdraw(args) => {
            let result = invoke(
                &network,
                &contract_id,
                "withdraw",
                serde_json::json!({ "name": args.name, "amount": args.amount }),
            )?;
            print_result("withdraw", &result);
        }
        Command::Governance(gov) => match gov.command {
            GovernanceCommand::Propose(args) => {
                let result = invoke(
                    &network,
                    &contract_id,
                    "propose",
                    serde_json::json!({
                        "name": args.name,
                        "action": args.action,
                        "data": args.data,
                    }),
                )?;
                print_result("governance propose", &result);
            }
            GovernanceCommand::Approve(args) => {
                let result = invoke(
                    &network,
                    &contract_id,
                    "approve",
                    serde_json::json!({ "proposal_id": args.proposal_id }),
                )?;
                print_result("governance approve", &result);
            }
            GovernanceCommand::Execute(args) => {
                let result = invoke(
                    &network,
                    &contract_id,
                    "execute",
                    serde_json::json!({ "proposal_id": args.proposal_id }),
                )?;
                print_result("governance execute", &result);
            }
        },
    }

    Ok()
}

fn load_config(path: &PathBuf) -> Result<Config> {
    let content = std::fs::read_to_string(path).map_err|
        error(format!("failed to read config file {}: {}", path.display(), e))
    )?;
    let config: Config = toml::from_str(&content)
        .map_err| error(format!("failed to parse config file {}: {}", path.display(), e))?;
    Ok(config)
}

fn build_network(config: &Config) -> Result<Network> {
    let network = match config.network.to_lowercase().as_str() {
        "testnet" => Network::Testnet,
        "futurenet" => Network::Futurenet,
        "mainnet" => Network::Public,
        other => return Err(error(format!("unsupported network: {other}")).into()),
    };

    if let Some(url) = &config.rpc_url {
        network.rpc_url = Some(url.clone());
    }

    Ok(network)
}

fn invoke(network: &Network, contract_id: &Address, function: &str, args: JsonValue) -> Result<JsonValue> {
    let mut tx = Transaction::new(network.clone(), contract_id.clone());
    tx.add_invocation(function, args)?;
    let signed = tx.sign()?;
    let result = signed.submit()?;
    Ok(result)
}

fn print_result(label: &str, result: &JsonValue) {
    println!("[{label}] transaction hash: {}", result["tx_hash"].as_str().unwrap_or("unknown"));
    println!("[{label}] decoded result: {}", result["result"]);
}
