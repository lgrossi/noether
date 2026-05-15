use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::Value;
use tokio::fs;

use crate::contract::DecisionMode;
use crate::error::NoetError;
use crate::fixture::{list_fixture_paths, read_fixture};
use crate::policy::load_policy;
use crate::proxy::load_proxy_routes;
use crate::redaction::redaction_findings;
use crate::server::{ServeConfig, serve};

#[derive(Parser)]
#[command(name = "noet")]
#[command(about = "Noether control sidecar tooling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the local capture and decision server.
    Serve(ServeArgs),
    /// Validate and inspect policy files.
    Policy(PolicyCommand),
    /// Inspect captured fixture files.
    Fixtures(FixturesCommand),
}

#[derive(Parser)]
struct ServeArgs {
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1:4040")]
    bind: SocketAddr,

    /// Directory where redacted capture fixtures are written.
    #[arg(long, default_value = ".noet/fixtures")]
    fixture_dir: PathBuf,

    /// Optional upstream base URL. When omitted, Noether returns mock responses.
    #[arg(long)]
    upstream: Option<url::Url>,

    /// Optional transparent proxy route config YAML.
    #[arg(long)]
    routes: Option<PathBuf>,

    /// Optional policy.noet.yaml file for decisions and capture enforcement.
    #[arg(long)]
    policy: Option<PathBuf>,

    /// Decision mode for capture proxy requests when a policy is configured.
    #[arg(long, value_enum, default_value_t = DecisionMode::DryRun)]
    decision_mode: DecisionMode,
}

#[derive(Parser)]
struct PolicyCommand {
    #[command(subcommand)]
    command: PolicySubcommand,
}

#[derive(Subcommand)]
enum PolicySubcommand {
    /// Parse and validate a policy.noet.yaml file.
    Check { path: PathBuf },
}

#[derive(Parser)]
struct FixturesCommand {
    #[command(subcommand)]
    command: FixturesSubcommand,
}

#[derive(Subcommand)]
enum FixturesSubcommand {
    /// List fixture JSON files in a directory.
    List {
        #[arg(default_value = ".noet/fixtures")]
        dir: PathBuf,
    },
    /// Pretty-print a fixture JSON file.
    Show { path: PathBuf },
    /// Fail if a fixture contains unredacted credential-like JSON keys.
    RedactCheck { path: PathBuf },
}

pub async fn run() -> Result<(), NoetError> {
    let cli = Cli::parse();

    match cli.command {
        Command::Serve(args) => {
            let policy = match args.policy {
                Some(path) => Some(load_policy(&path).await?),
                None => None,
            };
            let routes = match args.routes {
                Some(path) => load_proxy_routes(&path).await?.routes,
                None => Vec::new(),
            };
            serve(ServeConfig {
                bind: args.bind,
                fixture_dir: args.fixture_dir,
                upstream: args.upstream,
                routes,
                policy,
                decision_mode: args.decision_mode,
            })
            .await
        }
        Command::Policy(command) => run_policy(command).await,
        Command::Fixtures(command) => run_fixtures(command).await,
    }
}

async fn run_policy(command: PolicyCommand) -> Result<(), NoetError> {
    match command.command {
        PolicySubcommand::Check { path } => {
            let policy = load_policy(&path).await?;
            println!(
                "policy ok: version={}, budgets={}, policies={}",
                policy.version,
                policy.budgets.len(),
                policy.policies.len()
            );
            Ok(())
        }
    }
}

async fn run_fixtures(command: FixturesCommand) -> Result<(), NoetError> {
    match command.command {
        FixturesSubcommand::List { dir } => {
            for path in list_fixture_paths(&dir).await? {
                println!("{}", path.display());
            }
            Ok(())
        }
        FixturesSubcommand::Show { path } => {
            let fixture = read_fixture(&path).await?;
            println!("{}", serde_json::to_string_pretty(&fixture)?);
            Ok(())
        }
        FixturesSubcommand::RedactCheck { path } => {
            let bytes = fs::read(&path).await?;
            let value: Value = serde_json::from_slice(&bytes)?;
            let findings = redaction_findings(&value);
            if findings.is_empty() {
                println!("redaction ok: {}", path.display());
                Ok(())
            } else {
                Err(NoetError::InvalidPolicy(format!(
                    "unredacted credential-like keys in {}: {}",
                    path.display(),
                    findings.join(", ")
                )))
            }
        }
    }
}
