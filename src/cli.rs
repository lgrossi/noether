use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::Value;
use tokio::fs;

use crate::contract::DecisionMode;
use crate::error::NoetError;
use crate::fixture::{list_fixture_paths, read_fixture};
use crate::ledger::BudgetLedger;
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
    /// Report persisted decisions, usage, traces, and observations.
    Report(ReportCommand),
}

#[derive(Parser)]
struct ServeArgs {
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1:4040")]
    bind: SocketAddr,

    /// Directory where redacted capture fixtures are written.
    #[arg(long, default_value = ".noet/fixtures")]
    fixture_dir: PathBuf,

    /// SQLite ledger path for durable local state.
    #[arg(long, default_value = ".noet/noether.sqlite")]
    db_path: PathBuf,

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

#[derive(Parser)]
struct ReportCommand {
    /// SQLite ledger path.
    #[arg(long, default_value = ".noet/noether.sqlite")]
    db_path: PathBuf,

    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    json: bool,

    #[command(subcommand)]
    command: ReportSubcommand,
}

#[derive(Subcommand)]
enum ReportSubcommand {
    /// Summarize finalized usage and cost.
    Usage,
    /// List authorization decisions.
    Decisions,
    /// Show one trace story.
    Trace { trace_id: String },
    /// List tool/eval observations.
    Observations {
        #[arg(long)]
        kind: Option<String>,
    },
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
                db_path: args.db_path,
                upstream: args.upstream,
                routes,
                policy,
                decision_mode: args.decision_mode,
            })
            .await
        }
        Command::Policy(command) => run_policy(command).await,
        Command::Fixtures(command) => run_fixtures(command).await,
        Command::Report(command) => run_report(command).await,
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

async fn run_report(command: ReportCommand) -> Result<(), NoetError> {
    let ledger = BudgetLedger::open_sqlite(&command.db_path)?;
    match command.command {
        ReportSubcommand::Usage => {
            let report = ledger.usage_report()?;
            if command.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("total_cost_usd\t{:.6}", report.total_cost_usd);
                println!("project\tprovider\tmodel\tsubject\ttokens\tcost_usd\treservations");
                for row in report.rows {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{:.6}\t{}",
                        row.project.as_deref().unwrap_or("-"),
                        row.provider.as_deref().unwrap_or("-"),
                        row.model.as_deref().unwrap_or("-"),
                        row.subject.as_deref().unwrap_or("-"),
                        row.total_tokens,
                        row.total_cost_usd,
                        row.reservations
                    );
                }
            }
        }
        ReportSubcommand::Decisions => {
            print_items(ledger.decisions_report()?, command.json)?;
        }
        ReportSubcommand::Trace { trace_id } => {
            let report = ledger.trace_report(&trace_id)?;
            if command.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("trace\t{}", report.trace_id);
                for item in report.items {
                    println!(
                        "{}\t{}\t{}",
                        item.occurred_at.to_rfc3339(),
                        item.kind,
                        item.summary
                    );
                }
            }
        }
        ReportSubcommand::Observations { kind } => {
            let prefix = match kind.as_deref() {
                Some("tool") => Some("tool."),
                Some("eval") => Some("eval."),
                Some(value) => Some(value),
                None => None,
            };
            print_items(ledger.observations_report(prefix)?, command.json)?;
        }
    }
    Ok(())
}

fn print_items(items: Vec<crate::ledger::TraceReportItem>, json: bool) -> Result<(), NoetError> {
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        println!("occurred_at\tkind\tsummary");
        for item in items {
            println!(
                "{}\t{}\t{}",
                item.occurred_at.to_rfc3339(),
                item.kind,
                item.summary
            );
        }
    }
    Ok(())
}
