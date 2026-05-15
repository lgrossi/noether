use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;
use tokio::fs;
use tokio::process::Command as TokioCommand;

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
    /// Launch Pi with the Noether authorization extension injected.
    Pi(PiArgs),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PiFailureMode {
    /// Continue Pi if Noether is unavailable.
    FailOpen,
    /// Abort provider sends if Noether is unavailable.
    FailClosed,
}

impl PiFailureMode {
    fn as_env(self) -> &'static str {
        match self {
            Self::FailOpen => "fail_open",
            Self::FailClosed => "fail_closed",
        }
    }
}

#[derive(Parser)]
#[command(trailing_var_arg = true)]
struct PiArgs {
    /// Noether sidecar URL. Defaults to NOET_URL or http://127.0.0.1:4040.
    #[arg(long)]
    noether_url: Option<url::Url>,

    /// Policy project metadata sent to Noether.
    #[arg(long)]
    project: Option<String>,

    /// Policy subject metadata sent to Noether.
    #[arg(long)]
    subject: Option<String>,

    /// Behavior when Noether cannot be reached.
    #[arg(long, value_enum, default_value_t = PiFailureMode::FailOpen)]
    fail_mode: PiFailureMode,

    /// Pi executable to launch.
    #[arg(long, default_value = "pi")]
    pi_bin: PathBuf,

    /// Noether Pi extension path.
    #[arg(long)]
    extension_path: Option<PathBuf>,

    /// Isolated Pi session directory for this wrapped run.
    #[arg(long, default_value = ".noet/pi-sessions")]
    session_dir: PathBuf,

    /// Optional isolated Pi agent config directory. Omit to read the user's normal Pi config.
    #[arg(long)]
    agent_dir: Option<PathBuf>,

    /// Skip pre-launch Noether /health check.
    #[arg(long)]
    no_health_check: bool,

    /// Disable auto-discovered Pi extensions and load only the Noether extension plus explicit Pi args.
    #[arg(long)]
    no_discovered_extensions: bool,

    /// Arguments passed through to Pi. Use `--` before Pi args when a flag conflicts with wrapper flags.
    #[arg(num_args = 0.., allow_hyphen_values = true)]
    pi_args: Vec<String>,
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
        Command::Pi(args) => run_pi(args).await,
        Command::Policy(command) => run_policy(command).await,
        Command::Fixtures(command) => run_fixtures(command).await,
    }
}

async fn run_pi(args: PiArgs) -> Result<(), NoetError> {
    let noether_url = resolve_noether_url(args.noether_url)?;
    let extension_path = args
        .extension_path
        .unwrap_or_else(default_pi_extension_path);
    if !extension_path.is_file() {
        return Err(NoetError::InvalidConfig(format!(
            "Noether Pi extension does not exist: {}",
            extension_path.display()
        )));
    }

    if !args.no_health_check {
        match check_noether_health(&noether_url).await {
            Ok(()) => {}
            Err(err) if args.fail_mode == PiFailureMode::FailOpen => {
                eprintln!(
                    "warning: Noether health check failed ({err}); launching Pi because --fail-mode=fail-open"
                );
            }
            Err(err) => return Err(err),
        }
    }

    fs::create_dir_all(&args.session_dir).await?;
    if let Some(agent_dir) = &args.agent_dir {
        fs::create_dir_all(agent_dir).await?;
    }

    let mut command = TokioCommand::new(&args.pi_bin);
    command
        .arg("--session-dir")
        .arg(&args.session_dir)
        .env("NOET_URL", noether_url.as_str())
        .env("NOET_PI_FAIL_MODE", args.fail_mode.as_env())
        .env("NOET_PI_EXTENSION_VERSION", env!("CARGO_PKG_VERSION"))
        .env("PI_CODING_AGENT_SESSION_DIR", &args.session_dir);

    if let Some(project) = args.project {
        command.env("NOET_PI_PROJECT", project);
    }
    if let Some(subject) = args.subject {
        command.env("NOET_PI_SUBJECT", subject);
    }
    if let Some(agent_dir) = args.agent_dir {
        command.env("PI_CODING_AGENT_DIR", agent_dir);
    }

    if args.no_discovered_extensions {
        command.arg("--no-extensions");
    }
    command.arg("--extension").arg(extension_path);
    command.args(args.pi_args);
    command
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());

    let status = command.status().await?;
    if status.success() {
        Ok(())
    } else {
        Err(NoetError::InvalidConfig(format!(
            "pi exited with status {status}"
        )))
    }
}

fn resolve_noether_url(arg_url: Option<url::Url>) -> Result<url::Url, NoetError> {
    match arg_url {
        Some(url) => Ok(url),
        None => std::env::var("NOET_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:4040".to_owned())
            .parse()
            .map_err(NoetError::Url),
    }
}

fn default_pi_extension_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("integrations/pi/noether-extension.js")
}

async fn check_noether_health(noether_url: &url::Url) -> Result<(), NoetError> {
    let health_url = noether_url.join("/health")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(750))
        .build()?;
    let response = client.get(health_url).send().await?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(NoetError::InvalidConfig(format!(
            "Noether health check returned {}",
            response.status()
        )))
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
