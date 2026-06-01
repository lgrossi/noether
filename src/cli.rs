use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;

use clap::{Parser, Subcommand};
use serde_json::Value;
use tokio::fs;

use crate::config::AutoUpdateMode;
use crate::contract::{AuthorizeDecision, DecisionMode, FinalizeReservation, TraceEvent};
use crate::dashboard::{render_dashboard, render_simulation_dashboard, summary_value};
use crate::error::NoetError;
use crate::fixture::{list_fixture_paths, read_fixture};
use crate::ledger::{
    AsyncPostgresLedgerOptions, BudgetLedger, TraceReport, TraceReportItem, UsageReport,
};
use crate::local::{
    DEFAULT_LOCAL_BIND, DEFAULT_LOCAL_CONFIG, DEFAULT_LOCAL_POLICY, clear_local_sidecar_owner,
    ensure_local_runtime_layout, load_local_config, read_local_sidecar_owner,
    write_local_sidecar_owner_sync,
};
use crate::policy::{load_policy, policy_validation_warnings};
use crate::proxy::load_proxy_routes;
use crate::redaction::redaction_findings;
use crate::reporting;
use crate::scenario::{
    ScenarioAssertion, ScenarioFallbackExpectation, ScenarioFile, ScenarioReportSource,
    ScenarioRequest, validate_scenario,
};
use crate::server::{ServeConfig, serve};
use crate::simulation::{
    SimulationDatabase, SimulationFile, compare_strategies_with_database, validate_simulation,
};
use crate::update::{DEFAULT_UPDATE_MANIFEST_URL, UpdatePlan, apply_update, fetch_update_plan};

#[derive(Parser)]
#[command(name = "noet")]
#[command(about = "Noether control sidecar tooling")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run noet from the standard runtime layout.
    Up(UpArgs),
    /// Stop a detached noet runtime.
    Down(DownArgs),
    /// Print noet runtime status.
    Status(StatusArgs),
    /// Inspect or initialize noet configuration.
    Config(ConfigCommand),
    /// Print detached noet runtime logs.
    Logs(LogsArgs),
    /// Open a noet app surface in the browser.
    Open(OpenArgs),
    /// Run the local capture and decision server.
    Serve(ServeArgs),
    /// Run Noether with the standard local `.noet/` runtime layout.
    Local(LocalCommand),
    /// Validate and inspect policy files.
    Policy(PolicyCommand),
    /// Inspect captured fixture files.
    Fixtures(FixturesCommand),
    /// Report persisted decisions, usage, traces, and observations.
    Report(ReportCommand),
    /// Replay executable Noether scenarios without live provider traffic.
    Scenario(ScenarioCommand),
    /// Compare strategy variants over synthetic demand.
    Simulate(SimulateCommand),
    /// Check for and apply Noether core binary updates.
    Update(UpdateCommand),
}

#[derive(Parser)]
struct ServeArgs {
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1:4040")]
    bind: SocketAddr,

    /// Directory where redacted capture fixtures are written.
    #[arg(long, default_value = ".noet/fixtures")]
    fixture_dir: PathBuf,

    /// Directory where generated simulation artifacts are read for browser/API surfaces.
    #[arg(long, default_value = ".noet/simulations")]
    simulation_dir: PathBuf,

    /// SQLite ledger path for durable local state.
    #[arg(long, default_value = ".noet/noether.sqlite")]
    db_path: PathBuf,

    /// PostgreSQL connection URL. When set, this replaces the SQLite ledger path.
    #[arg(long, env = "NOET_DATABASE_URL")]
    database_url: Option<String>,

    /// Optional bearer token required for HTTP requests.
    #[arg(long, env = "NOET_API_KEY")]
    api_key: Option<String>,

    /// Trusted IAP/reverse-proxy header carrying authenticated actor identity.
    #[arg(long, env = "NOET_ACTOR_HEADER")]
    actor_header: Option<String>,

    /// PostgreSQL durability/latency profile: strict or performance.
    #[arg(long, env = "NOET_POSTGRES_PROFILE", default_value = "strict")]
    postgres_profile: String,

    /// Number of async PostgreSQL connections to use for hot-path writes.
    #[arg(long)]
    postgres_pool_size: Option<usize>,

    /// Override whether PostgreSQL finalization persistence is queued after updating in-memory state.
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    postgres_async_finalize: Option<bool>,

    /// Bounded queue size for async PostgreSQL finalize persistence.
    #[arg(long, long)]
    postgres_finalize_queue_capacity: Option<usize>,

    /// Per-connection PostgreSQL synchronous_commit setting: on, off, local, remote_write, remote_apply.
    #[arg(long)]
    postgres_synchronous_commit: Option<String>,

    /// Emit debug logs with PostgreSQL hot-path stage timings.
    #[arg(long)]
    postgres_stage_timing: bool,

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
struct UpArgs {
    /// Config file to run. When omitted, noet uses the standard local config.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Directory that should contain the `.noet/` runtime home. Defaults to the user's home.
    #[arg(long)]
    root: Option<PathBuf>,

    /// Address to bind. Overrides config.yaml when set.
    #[arg(long)]
    bind: Option<SocketAddr>,

    /// Start in the background and write logs under the runtime home.
    #[arg(short = 'd', long)]
    detach: bool,

    /// Optional upstream base URL. When omitted, Noether returns mock responses.
    #[arg(long)]
    upstream: Option<url::Url>,

    /// Optional transparent proxy route config YAML.
    #[arg(long)]
    routes: Option<PathBuf>,

    /// Decision mode for the local sidecar. Overrides config.yaml when set.
    #[arg(long, value_enum)]
    decision_mode: Option<DecisionMode>,
}

#[derive(Parser)]
struct DownArgs {
    /// Directory that should contain the `.noet/` runtime home. Defaults to the user's home.
    #[arg(long)]
    root: Option<PathBuf>,
}

#[derive(Parser)]
struct StatusArgs {
    /// Directory that should contain the `.noet/` runtime home. Defaults to the user's home.
    #[arg(long)]
    root: Option<PathBuf>,
}

#[derive(Parser)]
struct ConfigCommand {
    #[command(subcommand)]
    command: ConfigSubcommand,
}

#[derive(Subcommand)]
enum ConfigSubcommand {
    /// Print the resolved config path.
    Path(ConfigPathArgs),
    /// Create the default config/policy/runtime layout if missing.
    Init(ConfigPathArgs),
    /// Print the effective config YAML.
    Show(ConfigPathArgs),
    /// Open the config file in $EDITOR.
    Edit(ConfigPathArgs),
}

#[derive(Parser)]
struct ConfigPathArgs {
    /// Directory that should contain the `.noet/` runtime home. Defaults to the user's home.
    #[arg(long)]
    root: Option<PathBuf>,

    /// Config profile to initialize or resolve.
    #[arg(long, value_enum, default_value_t = ConfigProfile::Local)]
    profile: ConfigProfile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum ConfigProfile {
    Local,
    Server,
    Container,
}

#[derive(Parser)]
struct LogsArgs {
    /// Directory that should contain the `.noet/` runtime home. Defaults to the user's home.
    #[arg(long)]
    root: Option<PathBuf>,
}

#[derive(Parser)]
struct OpenArgs {
    /// App surface to open.
    #[arg(default_value = "policy")]
    surface: OpenSurface,

    /// Directory that should contain the `.noet/` runtime home. Defaults to the user's home.
    #[arg(long)]
    root: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum OpenSurface {
    Policy,
    Runs,
    Replay,
    Docs,
}

#[derive(Parser)]
struct LocalCommand {
    #[command(subcommand)]
    command: LocalSubcommand,
}

#[derive(Subcommand)]
enum LocalSubcommand {
    /// Start the local sidecar with repo-local `.noet/` defaults.
    Up(LocalUpArgs),
    /// Print repo-local sidecar owner state.
    Status(LocalStatusArgs),
}

#[derive(Parser)]
struct LocalUpArgs {
    /// Repo root that should contain the `.noet/` runtime home.
    #[arg(long, default_value = ".")]
    root: PathBuf,

    /// Address to bind for the local sidecar.
    #[arg(long, default_value = DEFAULT_LOCAL_BIND)]
    bind: SocketAddr,

    /// Optional upstream base URL. When omitted, Noether returns mock responses.
    #[arg(long)]
    upstream: Option<url::Url>,

    /// Optional transparent proxy route config YAML.
    #[arg(long)]
    routes: Option<PathBuf>,

    /// Decision mode for the local sidecar.
    #[arg(long, value_enum, default_value_t = DecisionMode::Enforce)]
    decision_mode: DecisionMode,
}

#[derive(Parser)]
struct LocalStatusArgs {
    /// Repo root that should contain the `.noet/` runtime home.
    #[arg(long, default_value = ".")]
    root: PathBuf,
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

    /// PostgreSQL connection URL. When set, this replaces the SQLite ledger path.
    #[arg(long, env = "NOET_DATABASE_URL")]
    database_url: Option<String>,

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
        #[arg(long)]
        trace: Option<String>,
    },
    /// Summarize self-approval overrides and audit signals.
    ApprovalAudit,
    /// Write a self-contained visual HTML dashboard.
    Dashboard {
        /// Output HTML path.
        #[arg(long, default_value = ".noet/noether-dashboard.html")]
        out: PathBuf,
        /// Trace to feature. Defaults to the latest decision trace when available.
        #[arg(long)]
        trace: Option<String>,
    },
}

#[derive(Parser)]
struct ScenarioCommand {
    #[command(subcommand)]
    command: ScenarioSubcommand,
}

#[derive(Subcommand)]
enum ScenarioSubcommand {
    /// Replay a scenario file and generate local artifacts.
    Run {
        /// Scenario YAML file.
        path: PathBuf,
        /// Directory where scenario artifacts are written.
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
}

#[derive(Parser)]
struct SimulateCommand {
    /// Simulation YAML file.
    path: PathBuf,
    /// Directory where simulation artifacts are written.
    #[arg(long)]
    out_dir: Option<PathBuf>,
    /// PostgreSQL connection URL. When set, simulation strategies execute on PostgreSQL.
    #[arg(long, env = "NOET_DATABASE_URL")]
    database_url: Option<String>,
    /// PostgreSQL durability/latency profile: strict or performance.
    #[arg(long, env = "NOET_POSTGRES_PROFILE", default_value = "strict")]
    postgres_profile: String,
}

#[derive(Parser)]
struct UpdateCommand {
    #[command(subcommand)]
    command: UpdateSubcommand,
}

#[derive(Subcommand)]
enum UpdateSubcommand {
    /// Check the release manifest for an auto-update-eligible version.
    Check(UpdateCheckArgs),
    /// Apply an auto-update-eligible release to the current noet binary.
    Apply(UpdateApplyArgs),
}

#[derive(Parser)]
struct UpdateCheckArgs {
    /// Release manifest URL.
    #[arg(long, default_value = DEFAULT_UPDATE_MANIFEST_URL)]
    manifest_url: String,
}

#[derive(Parser)]
struct UpdateApplyArgs {
    /// Release manifest URL.
    #[arg(long, default_value = DEFAULT_UPDATE_MANIFEST_URL)]
    manifest_url: String,

    /// Confirm replacing the current noet binary.
    #[arg(long)]
    yes: bool,
}

pub async fn run() -> Result<(), NoetError> {
    let cli = Cli::parse();

    match cli.command {
        Command::Up(args) => run_up(args).await,
        Command::Down(args) => run_down(args).await,
        Command::Status(args) => run_status(args).await,
        Command::Config(command) => run_config(command).await,
        Command::Logs(args) => run_logs(args).await,
        Command::Open(args) => run_open(args).await,
        Command::Serve(args) => {
            let policy_path = args.policy.clone();
            let postgres_options = if args.database_url.is_some() {
                postgres_options_from_serve_args(&args)?
            } else {
                AsyncPostgresLedgerOptions::default()
            };
            let policy = match policy_path.as_ref() {
                Some(path) => Some(load_policy(path).await?),
                None => None,
            };
            let routes = match args.routes {
                Some(path) => load_proxy_routes(&path).await?.routes,
                None => Vec::new(),
            };
            serve(ServeConfig {
                bind: args.bind,
                fixture_dir: args.fixture_dir,
                simulation_dir: args.simulation_dir,
                db_path: args.db_path,
                database_url: args.database_url,
                postgres_options,
                upstream: args.upstream,
                routes,
                policy_path,
                policy,
                noether_config: Default::default(),
                decision_mode: args.decision_mode,
                api_key: args.api_key,
                actor_header: args.actor_header,
                on_bound: None,
            })
            .await
        }
        Command::Local(command) => run_local(command).await,
        Command::Policy(command) => run_policy(command).await,
        Command::Fixtures(command) => run_fixtures(command).await,
        Command::Report(command) => run_report(command).await,
        Command::Scenario(command) => run_scenario(command).await,
        Command::Simulate(command) => run_simulate(command).await,
        Command::Update(command) => run_update(command).await,
    }
}

fn postgres_options_from_serve_args(
    args: &ServeArgs,
) -> Result<AsyncPostgresLedgerOptions, NoetError> {
    let mut options = AsyncPostgresLedgerOptions::from_profile(&args.postgres_profile)?;
    if let Some(pool_size) = parse_env_usize_option("NOET_POSTGRES_POOL_SIZE")? {
        options.pool_size = pool_size.max(1);
    }
    if let Some(async_finalize) = parse_env_bool_option("NOET_POSTGRES_ASYNC_FINALIZE")? {
        options.async_finalize = async_finalize;
    }
    if let Some(finalize_queue_capacity) =
        parse_env_usize_option("NOET_POSTGRES_FINALIZE_QUEUE_CAPACITY")?
    {
        options.finalize_queue_capacity = finalize_queue_capacity.max(1);
    }
    if let Some(synchronous_commit) = std::env::var("NOET_POSTGRES_SYNCHRONOUS_COMMIT")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        options.synchronous_commit = Some(synchronous_commit);
    }
    apply_postgres_timeout_env_options(&mut options)?;
    if let Some(stage_timing) = parse_env_bool_option("NOET_POSTGRES_STAGE_TIMING")? {
        options.stage_timing = stage_timing;
    }
    if let Some(pool_size) = args.postgres_pool_size {
        options.pool_size = pool_size.max(1);
    }
    if let Some(async_finalize) = args.postgres_async_finalize {
        options.async_finalize = async_finalize;
    }
    if let Some(finalize_queue_capacity) = args.postgres_finalize_queue_capacity {
        options.finalize_queue_capacity = finalize_queue_capacity.max(1);
    }
    if let Some(synchronous_commit) = args.postgres_synchronous_commit.clone() {
        options.synchronous_commit = Some(synchronous_commit);
    }
    if args.postgres_stage_timing {
        options.stage_timing = true;
    }
    Ok(options)
}

fn postgres_options_from_runtime_env() -> Result<AsyncPostgresLedgerOptions, NoetError> {
    let profile = std::env::var("NOET_POSTGRES_PROFILE").unwrap_or_else(|_| "strict".to_owned());
    let mut options = AsyncPostgresLedgerOptions::from_profile(&profile)?;
    if let Some(pool_size) = parse_env_usize_option("NOET_POSTGRES_POOL_SIZE")? {
        options.pool_size = pool_size.max(1);
    }
    if let Some(async_finalize) = parse_env_bool_option("NOET_POSTGRES_ASYNC_FINALIZE")? {
        options.async_finalize = async_finalize;
    }
    if let Some(finalize_queue_capacity) =
        parse_env_usize_option("NOET_POSTGRES_FINALIZE_QUEUE_CAPACITY")?
    {
        options.finalize_queue_capacity = finalize_queue_capacity.max(1);
    }
    if let Some(synchronous_commit) = std::env::var("NOET_POSTGRES_SYNCHRONOUS_COMMIT")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        options.synchronous_commit = Some(synchronous_commit);
    }
    apply_postgres_timeout_env_options(&mut options)?;
    if let Some(stage_timing) = parse_env_bool_option("NOET_POSTGRES_STAGE_TIMING")? {
        options.stage_timing = stage_timing;
    }
    Ok(options)
}

fn apply_postgres_timeout_env_options(
    options: &mut AsyncPostgresLedgerOptions,
) -> Result<(), NoetError> {
    if let Some(acquire_timeout_ms) = parse_env_u64_option("NOET_POSTGRES_ACQUIRE_TIMEOUT_MS")? {
        options.acquire_timeout_ms = acquire_timeout_ms.max(1);
    }
    if let Some(statement_timeout_ms) = parse_env_u64_option("NOET_POSTGRES_STATEMENT_TIMEOUT_MS")?
    {
        options.statement_timeout_ms = statement_timeout_ms;
    }
    if let Some(idle_transaction_timeout_ms) =
        parse_env_u64_option("NOET_POSTGRES_IDLE_TX_TIMEOUT_MS")?
    {
        options.idle_transaction_timeout_ms = idle_transaction_timeout_ms;
    }
    if let Some(lock_timeout_ms) = parse_env_u64_option("NOET_POSTGRES_LOCK_TIMEOUT_MS")? {
        options.lock_timeout_ms = lock_timeout_ms;
    }
    Ok(())
}

fn parse_env_usize_option(name: &str) -> Result<Option<usize>, NoetError> {
    let Some(value) = std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    value.trim().parse::<usize>().map(Some).map_err(|error| {
        NoetError::InvalidConfig(format!("invalid {name} value {value:?}: {error}"))
    })
}

fn parse_env_u64_option(name: &str) -> Result<Option<u64>, NoetError> {
    let Some(value) = std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    value.trim().parse::<u64>().map(Some).map_err(|error| {
        NoetError::InvalidConfig(format!("invalid {name} value {value:?}: {error}"))
    })
}

fn parse_env_bool_option(name: &str) -> Result<Option<bool>, NoetError> {
    let Some(value) = std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(NoetError::InvalidConfig(format!(
            "invalid {name} value {value:?}; expected true/false"
        ))),
    }
}

async fn run_local(command: LocalCommand) -> Result<(), NoetError> {
    match command.command {
        LocalSubcommand::Up(args) => {
            run_runtime(RuntimeArgs {
                config_path: None,
                root: args.root,
                bind: Some(args.bind),
                detach: false,
                upstream: args.upstream,
                routes: args.routes,
                decision_mode: Some(args.decision_mode),
            })
            .await
        }
        LocalSubcommand::Status(args) => print_runtime_status(args.root.as_path()).await,
    }
}

async fn run_up(args: UpArgs) -> Result<(), NoetError> {
    run_runtime(RuntimeArgs {
        config_path: args.config,
        root: resolve_noet_root(args.root),
        bind: args.bind,
        detach: args.detach,
        upstream: args.upstream,
        routes: args.routes,
        decision_mode: args.decision_mode,
    })
    .await
}

async fn run_down(args: DownArgs) -> Result<(), NoetError> {
    let root = resolve_noet_root(args.root);
    match read_local_sidecar_owner(&root).await? {
        Some(owner) => {
            if !process_exists(owner.pid) {
                clear_local_sidecar_owner(&root).await?;
                println!("state\tstopped");
                println!("stale_pid\t{}", owner.pid);
                return Ok(());
            }
            if !process_looks_like_noet(owner.pid) {
                return Err(NoetError::InvalidConfig(format!(
                    "refusing to stop pid {}; owner file is stale or does not point to noet",
                    owner.pid
                )));
            }
            stop_process(owner.pid)?;
            clear_local_sidecar_owner(&root).await?;
            println!("state\tstopped");
            println!("pid\t{}", owner.pid);
        }
        None => println!("state\tstopped"),
    }
    Ok(())
}

async fn run_status(args: StatusArgs) -> Result<(), NoetError> {
    let root = resolve_noet_root(args.root);
    print_runtime_status(&root).await
}

async fn run_config(command: ConfigCommand) -> Result<(), NoetError> {
    match command.command {
        ConfigSubcommand::Path(args) => {
            let layout = config_layout(args.profile, args.root);
            println!("{}", layout.config_path.display());
            Ok(())
        }
        ConfigSubcommand::Init(args) => {
            let layout = ensure_config_layout(args.profile, args.root).await?;
            println!("config\t{}", layout.config_path.display());
            println!("policy\t{}", layout.policy_path.display());
            println!("db\t{}", layout.db_path.display());
            Ok(())
        }
        ConfigSubcommand::Show(args) => {
            let layout = ensure_config_layout(args.profile, args.root).await?;
            let config = load_local_config(&layout.config_path).await?;
            println!("{}", serde_yaml::to_string(&config)?);
            Ok(())
        }
        ConfigSubcommand::Edit(args) => {
            let layout = ensure_config_layout(args.profile, args.root).await?;
            open_config_editor(&layout.config_path)?;
            Ok(())
        }
    }
}

async fn run_logs(args: LogsArgs) -> Result<(), NoetError> {
    let root = resolve_noet_root(args.root);
    let layout = crate::local::LocalRuntimeLayout::for_root(&root);
    match fs::read_to_string(&layout.log_path).await {
        Ok(logs) => {
            print!("{logs}");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(NoetError::NotFound(
            format!("no noet log found at {}", layout.log_path.display()),
        )),
        Err(error) => Err(error.into()),
    }
}

async fn run_open(args: OpenArgs) -> Result<(), NoetError> {
    let root = resolve_noet_root(args.root);
    let base_url = read_local_sidecar_owner(&root)
        .await?
        .map(|owner| owner.url)
        .unwrap_or_else(|| format!("http://{DEFAULT_LOCAL_BIND}"));
    let url = format!("{}{}", base_url.trim_end_matches('/'), args.surface.path());
    open::that(&url)
        .map_err(|error| NoetError::InvalidConfig(format!("failed to open {url}: {error}")))?;
    println!("{url}");
    Ok(())
}

impl OpenSurface {
    fn path(self) -> &'static str {
        match self {
            Self::Policy => "/policy",
            Self::Runs => "/runs",
            Self::Replay => "/replay",
            Self::Docs => "/docs",
        }
    }
}

struct ConfigLayout {
    config_path: PathBuf,
    policy_path: PathBuf,
    db_path: PathBuf,
}

fn config_layout(profile: ConfigProfile, root: Option<PathBuf>) -> ConfigLayout {
    match profile {
        ConfigProfile::Local => {
            let layout = crate::local::LocalRuntimeLayout::for_root(&resolve_noet_root(root));
            ConfigLayout {
                config_path: layout.config_path,
                policy_path: layout.policy_path,
                db_path: layout.db_path,
            }
        }
        ConfigProfile::Server | ConfigProfile::Container => ConfigLayout {
            config_path: PathBuf::from("/etc/noet/config.yaml"),
            policy_path: PathBuf::from("/etc/noet/policy.yaml"),
            db_path: PathBuf::from("/var/lib/noet/noet.sqlite"),
        },
    }
}

async fn ensure_config_layout(
    profile: ConfigProfile,
    root: Option<PathBuf>,
) -> Result<ConfigLayout, NoetError> {
    if profile == ConfigProfile::Local {
        let layout = ensure_local_runtime_layout(&resolve_noet_root(root)).await?;
        return Ok(ConfigLayout {
            config_path: layout.config_path,
            policy_path: layout.policy_path,
            db_path: layout.db_path,
        });
    }

    let layout = config_layout(profile, root);
    if let Some(parent) = layout.config_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    if let Some(parent) = layout.db_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    if !fs::try_exists(&layout.policy_path).await? {
        fs::write(&layout.policy_path, DEFAULT_LOCAL_POLICY).await?;
    }
    if !fs::try_exists(&layout.config_path).await? {
        fs::write(&layout.config_path, profile_config_yaml(profile)).await?;
    }
    Ok(layout)
}

fn profile_config_yaml(profile: ConfigProfile) -> &'static str {
    match profile {
        ConfigProfile::Local => DEFAULT_LOCAL_CONFIG,
        ConfigProfile::Server => {
            r#"server:
  bind: 127.0.0.1:4051
policy:
  path: /etc/noet/policy.yaml
  decision_mode: enforce
storage:
  sqlite_path: /var/lib/noet/noet.sqlite
  fixture_dir: /var/lib/noet/fixtures
  simulation_dir: /var/lib/noet/simulations
updates:
  auto: patch
  check_on_start: false
advisory:
  warning_cadence: 4h
"#
        }
        ConfigProfile::Container => {
            r#"server:
  bind: 0.0.0.0:4051
policy:
  path: /etc/noet/policy.yaml
  decision_mode: enforce
storage:
  sqlite_path: /var/lib/noet/noet.sqlite
  fixture_dir: /var/lib/noet/fixtures
  simulation_dir: /var/lib/noet/simulations
updates:
  auto: off
  check_on_start: false
advisory:
  warning_cadence: 4h
"#
        }
    }
}

struct RuntimeArgs {
    config_path: Option<PathBuf>,
    root: PathBuf,
    bind: Option<SocketAddr>,
    detach: bool,
    upstream: Option<url::Url>,
    routes: Option<PathBuf>,
    decision_mode: Option<DecisionMode>,
}

async fn run_runtime(args: RuntimeArgs) -> Result<(), NoetError> {
    if args.config_path.is_some() && args.detach {
        return Err(NoetError::InvalidConfig(
            "`noet up --config` runs in the foreground; use process manager or container runtime to detach"
                .to_owned(),
        ));
    }
    let local_layout = if args.config_path.is_some() {
        None
    } else {
        Some(ensure_local_runtime_layout(&args.root).await?)
    };
    if args.detach {
        let layout = local_layout
            .as_ref()
            .expect("detached runtime has local layout");
        spawn_detached_runtime(&args, layout)?;
        println!("state\tstarting");
        println!("log\t{}", layout.log_path.display());
        return Ok(());
    }

    let config_path = args
        .config_path
        .clone()
        .or_else(|| {
            local_layout
                .as_ref()
                .map(|layout| layout.config_path.clone())
        })
        .ok_or_else(|| NoetError::InvalidConfig("missing noet config path".to_owned()))?;
    let noether_config = load_local_config(&config_path).await?;
    run_update_on_start_if_configured(&noether_config).await?;
    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    let bind =
        args.bind
            .or(noether_config.server.bind)
            .unwrap_or(DEFAULT_LOCAL_BIND.parse().map_err(|error| {
                NoetError::InvalidConfig(format!(
                    "invalid default bind {DEFAULT_LOCAL_BIND}: {error}"
                ))
            })?);
    let policy_path = noether_config
        .policy
        .path
        .as_ref()
        .map(|path| resolve_config_path(config_dir, path))
        .or_else(|| {
            local_layout
                .as_ref()
                .map(|layout| layout.policy_path.clone())
        })
        .unwrap_or_else(|| config_dir.join("policy.yaml"));
    let db_path = noether_config
        .storage
        .sqlite_path
        .as_ref()
        .map(|path| resolve_config_path(config_dir, path))
        .or_else(|| local_layout.as_ref().map(|layout| layout.db_path.clone()))
        .unwrap_or_else(|| config_dir.join("noet.sqlite"));
    let fixture_dir = noether_config
        .storage
        .fixture_dir
        .as_ref()
        .map(|path| resolve_config_path(config_dir, path))
        .or_else(|| {
            local_layout
                .as_ref()
                .map(|layout| layout.fixture_dir.clone())
        })
        .unwrap_or_else(|| db_path.parent().unwrap_or(config_dir).join("fixtures"));
    let simulation_dir = noether_config
        .storage
        .simulation_dir
        .as_ref()
        .map(|path| resolve_config_path(config_dir, path))
        .or_else(|| {
            local_layout
                .as_ref()
                .map(|layout| layout.simulation_dir.clone())
        })
        .unwrap_or_else(|| db_path.parent().unwrap_or(config_dir).join("simulations"));
    let decision_mode = args
        .decision_mode
        .or(noether_config.policy.decision_mode)
        .unwrap_or(DecisionMode::Enforce);
    let database_url = noether_config.storage.database_url.clone().or_else(|| {
        std::env::var("NOET_DATABASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
    });
    let postgres_options = if database_url.is_some() {
        postgres_options_from_runtime_env()?
    } else {
        AsyncPostgresLedgerOptions::default()
    };

    let policy = load_policy(&policy_path).await?;
    let routes = match args.routes {
        Some(path) => load_proxy_routes(&path).await?.routes,
        None => Vec::new(),
    };
    let on_bound = local_layout.clone().map(|layout| {
        let bind = bind.to_string();
        Box::new(move || {
            write_local_sidecar_owner_sync(&layout, &bind)?;
            Ok(())
        }) as Box<dyn FnOnce() -> Result<(), NoetError> + Send>
    });
    let serve_config = ServeConfig {
        bind,
        fixture_dir,
        simulation_dir,
        db_path,
        database_url,
        postgres_options,
        upstream: args.upstream,
        routes,
        policy_path: Some(policy_path),
        policy: Some(policy),
        noether_config,
        decision_mode,
        api_key: std::env::var("NOET_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        actor_header: std::env::var("NOET_ACTOR_HEADER")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        on_bound,
    };
    let result = tokio::select! {
        result = serve(serve_config) => result,
        signal = wait_for_shutdown_signal() => signal,
    };
    if local_layout.is_some() {
        clear_local_sidecar_owner(&args.root).await?;
    }
    result
}

async fn run_update_on_start_if_configured(
    config: &crate::config::NoetherConfig,
) -> Result<(), NoetError> {
    if config.updates.auto != AutoUpdateMode::Patch || !config.updates.check_on_start {
        return Ok(());
    }
    let manifest_url = config
        .updates
        .manifest_url
        .as_deref()
        .unwrap_or(DEFAULT_UPDATE_MANIFEST_URL);
    let plan = match fetch_update_plan(manifest_url).await {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("update\tskipped check failed: {error}");
            return Ok(());
        }
    };
    if !plan.auto_update_allowed || plan.artifact.is_none() {
        print_update_on_start_status(&plan);
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    let installed = apply_update(&plan).await?;
    println!("updated\t{} -> {}", plan.current, plan.latest);
    println!("binary\t{}", installed.display());
    restart_after_update(&executable)?;
    Ok(())
}

#[cfg(unix)]
fn restart_after_update(executable: &Path) -> Result<(), NoetError> {
    use std::os::unix::process::CommandExt;

    let error = std::process::Command::new(executable)
        .args(std::env::args_os().skip(1))
        .exec();
    Err(error.into())
}

#[cfg(not(unix))]
fn restart_after_update(_executable: &Path) -> Result<(), NoetError> {
    Err(NoetError::InvalidConfig(
        "updated noet binary; restart the process to use it".to_owned(),
    ))
}

fn print_update_on_start_status(plan: &UpdatePlan) {
    if plan.latest <= plan.current {
        println!("update\tcurrent {}", plan.current);
    } else if plan.artifact.is_none() {
        println!(
            "update\tskipped release {} has no artifact for {}",
            plan.latest, plan.target
        );
    } else {
        println!(
            "update\tskipped release {} is not auto-update eligible from {}",
            plan.latest, plan.current
        );
    }
}

async fn print_runtime_status(root: &Path) -> Result<(), NoetError> {
    match read_local_sidecar_owner(root).await? {
        Some(owner) => {
            println!("state\t{}", owner.state);
            println!("pid\t{}", owner.pid);
            println!("cwd\t{}", owner.cwd.display());
            println!("bind\t{}", owner.bind);
            println!("url\t{}", owner.url);
            println!("started_at\t{}", owner.started_at);
        }
        None => println!("state\tstopped"),
    }
    Ok(())
}

fn spawn_detached_runtime(
    args: &RuntimeArgs,
    layout: &crate::local::LocalRuntimeLayout,
) -> Result<(), NoetError> {
    let executable = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&layout.log_path)?;
    let err_log = log.try_clone()?;
    let mut command = std::process::Command::new(executable);
    command.arg("up").arg("--root").arg(&args.root);
    if let Some(bind) = args.bind {
        command.arg("--bind").arg(bind.to_string());
    }
    if let Some(upstream) = args.upstream.as_ref() {
        command.arg("--upstream").arg(upstream.as_str());
    }
    if let Some(routes) = args.routes.as_ref() {
        command.arg("--routes").arg(routes);
    }
    if let Some(decision_mode) = args.decision_mode {
        command
            .arg("--decision-mode")
            .arg(decision_mode_cli_value(decision_mode));
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err_log))
        .spawn()?;
    Ok(())
}

fn stop_process(pid: u32) -> Result<(), NoetError> {
    #[cfg(unix)]
    let status = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status()?;
    #[cfg(windows)]
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(NoetError::InvalidConfig(format!(
            "failed to stop noet process {pid}"
        )))
    }
}

fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .map(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
}

fn process_looks_like_noet(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .and_then(|path| path.file_name().map(|name| name.to_owned()))
            .and_then(|name| name.to_str().map(str::to_owned))
            .map(|name| is_noet_executable_name(&name))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

fn is_noet_executable_name(name: &str) -> bool {
    matches!(
        name,
        "noet" | "noet.exe" | "noet (deleted)" | "noet.exe (deleted)"
    )
}

fn resolve_noet_root(root: Option<PathBuf>) -> PathBuf {
    root.unwrap_or_else(default_noet_parent)
}

fn default_noet_parent() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn resolve_config_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn decision_mode_cli_value(mode: DecisionMode) -> &'static str {
    match mode {
        DecisionMode::DryRun => "dry-run",
        DecisionMode::Enforce => "enforce",
    }
}

async fn wait_for_shutdown_signal() -> Result<(), NoetError> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
    }
    Ok(())
}

fn open_config_editor(path: &Path) -> Result<(), NoetError> {
    if let Some(editor) = std::env::var_os("EDITOR").filter(|value| !value.is_empty()) {
        let status = std::process::Command::new(editor).arg(path).status()?;
        if status.success() {
            return Ok(());
        }
        return Err(NoetError::InvalidConfig(format!(
            "editor exited unsuccessfully for {}",
            path.display()
        )));
    }
    open::that(path).map_err(|error| {
        NoetError::InvalidConfig(format!("failed to open {}: {error}", path.display()))
    })
}

async fn run_policy(command: PolicyCommand) -> Result<(), NoetError> {
    match command.command {
        PolicySubcommand::Check { path } => {
            let policy = load_policy(&path).await?;
            for warning in policy_validation_warnings(&policy) {
                println!("warning: {warning}");
            }
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
    let ledger = match command.database_url.as_deref() {
        Some(database_url) => BudgetLedger::open_postgres(database_url)?,
        None => BudgetLedger::open_sqlite(&command.db_path)?,
    };
    match command.command {
        ReportSubcommand::Usage => {
            let report = reporting::usage_report(&ledger)?;
            if command.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for line in render_usage_report_lines(&report) {
                    println!("{line}");
                }
            }
        }
        ReportSubcommand::Decisions => {
            print_items(reporting::decisions_report(&ledger)?, command.json)?;
        }
        ReportSubcommand::Trace { trace_id } => {
            let report = reporting::trace_report(&ledger, &trace_id)?;
            if command.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for line in render_trace_report_lines(&report) {
                    println!("{line}");
                }
            }
        }
        ReportSubcommand::Observations { kind, trace } => {
            print_items(
                reporting::observations_report(&ledger, kind.as_deref(), trace.as_deref())?,
                command.json,
            )?;
        }
        ReportSubcommand::ApprovalAudit => {
            let report = reporting::approval_audit_report(&ledger)?;
            if command.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for line in render_approval_audit_report_lines(&report) {
                    println!("{line}");
                }
            }
        }
        ReportSubcommand::Dashboard { out, trace } => {
            let report = reporting::dashboard_report(&ledger, trace.as_deref())?;
            let html = render_dashboard(
                &report.usage,
                &report.decisions,
                report.trace.as_ref(),
                &report.observations,
            );
            if let Some(parent) = out.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent).await?;
            }
            fs::write(&out, html).await?;
            println!("dashboard\t{}", out.display());
            if let Some(trace_id) = report.featured_trace_id {
                println!("featured_trace\t{trace_id}");
            }
        }
    }
    Ok(())
}

async fn run_scenario(command: ScenarioCommand) -> Result<(), NoetError> {
    match command.command {
        ScenarioSubcommand::Run { path, out_dir } => {
            let artifacts = replay_scenario_file(&path, out_dir.as_deref()).await?;
            println!("scenario\t{}", artifacts.name);
            println!("output_dir\t{}", artifacts.output_dir.display());
            println!("db_path\t{}", artifacts.db_path.display());
            println!("usage_report\t{}", artifacts.usage_report_path.display());
            println!(
                "decisions_report\t{}",
                artifacts.decisions_report_path.display()
            );
            println!("dashboard\t{}", artifacts.dashboard_path.display());
            for (request_id, path) in artifacts.trace_report_paths {
                println!("trace_report\t{request_id}\t{}", path.display());
            }
            Ok(())
        }
    }
}

async fn run_simulate(command: SimulateCommand) -> Result<(), NoetError> {
    let simulation = load_simulation_file(&command.path).await?;
    let simulation_name = simulation
        .name
        .clone()
        .unwrap_or_else(|| simulation_output_slug(&command.path));
    let out_dir = command.out_dir.clone().unwrap_or_else(|| {
        PathBuf::from(".noet/simulations").join(simulation_output_slug(&command.path))
    });
    let report = compare_strategies_with_database(
        &simulation,
        &out_dir,
        simulation_database_from_command(&command)?,
    )
    .await?;
    let report_path = out_dir.join("simulation-report.json");
    write_json_file(&report_path, &report).await?;
    let simulation_dashboard_path = out_dir.join("simulation-dashboard.html");
    fs::write(
        &simulation_dashboard_path,
        render_simulation_dashboard(&report),
    )
    .await?;
    println!("simulation\t{}", simulation_name);
    println!("output_dir\t{}", out_dir.display());
    println!("simulation_report\t{}", report_path.display());
    println!(
        "simulation_dashboard\t{}",
        simulation_dashboard_path.display()
    );
    if let Some(timing) = &report.timing {
        println!("timing_total_ms\t{:.2}", timing.total_ms);
        println!(
            "timing_generate_demand_ms\t{:.2}",
            timing.generate_demand_ms
        );
        println!("timing_strategies_ms\t{:.2}", timing.strategies_ms);
    }
    for strategy in &report.strategies {
        let strategy_usage_report_path = out_dir.join(&strategy.usage_report_path);
        let strategy_decisions_report_path = out_dir.join(&strategy.decisions_report_path);
        let strategy_dashboard_path = strategy_usage_report_path
            .parent()
            .unwrap_or(&out_dir)
            .join("noether-dashboard.html");
        let usage: UsageReport =
            serde_json::from_slice(&fs::read(&strategy_usage_report_path).await?)?;
        let decisions: Vec<TraceReportItem> =
            serde_json::from_slice(&fs::read(&strategy_decisions_report_path).await?)?;
        fs::write(
            &strategy_dashboard_path,
            render_dashboard(&usage, &decisions, None, &[]),
        )
        .await?;
        println!("strategy\t{}", strategy.id);
        for (key, value) in strategy.database_location().cli_lines(&out_dir) {
            println!("{key}\t{value}");
        }
        if let Some(timing) = &strategy.timing {
            println!("timing_strategy_total_ms\t{:.2}", timing.total_ms);
            println!("timing_strategy_init_ms\t{:.2}", timing.init_ms);
            println!("timing_strategy_replay_ms\t{:.2}", timing.replay_ms);
            println!("timing_strategy_persist_ms\t{:.2}", timing.persist_ms);
            println!("timing_strategy_report_ms\t{:.2}", timing.report_ms);
            println!("timing_strategy_artifact_ms\t{:.2}", timing.artifact_ms);
        }
        println!("usage_report\t{}", strategy_usage_report_path.display());
        println!(
            "decisions_report\t{}",
            strategy_decisions_report_path.display()
        );
        println!("dashboard\t{}", strategy_dashboard_path.display());
    }
    Ok(())
}

async fn run_update(command: UpdateCommand) -> Result<(), NoetError> {
    match command.command {
        UpdateSubcommand::Check(args) => {
            let plan = fetch_update_plan(&args.manifest_url).await?;
            print_update_plan(&plan);
            Ok(())
        }
        UpdateSubcommand::Apply(args) => {
            let plan = fetch_update_plan(&args.manifest_url).await?;
            print_update_plan(&plan);
            if !args.yes {
                return Err(NoetError::InvalidConfig(
                    "refusing to replace binary without --yes".to_owned(),
                ));
            }
            let installed_path = apply_update(&plan).await?;
            println!(
                "updated noet to {} at {}",
                plan.latest,
                installed_path.display()
            );
            #[cfg(windows)]
            println!("windows replacement is scheduled and completes after this process exits");
            Ok(())
        }
    }
}

fn print_update_plan(plan: &crate::update::UpdatePlan) {
    println!("current: {}", plan.current);
    println!("latest: {}", plan.latest);
    println!("channel: {}", plan.manifest.channel);
    println!("release_type: {}", plan.manifest.release_type);
    println!("target: {}", plan.target);
    println!(
        "auto_update_eligible: {}",
        plan.manifest.auto_update_eligible
    );
    println!("auto_update_allowed: {}", plan.auto_update_allowed);
    match plan.artifact.as_ref() {
        Some(artifact) => println!("artifact: {}", artifact.file),
        None => println!("artifact: missing"),
    }
}

fn simulation_database_from_command(
    command: &SimulateCommand,
) -> Result<SimulationDatabase, NoetError> {
    command
        .database_url
        .clone()
        .map(|database_url| {
            AsyncPostgresLedgerOptions::from_profile(&command.postgres_profile)
                .map(|options| SimulationDatabase::postgres(database_url, options))
        })
        .unwrap_or_else(|| Ok(SimulationDatabase::sqlite()))
}

struct ScenarioArtifacts {
    name: String,
    output_dir: PathBuf,
    db_path: PathBuf,
    usage_report_path: PathBuf,
    decisions_report_path: PathBuf,
    dashboard_path: PathBuf,
    trace_report_paths: Vec<(String, PathBuf)>,
}

struct ScenarioReplayRequestResult {
    request_id: String,
    trace_id: String,
    decision: AuthorizeDecision,
}

async fn replay_scenario_file(
    path: &Path,
    out_dir: Option<&Path>,
) -> Result<ScenarioArtifacts, NoetError> {
    let scenario = load_scenario_file(path).await?;
    let scenario_name = scenario
        .name
        .clone()
        .unwrap_or_else(|| scenario_output_slug(path));
    let output_dir = out_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".noet/scenarios").join(scenario_output_slug(path)));
    fs::create_dir_all(&output_dir).await?;

    let traces_dir = output_dir.join("traces");
    if fs::try_exists(&traces_dir).await? {
        fs::remove_dir_all(&traces_dir).await?;
    }
    fs::create_dir_all(&traces_dir).await?;

    let db_path = output_dir.join("noet.sqlite");
    if fs::try_exists(&db_path).await? {
        fs::remove_file(&db_path).await?;
    }

    let session_id = format!(
        "scenario:{}",
        scenario_name.replace(char::is_whitespace, "-")
    );
    let mut ledger = BudgetLedger::open_sqlite(&db_path)?;
    let mut trace_report_paths = Vec::new();
    let mut replay_results = Vec::new();
    let mut trace_reports = BTreeMap::new();

    for request in &scenario.requests {
        let replay_result = replay_scenario_request(&mut ledger, &scenario, request, &session_id)?;
        let trace_report = ledger.trace_report(&replay_result.trace_id)?;
        let trace_report_path = traces_dir.join(format!(
            "{}.json",
            encode_path_component(&replay_result.request_id, "trace")
        ));
        write_json_file(&trace_report_path, &trace_report).await?;
        trace_report_paths.push((replay_result.request_id.clone(), trace_report_path));
        trace_reports.insert(replay_result.request_id.clone(), trace_report);
        replay_results.push(replay_result);
    }

    let usage = ledger.usage_report()?;
    let usage_report_path = output_dir.join("usage-report.json");
    write_json_file(&usage_report_path, &usage).await?;

    let decisions = ledger.decisions_report()?;
    let decisions_report_path = output_dir.join("decisions-report.json");
    write_json_file(&decisions_report_path, &decisions).await?;

    let featured_trace_id = decisions
        .iter()
        .find_map(|item| summary_value(&item.summary, "trace"));
    let trace_report = featured_trace_id
        .as_deref()
        .map(|trace_id| ledger.trace_report(trace_id))
        .transpose()?;
    let observations = ledger.observations_report(None, featured_trace_id.as_deref())?;
    let dashboard = render_dashboard(&usage, &decisions, trace_report.as_ref(), &observations);
    evaluate_scenario_assertions(
        &scenario,
        &replay_results,
        &usage,
        &decisions,
        &trace_reports,
        &dashboard,
    )?;
    let dashboard_path = output_dir.join("noether-dashboard.html");
    fs::write(&dashboard_path, dashboard).await?;

    Ok(ScenarioArtifacts {
        name: scenario_name,
        output_dir,
        db_path,
        usage_report_path,
        decisions_report_path,
        dashboard_path,
        trace_report_paths,
    })
}

async fn load_scenario_file(path: &Path) -> Result<ScenarioFile, NoetError> {
    let bytes = fs::read(path).await?;
    let scenario: ScenarioFile = serde_yaml::from_slice(&bytes)?;
    validate_scenario(&scenario)?;
    Ok(scenario)
}

async fn load_simulation_file(path: &Path) -> Result<SimulationFile, NoetError> {
    let bytes = fs::read(path).await?;
    let simulation: SimulationFile = serde_yaml::from_slice(&bytes)?;
    validate_simulation(&simulation)?;
    Ok(simulation)
}

fn replay_scenario_request(
    ledger: &mut BudgetLedger,
    scenario: &ScenarioFile,
    request: &ScenarioRequest,
    session_id: &str,
) -> Result<ScenarioReplayRequestResult, NoetError> {
    let mut authorize = request.authorize.clone();
    authorize.entities = merged_entities(&scenario.entities, &authorize.entities);

    let metadata = &mut authorize.metadata;
    metadata.insert("request_id".to_owned(), Value::String(request.id.clone()));
    metadata.insert("trace_id".to_owned(), Value::String(request.id.clone()));
    metadata.insert(
        "session_id".to_owned(),
        Value::String(session_id.to_owned()),
    );
    metadata.insert("source".to_owned(), Value::String("scenario".to_owned()));

    let trace_id = request.id.clone();
    let decision = ledger.try_authorize(Some(&scenario.policy), &authorize)?;

    if decision.outcome != crate::contract::DecisionOutcome::Deny {
        if let Some(model_choice) = &request.model_choice {
            let mut payload = BTreeMap::new();
            if let Some(provider) = model_choice
                .provider
                .clone()
                .or_else(|| authorize.provider.clone())
            {
                payload.insert("provider".to_owned(), Value::String(provider));
            }
            if let Some(model) = model_choice
                .model
                .clone()
                .or_else(|| authorize.model.clone())
            {
                payload.insert("model".to_owned(), Value::String(model));
            }
            payload.insert("request_id".to_owned(), Value::String(request.id.clone()));
            payload.insert("source".to_owned(), Value::String("scenario".to_owned()));
            ledger.record_event(TraceEvent {
                id: None,
                trace_id: Some(trace_id.clone()),
                occurred_at: None,
                kind: "pi.provider_call.started".to_owned(),
                payload: serde_json::to_value(payload)?,
            })?;
        }

        for tool in &request.tool_activity {
            let mut payload = serde_json::to_value(tool)?;
            let payload_map = payload.as_object_mut().ok_or_else(|| {
                NoetError::InvalidConfig("scenario tool payload must be an object".to_owned())
            })?;
            payload_map.insert("request_id".to_owned(), Value::String(request.id.clone()));
            payload_map.insert("source".to_owned(), Value::String("scenario".to_owned()));
            ledger.record_event(TraceEvent {
                id: None,
                trace_id: Some(trace_id.clone()),
                occurred_at: None,
                kind: "tool.observed".to_owned(),
                payload,
            })?;
        }

        if let Some(finalize) = &request.finalize
            && let Some(reservation) = &decision.reservation
        {
            let payload = scenario_finalize_payload(request, finalize, &trace_id);
            ledger.finalize(&reservation.id, &payload)?;
        }
    }

    Ok(ScenarioReplayRequestResult {
        request_id: request.id.clone(),
        trace_id,
        decision,
    })
}

fn scenario_finalize_payload(
    request: &ScenarioRequest,
    finalize: &crate::scenario::ScenarioFinalizeStep,
    trace_id: &str,
) -> FinalizeReservation {
    let mut usage = finalize.usage.clone();
    if let Some(usage) = &mut usage
        && let Some(model_choice) = &request.model_choice
    {
        if usage.provider.is_none() {
            usage.provider = model_choice.provider.clone();
        }
        if usage.model.is_none() {
            usage.model = model_choice.model.clone();
        }
    }

    let mut metadata = finalize.metadata.clone();
    metadata.insert("request_id".to_owned(), Value::String(request.id.clone()));
    metadata.insert("trace_id".to_owned(), Value::String(trace_id.to_owned()));
    metadata.insert("source".to_owned(), Value::String("scenario".to_owned()));

    FinalizeReservation {
        reservation_id: None,
        outcome: crate::contract::FinalizeOutcome::Success,
        usage,
        actual_cost_usd: finalize.actual_cost_usd,
        metadata,
    }
}

fn merged_entities(global: &[String], local: &[String]) -> Vec<String> {
    let mut entities = Vec::new();
    for entity in global.iter().chain(local) {
        if !entities.contains(entity) {
            entities.push(entity.clone());
        }
    }
    entities
}

fn scenario_output_slug(path: &Path) -> String {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("scenario");
    let trimmed = file_name
        .strip_suffix(".yaml")
        .or_else(|| file_name.strip_suffix(".yml"))
        .unwrap_or(file_name)
        .strip_suffix(".noet")
        .unwrap_or(file_name);
    encode_path_component(trimmed, "scenario")
}

fn simulation_output_slug(path: &Path) -> String {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("simulation");
    let trimmed = file_name
        .strip_suffix(".yaml")
        .or_else(|| file_name.strip_suffix(".yml"))
        .unwrap_or(file_name)
        .strip_suffix(".noet")
        .unwrap_or(file_name);
    encode_path_component(trimmed, "simulation")
}

fn encode_path_component(value: &str, fallback: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(*byte as char);
            }
            _ => {
                let _ = write!(&mut encoded, "~{byte:02x}");
            }
        }
    }
    if encoded.is_empty() {
        fallback.to_owned()
    } else {
        encoded
    }
}

async fn write_json_file<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), NoetError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).await?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?).await?;
    Ok(())
}

fn evaluate_scenario_assertions(
    scenario: &ScenarioFile,
    replay_results: &[ScenarioReplayRequestResult],
    usage: &UsageReport,
    decisions: &[TraceReportItem],
    trace_reports: &BTreeMap<String, TraceReport>,
    dashboard: &str,
) -> Result<(), NoetError> {
    let decision_reports = scenario_decision_reports(replay_results, trace_reports);
    let replay_by_request: BTreeMap<&str, &ScenarioReplayRequestResult> = replay_results
        .iter()
        .map(|result| (result.request_id.as_str(), result))
        .collect();
    let usage_json = serde_json::to_value(usage)?;
    let decisions_json = serde_json::to_value(decisions)?;
    let usage_text = render_usage_report_lines(usage).join("\n");
    let decisions_text = render_items_lines(decisions).join("\n");
    let trace_texts: BTreeMap<String, String> = trace_reports
        .iter()
        .map(|(request_id, trace)| {
            (
                request_id.clone(),
                render_trace_report_lines(trace).join("\n"),
            )
        })
        .collect();
    let mut failures = Vec::new();
    let reports = ScenarioAssertionReports {
        decision_reports: &decision_reports,
        usage_json: &usage_json,
        decisions_json: &decisions_json,
        trace_reports,
        usage_text: &usage_text,
        decisions_text: &decisions_text,
        trace_texts: &trace_texts,
        dashboard,
    };

    for assertion in &scenario.assertions {
        evaluate_scenario_assertion(assertion, None, &reports, &mut failures);
    }

    for request in &scenario.requests {
        if let Some(expectation) = &request.denial {
            match replay_by_request.get(request.id.as_str()) {
                Some(result) => {
                    if result.decision.outcome != crate::contract::DecisionOutcome::Deny {
                        failures.push(format!(
                            "request {} expected denial but outcome was {:?}",
                            request.id, result.decision.outcome
                        ));
                    }
                    if let Some(rule_id) = expectation.rule_id.as_deref()
                        && !result
                            .decision
                            .explanations
                            .iter()
                            .any(|explanation| explanation.rule_id == rule_id)
                    {
                        failures.push(format!(
                            "request {} expected denial rule {rule_id}",
                            request.id
                        ));
                    }
                    if let Some(reason) = expectation.reason_contains.as_deref()
                        && !result
                            .decision
                            .explanations
                            .iter()
                            .any(|explanation| explanation.reason.contains(reason))
                    {
                        failures.push(format!(
                            "request {} expected denial reason containing {reason}",
                            request.id
                        ));
                    }
                }
                None => failures.push(format!(
                    "request {} is missing replay data for denial expectation",
                    request.id
                )),
            }
        }

        if let Some(expectation) = &request.fallback {
            evaluate_fallback_expectation(
                request.id.as_str(),
                expectation,
                &decision_reports,
                &mut failures,
            );
        }

        for assertion in &request.assertions {
            evaluate_scenario_assertion(
                assertion,
                Some(request.id.as_str()),
                &reports,
                &mut failures,
            );
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(NoetError::InvalidConfig(format!(
            "scenario assertions failed: {}",
            failures.join("; ")
        )))
    }
}

struct ScenarioAssertionReports<'a> {
    decision_reports: &'a BTreeMap<String, &'a TraceReportItem>,
    usage_json: &'a Value,
    decisions_json: &'a Value,
    trace_reports: &'a BTreeMap<String, TraceReport>,
    usage_text: &'a str,
    decisions_text: &'a str,
    trace_texts: &'a BTreeMap<String, String>,
    dashboard: &'a str,
}

fn evaluate_scenario_assertion(
    assertion: &ScenarioAssertion,
    default_request_id: Option<&str>,
    reports: &ScenarioAssertionReports,
    failures: &mut Vec<String>,
) {
    match assertion {
        ScenarioAssertion::DecisionOutcome {
            request_id,
            outcome,
        } => match reports.decision_reports.get(request_id.as_str()) {
            Some(item) if decision_matches_outcome(item.kind.as_str(), *outcome) => {}
            Some(item) => failures.push(format!(
                "request {request_id} expected outcome {:?} but report kind was {}",
                outcome, item.kind
            )),
            None => failures.push(format!("missing decision report for request {request_id}")),
        },
        ScenarioAssertion::SelectedBudget {
            request_id,
            budget_id,
        } => match reports.decision_reports.get(request_id.as_str()) {
            Some(item)
                if item
                    .routing
                    .as_ref()
                    .and_then(|routing| routing.selected_budget_id.as_deref())
                    == Some(budget_id.as_str()) => {}
            Some(item) => failures.push(format!(
                "request {request_id} expected selected budget {budget_id} but saw {:?}",
                item.routing
                    .as_ref()
                    .and_then(|routing| routing.selected_budget_id.as_deref())
            )),
            None => failures.push(format!("missing decision report for request {request_id}")),
        },
        ScenarioAssertion::Denied { request_id } => {
            match reports.decision_reports.get(request_id.as_str()) {
                Some(item)
                    if decision_matches_outcome(
                        item.kind.as_str(),
                        crate::contract::DecisionOutcome::Deny,
                    ) => {}
                Some(item) => failures.push(format!(
                    "request {request_id} expected deny outcome but report kind was {}",
                    item.kind
                )),
                None => failures.push(format!("missing decision report for request {request_id}")),
            }
        }
        ScenarioAssertion::TotalCostUsd { amount_usd } => {
            if (reports.usage_json["total_cost_usd"]
                .as_f64()
                .unwrap_or_default()
                - amount_usd)
                .abs()
                > 1e-9
            {
                failures.push(format!(
                    "expected total_cost_usd {:.6} but saw {:.6}",
                    amount_usd,
                    reports.usage_json["total_cost_usd"]
                        .as_f64()
                        .unwrap_or_default()
                ));
            }
        }
        ScenarioAssertion::LimitHit {
            request_id,
            rule_id,
        } => match reports.decision_reports.get(request_id.as_str()) {
            Some(item)
                if item
                    .limit_hits
                    .as_ref()
                    .is_some_and(|hits| hits.iter().any(|hit| hit.rule_id == *rule_id)) => {}
            Some(_) => failures.push(format!("request {request_id} expected limit hit {rule_id}")),
            None => failures.push(format!("missing decision report for request {request_id}")),
        },
        ScenarioAssertion::Fallback {
            request_id,
            requested_budget_id,
            selected_budget_id,
            matched_entity,
        } => evaluate_fallback_assertion(
            request_id,
            requested_budget_id.as_deref(),
            selected_budget_id.as_deref(),
            matched_entity.as_deref(),
            reports.decision_reports,
            failures,
        ),
        ScenarioAssertion::ReportJson {
            report,
            request_id,
            pointer,
            equals,
        } => {
            let actual = report_json_value(
                *report,
                request_id.as_deref().or(default_request_id),
                pointer,
                reports.usage_json,
                reports.decisions_json,
                reports.trace_reports,
            );
            match actual {
                Some(value) if value == *equals => {}
                Some(value) => failures.push(format!(
                    "report_json {:?} {} expected {} but saw {}",
                    report, pointer, equals, value
                )),
                None => failures.push(format!(
                    "report_json {:?} {} did not resolve",
                    report, pointer
                )),
            }
        }
        ScenarioAssertion::ReportContains {
            report,
            request_id,
            text,
        } => {
            if !report_text_value(
                *report,
                request_id.as_deref().or(default_request_id),
                reports.usage_text,
                reports.decisions_text,
                reports.trace_texts,
            )
            .is_some_and(|report_text| report_text.contains(text))
            {
                failures.push(format!("report output {:?} missing {text}", report));
            }
        }
        ScenarioAssertion::DashboardContains { text } => {
            if !reports.dashboard.contains(text) {
                failures.push(format!("dashboard output missing {text}"));
            }
        }
    }
}

fn scenario_decision_reports<'a>(
    replay_results: &[ScenarioReplayRequestResult],
    trace_reports: &'a BTreeMap<String, TraceReport>,
) -> BTreeMap<String, &'a TraceReportItem> {
    replay_results
        .iter()
        .filter_map(|result| {
            trace_reports
                .get(&result.request_id)
                .and_then(|trace| {
                    trace
                        .items
                        .iter()
                        .find(|item| item.kind.starts_with("decision."))
                })
                .map(|item| (result.request_id.clone(), item))
        })
        .collect()
}

fn report_json_value(
    report: ScenarioReportSource,
    request_id: Option<&str>,
    pointer: &str,
    usage_json: &Value,
    decisions_json: &Value,
    trace_reports: &BTreeMap<String, TraceReport>,
) -> Option<Value> {
    match report {
        ScenarioReportSource::Usage => usage_json.pointer(pointer).cloned(),
        ScenarioReportSource::Decisions => decisions_json.pointer(pointer).cloned(),
        ScenarioReportSource::Trace => request_id
            .and_then(|request_id| trace_reports.get(request_id))
            .and_then(|trace| serde_json::to_value(trace).ok())
            .and_then(|trace_json| trace_json.pointer(pointer).cloned()),
    }
}

fn report_text_value<'a>(
    report: ScenarioReportSource,
    request_id: Option<&str>,
    usage_text: &'a str,
    decisions_text: &'a str,
    trace_texts: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    match report {
        ScenarioReportSource::Usage => Some(usage_text),
        ScenarioReportSource::Decisions => Some(decisions_text),
        ScenarioReportSource::Trace => request_id
            .and_then(|request_id| trace_texts.get(request_id))
            .map(String::as_str),
    }
}

fn evaluate_fallback_expectation(
    request_id: &str,
    expectation: &ScenarioFallbackExpectation,
    decision_reports: &BTreeMap<String, &TraceReportItem>,
    failures: &mut Vec<String>,
) {
    evaluate_fallback_assertion(
        request_id,
        expectation.requested_budget_id.as_deref(),
        expectation.selected_budget_id.as_deref(),
        expectation.matched_entity.as_deref(),
        decision_reports,
        failures,
    );
}

fn evaluate_fallback_assertion(
    request_id: &str,
    requested_budget_id: Option<&str>,
    selected_budget_id: Option<&str>,
    matched_entity: Option<&str>,
    decision_reports: &BTreeMap<String, &TraceReportItem>,
    failures: &mut Vec<String>,
) {
    match decision_reports.get(request_id) {
        Some(item) => {
            let routing = item.routing.as_ref();
            if let Some(expected) = requested_budget_id
                && routing.and_then(|routing| routing.rejected_budget_id.as_deref())
                    != Some(expected)
            {
                failures.push(format!(
                    "request {request_id} expected fallback from requested budget {expected} but saw {:?}",
                    routing.and_then(|routing| routing.rejected_budget_id.as_deref())
                ));
            }
            if let Some(expected) = selected_budget_id
                && routing.and_then(|routing| routing.selected_budget_id.as_deref())
                    != Some(expected)
            {
                failures.push(format!(
                    "request {request_id} expected fallback selected budget {expected} but saw {:?}",
                    routing.and_then(|routing| routing.selected_budget_id.as_deref())
                ));
            }
            if let Some(expected) = matched_entity
                && routing.and_then(|routing| routing.matched_entity.as_deref()) != Some(expected)
            {
                failures.push(format!(
                    "request {request_id} expected fallback matched entity {expected} but saw {:?}",
                    routing.and_then(|routing| routing.matched_entity.as_deref())
                ));
            }
        }
        None => failures.push(format!("missing decision report for request {request_id}")),
    }
}

fn decision_matches_outcome(kind: &str, outcome: crate::contract::DecisionOutcome) -> bool {
    match outcome {
        crate::contract::DecisionOutcome::Allow => kind.ends_with(".allow"),
        crate::contract::DecisionOutcome::Warn => kind.ends_with(".warn"),
        crate::contract::DecisionOutcome::Deny => kind.ends_with(".deny"),
    }
}

fn render_usage_report_lines(report: &UsageReport) -> Vec<String> {
    let mut lines = vec![
        format!("total_cost_usd\t{:.6}", report.total_cost_usd),
        "project\tprovider\tmodel\tsubject\tinput_tokens\toutput_tokens\tcache_read_tokens\tcache_write_tokens\ttotal_tokens\tcost_usd\tcache_read_cost_usd\tcache_write_cost_usd\treservations\tactive\tfinalized".to_owned(),
    ];
    for row in &report.rows {
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.6}\t{:.6}\t{:.6}\t{}\t{}\t{}",
            row.project.as_deref().unwrap_or("-"),
            row.provider.as_deref().unwrap_or("-"),
            row.model.as_deref().unwrap_or("-"),
            row.subject.as_deref().unwrap_or("-"),
            row.input_tokens,
            row.output_tokens,
            row.cache_read_tokens,
            row.cache_write_tokens,
            row.total_tokens,
            row.total_cost_usd,
            row.cache_read_cost_usd,
            row.cache_write_cost_usd,
            row.reservations,
            row.active_reservations,
            row.finalized_reservations
        ));
    }
    if let Some(adoption) = &report.protected_adoption {
        lines.push(format!(
            "unused_protected_opportunity_usd\t{:.6}",
            adoption.unused_protected_opportunity_usd
        ));
        lines.push(format!(
            "carryover_liability_usd\t{:.6}",
            adoption.carryover_liability_usd
        ));
        lines.push(
            "adoption_level\tbudget_id\tentity_key\tprotected_amount_usd\tcurrent_grant_usd\tcarryover_usd\tused_current_grant_usd"
                .to_owned(),
        );
        for entity in &adoption.low_adopters {
            lines.push(format!(
                "low\t{}\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}",
                entity.budget_id,
                entity.entity_key,
                entity.protected_amount_usd,
                entity.current_grant_usd,
                entity.carryover_usd,
                entity.used_current_grant_usd
            ));
        }
        for entity in &adoption.high_adopters {
            lines.push(format!(
                "high\t{}\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}",
                entity.budget_id,
                entity.entity_key,
                entity.protected_amount_usd,
                entity.current_grant_usd,
                entity.carryover_usd,
                entity.used_current_grant_usd
            ));
        }
    }
    lines
}

fn render_items_lines(items: &[crate::ledger::TraceReportItem]) -> Vec<String> {
    let mut lines = vec!["occurred_at\tkind\tsummary".to_owned()];
    for item in items {
        lines.push(format!(
            "{}\t{}\t{}",
            item.occurred_at.to_rfc3339(),
            item.kind,
            item.summary
        ));
    }
    lines
}

fn render_trace_report_lines(report: &TraceReport) -> Vec<String> {
    let mut lines = vec![format!("trace\t{}", report.trace_id)];
    lines.extend(render_items_lines(&report.items));
    lines
}

fn render_approval_audit_report_lines(
    report: &crate::approval_audit::ApprovalAuditReport,
) -> Vec<String> {
    let mut lines = vec![
        format!("approval_overrides\t{}", report.summary.total),
        format!("approved\t{}", report.summary.approved),
        format!("rejected\t{}", report.summary.rejected),
        format!("high_risk\t{}", report.summary.high_risk),
        format!(
            "repeated_subject_rule_approvals\t{}",
            report.summary.repeated_subject_rule_approvals
        ),
        format!(
            "missing_attribution\t{}",
            report.summary.missing_attribution
        ),
        "occurred_at\toutcome\tsubject\tproject\trule\ttrace\tflags".to_owned(),
    ];
    for item in &report.items {
        let flags = if item.risk_flags.is_empty() {
            "none".to_owned()
        } else {
            item.risk_flags
                .iter()
                .map(|flag| format!("{flag:?}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        lines.push(format!(
            "{}\t{:?}\t{}\t{}\t{}\t{}\t{}",
            item.occurred_at,
            item.outcome,
            item.subject.as_deref().unwrap_or("-"),
            item.project.as_deref().unwrap_or("-"),
            item.rule_id.as_deref().unwrap_or("-"),
            item.trace_id.as_deref().unwrap_or("-"),
            flags
        ));
    }
    lines
}

fn print_items(items: Vec<crate::ledger::TraceReportItem>, json: bool) -> Result<(), NoetError> {
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        for line in render_items_lines(&items) {
            println!("{line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use std::path::Path;
    use tempfile::tempdir;

    use crate::dashboard::{
        decision_headline, decision_model_check_label, decision_supporting_line,
    };
    use crate::ledger::UsageReportRow;

    use super::*;

    fn compare_checked_in_simulation(path: &str) -> crate::simulation::SimulationComparisonReport {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let content = std::fs::read_to_string(manifest_dir.join(path))
            .expect("checked-in simulation example is readable");
        let simulation: crate::simulation::SimulationFile =
            serde_yaml::from_str(&content).expect("checked-in simulation example parses");
        let tempdir = tempdir().expect("tempdir");
        crate::simulation::compare_strategies(&simulation, tempdir.path())
            .expect("checked-in simulation comparison succeeds")
    }

    #[test]
    fn dashboard_renders_budget_routing_explanation_markers() {
        let decisions = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "decision.allow".to_owned(),
            summary: "decision_id=dec_1 selected_budget=project-budget matched_entity=project:noether selection_reason=selected fallback budget for project:noether rejected_budget=missing-budget rejected_reason=requested budget does not exist model_check=allowed:project-budget remaining_budget=0.750000".to_owned(),
            trace_id: None,
                    agent_run_id: None,
                entities: Vec::new(),
        routing: None,
            limit_hits: None,
            binding_limit: None,
        }];
        let usage = UsageReport {
            total_cost_usd: 0.0,
            rows: Vec::new(),
            protected_adoption: None,
        };

        let html = render_dashboard(&usage, &decisions, None, &[]);

        assert!(html.contains("selected_budget=project-budget"));
        assert!(html.contains("matched_entity=project:noether"));
        assert!(html.contains("rejected_budget=missing-budget"));
        assert!(html.contains("model_check=allowed:project-budget"));
    }

    #[test]
    fn usage_report_human_output_includes_protected_adoption_summary() {
        let usage = UsageReport {
            total_cost_usd: 30.0,
            rows: Vec::new(),
            protected_adoption: Some(crate::ledger::ProtectedAdoptionReport {
                unused_protected_opportunity_usd: 25.0,
                carryover_liability_usd: 5.0,
                low_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:alice".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 24.0,
                    carryover_usd: 0.0,
                    used_current_grant_usd: 1.0,
                }],
                high_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:bob".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 1.0,
                    carryover_usd: 5.0,
                    used_current_grant_usd: 24.0,
                }],
            }),
        };

        let lines = render_usage_report_lines(&usage);

        assert!(
            lines
                .iter()
                .any(|line| line == "unused_protected_opportunity_usd\t25.000000")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "carryover_liability_usd\t5.000000")
        );
        assert!(lines.iter().any(|line| line.contains("user:alice")));
        assert!(lines.iter().any(|line| line.contains("user:bob")));
    }

    #[test]
    fn dashboard_renders_protected_adoption_sections() {
        let usage = UsageReport {
            total_cost_usd: 30.0,
            rows: Vec::new(),
            protected_adoption: Some(crate::ledger::ProtectedAdoptionReport {
                unused_protected_opportunity_usd: 25.0,
                carryover_liability_usd: 5.0,
                low_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:alice".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 24.0,
                    carryover_usd: 0.0,
                    used_current_grant_usd: 1.0,
                }],
                high_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:bob".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 1.0,
                    carryover_usd: 5.0,
                    used_current_grant_usd: 24.0,
                }],
            }),
        };

        let html = render_dashboard(&usage, &[], None, &[]);

        assert!(html.contains("Protected opportunity"));
        assert!(html.contains("Carryover liability"));
        assert!(html.contains("Low adopters"));
        assert!(html.contains("Top consumers"));
        assert!(html.contains("Adoption snapshot"));
        assert!(html.contains("Protected opportunity remaining"));
        assert!(!html.contains("<table"));
    }

    #[test]
    fn dashboard_prioritizes_story_cards_over_tables_for_real_runs() {
        let usage = UsageReport {
            total_cost_usd: 25.0,
            rows: vec![
                UsageReportRow {
                    subject: Some("user:bob".to_owned()),
                    project: Some("platform".to_owned()),
                    provider: Some("openai".to_owned()),
                    model: Some("gpt-4.1".to_owned()),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    total_tokens: 96_000,
                    cache_read_cost_usd: 0.0,
                    cache_write_cost_usd: 0.0,
                    total_cost_usd: 24.0,
                    reservations: 1,
                    active_reservations: 0,
                    finalized_reservations: 1,
                },
                UsageReportRow {
                    subject: Some("user:alice".to_owned()),
                    project: Some("docs".to_owned()),
                    provider: Some("openai".to_owned()),
                    model: Some("gpt-4.1-mini".to_owned()),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    total_tokens: 4_000,
                    cache_read_cost_usd: 0.0,
                    cache_write_cost_usd: 0.0,
                    total_cost_usd: 1.0,
                    reservations: 1,
                    active_reservations: 0,
                    finalized_reservations: 1,
                },
            ],
            protected_adoption: Some(crate::ledger::ProtectedAdoptionReport {
                unused_protected_opportunity_usd: 25.0,
                carryover_liability_usd: 0.0,
                low_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:alice".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 24.0,
                    carryover_usd: 0.0,
                    used_current_grant_usd: 1.0,
                }],
                high_adopters: vec![crate::ledger::ProtectedAdoptionEntityReport {
                    budget_id: "ai-adoption".to_owned(),
                    entity_key: "user:bob".to_owned(),
                    protected_amount_usd: 25.0,
                    current_grant_usd: 1.0,
                    carryover_usd: 0.0,
                    used_current_grant_usd: 24.0,
                }],
            }),
        };
        let decisions = vec![
            TraceReportItem {
                occurred_at: Utc::now(),
                kind: "decision.allow".to_owned(),
                summary: "decision_id=req-bob trace=req-bob request=req-bob model=openai/gpt-4.1 selected_budget=ai-adoption matched_entity=org:example selection_reason=selected fallback budget for org:example model_check=allowed:ai-adoption remaining_budget=1975.000000".to_owned(),
                trace_id: Some("req-bob".to_owned()),
                        agent_run_id: None,
                entities: Vec::new(),
        routing: Some(crate::ledger::DecisionRoutingReport {
                    selected_budget_id: Some("ai-adoption".to_owned()),
                    matched_entity: Some("org:example".to_owned()),
                    selection_reason: Some("selected fallback budget for org:example".to_owned()),
                    rejected_budget_id: None,
                    rejected_budget_reason: None,
                    model_check: Some("allowed:ai-adoption".to_owned()),
                    budget_window_remaining_usd: Some(1975.0),
                    budget_window_mode: None,
                    budget_window_started_at: None,
                    budget_window_ends_at: None,
                }),
                limit_hits: None,
            binding_limit: None,
            },
            TraceReportItem {
                occurred_at: Utc::now(),
                kind: "decision.allow".to_owned(),
                summary: "decision_id=req-alice trace=req-alice request=req-alice model=openai/gpt-4.1-mini selected_budget=ai-adoption matched_entity=org:example selection_reason=selected fallback budget for org:example model_check=allowed:ai-adoption remaining_budget=1999.000000".to_owned(),
                trace_id: Some("req-alice".to_owned()),
                        agent_run_id: None,
                entities: Vec::new(),
        routing: Some(crate::ledger::DecisionRoutingReport {
                    selected_budget_id: Some("ai-adoption".to_owned()),
                    matched_entity: Some("org:example".to_owned()),
                    selection_reason: Some("selected fallback budget for org:example".to_owned()),
                    rejected_budget_id: None,
                    rejected_budget_reason: None,
                    model_check: Some("allowed:ai-adoption".to_owned()),
                    budget_window_remaining_usd: Some(1999.0),
                    budget_window_mode: None,
                    budget_window_started_at: None,
                    budget_window_ends_at: None,
                }),
                limit_hits: None,
            binding_limit: None,
            },
        ];

        let html = render_dashboard(&usage, &decisions, None, &[]);

        for marker in [
            "Budget posture",
            "Decision narrative",
            "Adoption snapshot",
            "Protected opportunity remaining",
            "Budget routing",
        ] {
            assert!(html.contains(marker), "missing narrative marker: {marker}");
        }
        assert!(!html.contains("<table"));
    }

    #[test]
    fn dashboard_renders_risky_run_section_for_limit_hits() {
        let usage = UsageReport {
            total_cost_usd: 0.0,
            rows: Vec::new(),
            protected_adoption: None,
        };
        let decisions = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "decision.deny".to_owned(),
            summary: "decision_id=dec_limit limit_hits=dev-budget.context_tokens".to_owned(),
            trace_id: None,
            agent_run_id: None,
            entities: Vec::new(),
            routing: None,
            limit_hits: Some(vec![crate::ledger::DecisionLimitHitReport {
                rule_id: "dev-budget.context_tokens".to_owned(),
                reason: "estimated context tokens 1200 exceed enforced limit max 1000".to_owned(),
                severity: crate::contract::DecisionSeverity::Deny,
                window_id: Some("daily-cap".to_owned()),
                window_mode: Some("tumbling".to_owned()),
                window_started_at: None,
                window_ends_at: None,
                projected_spend_usd: None,
                max_usd: None,
                scope_entity: None,
            }]),
            binding_limit: None,
        }];

        let html = render_dashboard(&usage, &decisions, None, &[]);

        assert!(html.contains("Risky runs"));
        assert!(html.contains("daily-cap tumbling limit"));
        assert!(html.contains("Show exact limit evidence"));
        assert!(html.contains("Limit hits"));
    }

    #[test]
    fn dashboard_renders_lifecycle_limits_section() {
        let usage = UsageReport {
            total_cost_usd: 0.0,
            rows: Vec::new(),
            protected_adoption: None,
        };
        let trace = TraceReport {
            trace_id: "trace-lifecycle".to_owned(),
            items: vec![TraceReportItem {
                occurred_at: Utc::now(),
                kind: "limit.report_only.tool_calls".to_owned(),
                summary: "tool_calls=12 max_tool_calls=10 reporting_only=true source=pi.tool_call"
                    .to_owned(),
                trace_id: Some("trace-lifecycle".to_owned()),
                agent_run_id: None,
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            }],
        };

        let html = render_dashboard(&usage, &[], Some(&trace), &[]);

        assert!(html.contains("Lifecycle limits (report-only)"));
        assert!(html.contains("audit evidence"));
        assert!(html.contains("not proof that Noether blocked"));
        assert!(html.contains("report-only"));
        assert!(html.contains("limit.report_only.tool_calls"));
    }

    #[test]
    fn dashboard_hides_empty_visual_sections_for_limit_only_run() {
        let usage = UsageReport {
            total_cost_usd: 0.0,
            rows: Vec::new(),
            protected_adoption: None,
        };
        let decisions = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "decision.deny".to_owned(),
            summary:
                "decision_id=dec_limit model=openai/gpt-4.1 limit_hits=runaway-budget.request_cost"
                    .to_owned(),
            trace_id: None,
            agent_run_id: None,
            entities: Vec::new(),
            routing: None,
            limit_hits: Some(vec![crate::ledger::DecisionLimitHitReport {
                rule_id: "runaway-budget.request_cost".to_owned(),
                reason: "estimated request cost $2.500000 exceeds enforced limit max $1.000000"
                    .to_owned(),
                severity: crate::contract::DecisionSeverity::Deny,
                window_id: None,
                window_mode: None,
                window_started_at: None,
                window_ends_at: None,
                projected_spend_usd: None,
                max_usd: None,
                scope_entity: None,
            }]),
            binding_limit: None,
        }];

        let html = render_dashboard(&usage, &decisions, None, &[]);

        assert!(!html.contains("<section class=\"split\"></section>"));
        assert!(!html.contains("Spend evidence"));
        assert!(!html.contains("Adoption snapshot"));
    }

    #[test]
    fn trace_report_human_output_has_stable_header_and_rows() {
        let report = TraceReport {
            trace_id: "trace-1".to_owned(),
            items: vec![TraceReportItem {
                occurred_at: Utc::now(),
                kind: "decision.allow".to_owned(),
                summary: "decision_id=dec_1".to_owned(),
                trace_id: None,
                agent_run_id: None,
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            }],
        };

        let lines = render_trace_report_lines(&report);

        assert_eq!(lines[0], "trace\ttrace-1");
        assert_eq!(lines[1], "occurred_at\tkind\tsummary");
        assert!(lines[2].contains("\tdecision.allow\tdecision_id=dec_1"));
    }

    #[test]
    fn trace_report_human_output_preserves_budget_window_summary_tokens() {
        let report = TraceReport {
            trace_id: "trace-window".to_owned(),
            items: vec![TraceReportItem {
                occurred_at: Utc::now(),
                kind: "decision.warn".to_owned(),
                summary: "decision_id=dec_window budget_window_mode=tumbling budget_window_start=2026-05-20T12:00:00Z budget_window_end=2026-05-20T13:00:00Z".to_owned(),
                trace_id: None,
                agent_run_id: None,
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
            binding_limit: None,
            }],
        };

        let lines = render_trace_report_lines(&report);

        assert!(lines[2].contains("budget_window_mode=tumbling"));
        assert!(lines[2].contains("budget_window_start=2026-05-20T12:00:00Z"));
        assert!(lines[2].contains("budget_window_end=2026-05-20T13:00:00Z"));
    }

    #[test]
    fn dashboard_baseline_acceptance_sections_are_present() {
        let usage = UsageReport {
            total_cost_usd: 1.25,
            rows: vec![UsageReportRow {
                subject: Some("user:local".to_owned()),
                project: Some("noether".to_owned()),
                provider: Some("openai".to_owned()),
                model: Some("gpt-4.1".to_owned()),
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_tokens: 150,
                cache_read_cost_usd: 0.0,
                cache_write_cost_usd: 0.0,
                total_cost_usd: 1.25,
                reservations: 1,
                active_reservations: 0,
                finalized_reservations: 1,
            }],
            protected_adoption: None,
        };
        let decisions = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "decision.allow".to_owned(),
            summary: "decision_id=dec_1".to_owned(),
            trace_id: None,
            agent_run_id: None,
            entities: Vec::new(),
            routing: None,
            limit_hits: None,
            binding_limit: None,
        }];
        let trace = TraceReport {
            trace_id: "trace-1".to_owned(),
            items: vec![TraceReportItem {
                occurred_at: Utc::now(),
                kind: "tool.observed".to_owned(),
                summary: "name=bash success=true".to_owned(),
                trace_id: Some("trace-1".to_owned()),
                agent_run_id: None,
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            }],
        };
        let observations = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "pi.turn_end".to_owned(),
            summary: "turn=1".to_owned(),
            trace_id: Some("trace-1".to_owned()),
            agent_run_id: None,
            entities: Vec::new(),
            routing: None,
            limit_hits: None,
            binding_limit: None,
        }];

        let html = render_dashboard(&usage, &decisions, Some(&trace), &observations);

        for marker in [
            "Outcome summary",
            "Finalized spend",
            "Budget posture",
            "Decision narrative",
            "Tool usage",
            "Run timeline",
        ] {
            assert!(html.contains(marker), "missing dashboard marker: {marker}");
        }
        assert!(!html.contains("Skills and context"));
        assert!(!html.contains("Agent activity"));
        assert!(!html.contains("<table"));
    }

    #[test]
    fn simulation_dashboard_renders_showcase_tradeoff_markers() {
        let runaway_report =
            compare_checked_in_simulation("examples/simulations/runaway-pressure.noet.yaml");
        let runaway_html = render_simulation_dashboard(&runaway_report);
        assert!(runaway_html.contains("Comparison summary"));
        assert!(runaway_html.contains("Budget limits changed the spend story"));
        assert!(runaway_html.contains("limited team budget blocked 107 limit-hit requests"));
        assert!(runaway_html.contains(
            "pooled without limit blocked 87 limit-hit requests, prevented $41.21, and left $0.01 unused."
        ));

        let adoption_report =
            compare_checked_in_simulation("examples/simulations/adoption-pressure.noet.yaml");
        let adoption_html = render_simulation_dashboard(&adoption_report);
        assert!(adoption_html.contains("Comparison summary"));
        assert!(adoption_html.contains("Budget limits changed the spend story"));
        assert!(adoption_html.contains("Protected opportunity"));
        assert!(adoption_html.contains(
            "protected adoption surfaced $1.49 of unused protected opportunity across 3 low adopters and 5 high adopters."
        ));
    }

    #[test]
    fn dashboard_renders_real_pi_run_shape_without_raw_logs() {
        let usage = UsageReport {
            total_cost_usd: 0.0019,
            rows: vec![UsageReportRow {
                subject: Some("user:demo".to_owned()),
                project: Some("noether".to_owned()),
                provider: Some("openai-codex".to_owned()),
                model: Some("gpt-demo".to_owned()),
                input_tokens: 900,
                output_tokens: 180,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_tokens: 1080,
                cache_read_cost_usd: 0.0,
                cache_write_cost_usd: 0.0,
                total_cost_usd: 0.0019,
                reservations: 1,
                active_reservations: 0,
                finalized_reservations: 1,
            }],
            protected_adoption: None,
        };
        let now = Utc::now();
        let decisions = vec![TraceReportItem {
            occurred_at: now,
            kind: "decision.allow".to_owned(),
            summary: "decision_id=dec_pi trace=trace-pi model=openai-codex/gpt-demo".to_owned(),
            trace_id: Some("trace-pi".to_owned()),
            agent_run_id: None,
            entities: Vec::new(),
            routing: None,
            limit_hits: None,
            binding_limit: None,
        }];
        let trace = TraceReport {
            trace_id: "trace-pi".to_owned(),
            items: vec![
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.agent_context".to_owned(),
                    summary: "selected_tools=read,bash skills=diagnose context_files=AGENTS.md".to_owned(),
                    trace_id: Some("trace-pi".to_owned()),
                            agent_run_id: None,
                entities: Vec::new(),
        routing: None,
                    limit_hits: None,
            binding_limit: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.provider_call.started".to_owned(),
                    summary: "provider=openai-codex model=gpt-demo shape=input_count=1".to_owned(),
                    trace_id: Some("trace-pi".to_owned()),
                            agent_run_id: None,
                entities: Vec::new(),
        routing: None,
                    limit_hits: None,
            binding_limit: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.tool_call".to_owned(),
                    summary: "tool_name=bash input_summary.command.length=42".to_owned(),
                    trace_id: Some("trace-pi".to_owned()),
                            agent_run_id: None,
                entities: Vec::new(),
        routing: None,
                    limit_hits: None,
            binding_limit: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "tool.observed".to_owned(),
                    summary: "name=bash success=true duration_ms=42".to_owned(),
                    trace_id: Some("trace-pi".to_owned()),
                            agent_run_id: None,
                entities: Vec::new(),
        routing: None,
                    limit_hits: None,
            binding_limit: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.message_end".to_owned(),
                    summary: "provider=openai-codex model=gpt-demo tokens=1080 cost=0.001900".to_owned(),
                    trace_id: Some("trace-pi".to_owned()),
                            agent_run_id: None,
                entities: Vec::new(),
        routing: None,
                    limit_hits: None,
            binding_limit: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.turn_end".to_owned(),
                    summary: "turn=1 usage=(provider=openai-codex model=gpt-demo tokens=1080 cost=0.001900)".to_owned(),
                    trace_id: Some("trace-pi".to_owned()),
                            agent_run_id: None,
                entities: Vec::new(),
        routing: None,
                    limit_hits: None,
            binding_limit: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.agent_end".to_owned(),
                    summary: "messages=2".to_owned(),
                    trace_id: Some("trace-pi".to_owned()),
                            agent_run_id: None,
                entities: Vec::new(),
        routing: None,
                    limit_hits: None,
            binding_limit: None,
                },
            ],
        };

        let html = render_dashboard(&usage, &decisions, Some(&trace), &[]);

        for marker in [
            "Tool usage",
            "Agent activity",
            "Skills and context",
            "pi.provider_call.started",
            "pi.agent_context",
            "pi.turn_end",
            "pi.agent_end",
            "tool.observed",
        ] {
            assert!(html.contains(marker), "missing Pi run marker: {marker}");
        }
        assert!(!html.contains(".raw.jsonl"));
        assert!(!html.contains("<table"));
    }

    #[test]
    fn items_report_human_output_has_stable_header_and_rows() {
        let items = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "tool.observed".to_owned(),
            summary: "name=bash success=true".to_owned(),
            trace_id: None,
            agent_run_id: None,
            entities: Vec::new(),
            routing: None,
            limit_hits: None,
            binding_limit: None,
        }];

        let lines = render_items_lines(&items);

        assert_eq!(lines[0], "occurred_at\tkind\tsummary");
        assert!(lines[1].contains("\ttool.observed\tname=bash success=true"));
    }

    #[test]
    fn serve_defaults_remain_local_first() {
        let cli = Cli::try_parse_from(["noet", "serve"]).expect("serve args parse");

        match cli.command {
            Command::Serve(args) => {
                assert_eq!(args.bind.to_string(), "127.0.0.1:4040");
                assert_eq!(args.fixture_dir, PathBuf::from(".noet/fixtures"));
                assert_eq!(args.simulation_dir, PathBuf::from(".noet/simulations"));
                assert_eq!(args.db_path, PathBuf::from(".noet/noether.sqlite"));
                assert!(args.upstream.is_none());
                assert!(args.routes.is_none());
                assert!(args.policy.is_none());
                assert_eq!(args.decision_mode, DecisionMode::DryRun);
            }
            _ => panic!("expected serve command"),
        }
    }

    #[test]
    fn noet_process_name_accepts_deleted_linux_executable_suffix() {
        assert!(is_noet_executable_name("noet"));
        assert!(is_noet_executable_name("noet (deleted)"));
        assert!(!is_noet_executable_name("python"));
    }

    #[test]
    fn local_up_defaults_use_standard_noether_runtime() {
        let cli = Cli::try_parse_from(["noet", "local", "up"]).expect("local args parse");

        match cli.command {
            Command::Local(command) => match command.command {
                LocalSubcommand::Up(args) => {
                    assert_eq!(args.bind.to_string(), "127.0.0.1:4051");
                    assert_eq!(args.root, PathBuf::from("."));
                    assert!(args.upstream.is_none());
                    assert!(args.routes.is_none());
                    assert_eq!(args.decision_mode, DecisionMode::Enforce);
                }
                LocalSubcommand::Status(_) => panic!("expected local up command"),
            },
            _ => panic!("expected local command"),
        }
    }

    #[test]
    fn local_status_defaults_to_standard_runtime() {
        let cli = Cli::try_parse_from(["noet", "local", "status"]).expect("local status parses");

        match cli.command {
            Command::Local(command) => match command.command {
                LocalSubcommand::Status(args) => {
                    assert_eq!(args.root, PathBuf::from("."));
                }
                LocalSubcommand::Up(_) => panic!("expected local status command"),
            },
            _ => panic!("expected local command"),
        }
    }

    #[test]
    fn up_defaults_to_foreground_standard_noet_runtime() {
        let cli = Cli::try_parse_from(["noet", "up"]).expect("up args parse");

        match cli.command {
            Command::Up(args) => {
                assert!(args.root.is_none());
                assert!(args.bind.is_none());
                assert!(!args.detach);
                assert!(args.upstream.is_none());
                assert!(args.routes.is_none());
                assert!(args.decision_mode.is_none());
            }
            _ => panic!("expected up command"),
        }
    }

    #[test]
    fn up_supports_explicit_detach_and_overrides() {
        let cli = Cli::try_parse_from([
            "noet",
            "up",
            "--root",
            "/tmp/noet-root",
            "-d",
            "--bind",
            "127.0.0.1:4052",
            "--decision-mode",
            "dry-run",
        ])
        .expect("up args parse");

        match cli.command {
            Command::Up(args) => {
                assert_eq!(args.root, Some(PathBuf::from("/tmp/noet-root")));
                assert_eq!(args.bind.unwrap().to_string(), "127.0.0.1:4052");
                assert!(args.detach);
                assert_eq!(args.decision_mode, Some(DecisionMode::DryRun));
            }
            _ => panic!("expected up command"),
        }
    }

    #[test]
    fn config_subcommands_default_to_standard_noet_runtime() {
        let cli = Cli::try_parse_from(["noet", "config", "path"]).expect("config path parses");

        match cli.command {
            Command::Config(command) => match command.command {
                ConfigSubcommand::Path(args) => assert!(args.root.is_none()),
                _ => panic!("expected config path command"),
            },
            _ => panic!("expected config command"),
        }
    }

    #[test]
    fn logs_defaults_to_standard_noet_runtime() {
        let cli = Cli::try_parse_from(["noet", "logs"]).expect("logs parses");

        match cli.command {
            Command::Logs(args) => assert!(args.root.is_none()),
            _ => panic!("expected logs command"),
        }
    }

    #[test]
    fn open_defaults_to_policy_surface() {
        let cli = Cli::try_parse_from(["noet", "open"]).expect("open parses");

        match cli.command {
            Command::Open(args) => {
                assert!(args.root.is_none());
                assert_eq!(args.surface.path(), "/policy");
            }
            _ => panic!("expected open command"),
        }
    }

    #[tokio::test]
    async fn scenario_run_replays_a_file_into_reports_and_dashboard() {
        let tempdir = tempdir().expect("tempdir");
        let scenario_path = tempdir.path().join("local-dev.noet.yaml");
        let out_dir = tempdir.path().join("artifacts");
        std::fs::write(
            &scenario_path,
            r#"
version: 1
name: local developer
policy:
  version: 0
  budgets:
    - id: project-noether
      limits:
        spend:
          - id: budget-cap
            window: 30d
            mode: tumbling
            anchor:
              kind: first_seen
            max_usd: 10
            action: block
      match:
        project: noether
entities: [project:noether, user:alice]
requests:
  - id: req-1
    authorize:
      project: noether
      provider: openai
      model: gpt-4.1
      estimated_tokens: 1200
    model_choice:
      provider: openai
      model: gpt-4.1
    tool_activity:
      - name: bash
        success: true
        duration_ms: 42
    finalize:
      actual_cost_usd: 0.002
      usage:
        provider: openai
        model: gpt-4.1
        input_tokens: 700
        output_tokens: 300
        total_tokens: 1000
        cost_usd: 0.002
assertions:
  - kind: decision_outcome
    request_id: req-1
    outcome: allow
  - kind: selected_budget
    request_id: req-1
    budget_id: project-noether
  - kind: total_cost_usd
    amount_usd: 0.002
  - kind: report_json
    report: usage
    pointer: /rows/0/model
    equals: gpt-4.1
  - kind: report_contains
    report: decisions
    text: selected_budget=project-noether
  - kind: dashboard_contains
    text: name=bash success=true
"#,
        )
        .expect("write scenario");

        run_scenario(ScenarioCommand {
            command: ScenarioSubcommand::Run {
                path: scenario_path.clone(),
                out_dir: Some(out_dir.clone()),
            },
        })
        .await
        .expect("scenario run succeeds");

        let usage_report_path = out_dir.join("usage-report.json");
        let decisions_report_path = out_dir.join("decisions-report.json");
        let dashboard_path = out_dir.join("noether-dashboard.html");
        let trace_report_path = out_dir.join("traces").join("req-1.json");
        assert!(usage_report_path.exists());
        assert!(decisions_report_path.exists());
        assert!(dashboard_path.exists());
        assert!(trace_report_path.exists());

        let usage_report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&usage_report_path).expect("read usage report"))
                .expect("usage report json");
        assert_eq!(usage_report["total_cost_usd"], 0.002);
        assert_eq!(usage_report["rows"][0]["project"], "noether");
        assert_eq!(usage_report["rows"][0]["provider"], "openai");
        assert_eq!(usage_report["rows"][0]["model"], "gpt-4.1");

        let decisions_report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&decisions_report_path).expect("read decisions report"),
        )
        .expect("decisions report json");
        assert_eq!(decisions_report[0]["kind"], "decision.allow");
        assert!(
            decisions_report[0]["summary"]
                .as_str()
                .expect("decision summary")
                .contains("selected_budget=project-noether")
        );
        assert!(
            decisions_report[0]["summary"]
                .as_str()
                .expect("decision summary")
                .contains("request=req-1")
        );

        let trace_report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&trace_report_path).expect("read trace report"))
                .expect("trace report json");
        let kinds: Vec<&str> = trace_report["items"]
            .as_array()
            .expect("trace items")
            .iter()
            .filter_map(|item| item["kind"].as_str())
            .collect();
        assert!(kinds.contains(&"tool.observed"));
        assert!(kinds.contains(&"usage.finalized"));

        let dashboard = std::fs::read_to_string(&dashboard_path).expect("read dashboard");
        assert!(dashboard.contains("Noether run dashboard"));
        assert!(dashboard.contains("selected_budget=project-noether"));
        assert!(dashboard.contains("name=bash success=true"));
        assert!(!dashboard.contains("<table"));
    }

    #[tokio::test]
    async fn scenario_run_exports_explicit_window_metadata_in_decisions_report() {
        let tempdir = tempdir().expect("tempdir");
        let scenario_path = tempdir.path().join("window-metadata.noet.yaml");
        let out_dir = tempdir.path().join("artifacts");
        std::fs::write(
            &scenario_path,
            r#"
version: 1
name: explicit windows
policy:
  version: 0
  budgets:
    - id: project-noether
      match:
        project: noether
      limits:
        spend:
          - id: budget-cap
            window: 60s
            mode: tumbling
            anchor:
              kind: first_seen
            max_usd: 20
            action: block
          - id: daily-cap
            window: 1d
            mode: tumbling
            anchor:
              kind: first_seen
            max_usd: 10
            action: block
entities: [project:noether, user:alice]
requests:
  - id: req-1
    authorize:
      project: noether
      provider: openai
      model: gpt-4.1
      estimated_cost_usd: 6
  - id: req-2
    authorize:
      project: noether
      provider: openai
      model: gpt-4.1
      estimated_cost_usd: 5
    denial:
      rule_id: project-noether.spend_window.daily-cap
assertions:
  - kind: decision_outcome
    request_id: req-1
    outcome: allow
  - kind: denied
    request_id: req-2
  - kind: limit_hit
    request_id: req-2
    rule_id: project-noether.spend_window.daily-cap
  - kind: report_json
    report: decisions
    pointer: /0/limit_hits/0/window_mode
    equals: tumbling
  - kind: report_json
    report: decisions
    pointer: /1/routing/budget_window_mode
    equals: tumbling
"#,
        )
        .expect("write scenario");

        run_scenario(ScenarioCommand {
            command: ScenarioSubcommand::Run {
                path: scenario_path,
                out_dir: Some(out_dir.clone()),
            },
        })
        .await
        .expect("scenario run succeeds");

        let decisions_report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(out_dir.join("decisions-report.json")).expect("read decisions report"),
        )
        .expect("decisions report json");
        assert_eq!(
            decisions_report[0]["limit_hits"][0]["window_mode"],
            "tumbling"
        );
        assert_eq!(
            decisions_report[1]["routing"]["budget_window_mode"],
            "tumbling"
        );
    }

    #[tokio::test]
    async fn scenario_run_fails_when_assertions_drift() {
        let tempdir = tempdir().expect("tempdir");
        let scenario_path = tempdir.path().join("assertion-drift.noet.yaml");
        let out_dir = tempdir.path().join("artifacts");
        std::fs::write(
            &scenario_path,
            r#"
version: 1
name: drift
policy:
  version: 0
  budgets:
    - id: project-noether
      limits:
        spend:
          - id: budget-cap
            window: 30d
            mode: tumbling
            anchor:
              kind: first_seen
            max_usd: 10
            action: block
      match:
        project: noether
requests:
  - id: req-1
    authorize:
      project: noether
      provider: openai
      model: gpt-4.1
      estimated_tokens: 1200
    finalize:
      actual_cost_usd: 0.002
      usage:
        provider: openai
        model: gpt-4.1
        total_tokens: 1000
        cost_usd: 0.002
assertions:
  - kind: report_contains
    report: usage
    text: selected_budget=project-noether
"#,
        )
        .expect("write scenario");

        let error = run_scenario(ScenarioCommand {
            command: ScenarioSubcommand::Run {
                path: scenario_path,
                out_dir: Some(out_dir),
            },
        })
        .await
        .expect_err("scenario run should fail");

        assert!(error.to_string().contains("scenario assertions failed"));
        assert!(
            error
                .to_string()
                .contains("selected_budget=project-noether")
        );
    }

    #[tokio::test]
    async fn scenario_run_supports_fallback_denial_and_limit_hit_assertions() {
        let tempdir = tempdir().expect("tempdir");
        let scenario_path = tempdir.path().join("routing-and-limits.noet.yaml");
        let out_dir = tempdir.path().join("artifacts");
        std::fs::write(
            &scenario_path,
            r#"
version: 1
name: routing and limits
policy:
  version: 0
  budgets:
    - id: project-budget
      limits:
        spend:
          - id: budget-cap
            window: 30d
            mode: tumbling
            anchor:
              kind: first_seen
            max_usd: 10
            action: block
      match:
        project: noether
    - id: team-budget
      limits:
        spend:
          - id: budget-cap
            window: 30d
            mode: tumbling
            anchor:
              kind: first_seen
            max_usd: 20
            action: block
      match:
        team: eng
    - id: limit-budget
      match:
        project: guarded
      limits:
        spend:
          - id: budget-cap
            window: 30d
            mode: tumbling
            anchor:
              kind: first_seen
            max_usd: 5
            action: block
        context_tokens:
          max_tokens: 1000
          action: block
requests:
  - id: req-fallback
    authorize:
      budget_id: missing-budget
      project: noether
      provider: openai
      model: gpt-4.1
      estimated_cost_usd: 0.25
      entities: [project:noether, team:eng]
    finalize:
      actual_cost_usd: 0.25
      usage:
        provider: openai
        model: gpt-4.1
        total_tokens: 1000
        cost_usd: 0.25
    fallback:
      requested_budget_id: missing-budget
      selected_budget_id: project-budget
      matched_entity: project:noether
  - id: req-limit
    authorize:
      project: guarded
      provider: openai
      model: gpt-4.1
      estimated_tokens: 1200
      entities: [project:guarded]
    denial:
      rule_id: limit-budget.context_tokens
      reason_contains: exceed enforced limit max 1000
assertions:
  - kind: fallback
    request_id: req-fallback
    requested_budget_id: missing-budget
    selected_budget_id: project-budget
    matched_entity: project:noether
  - kind: denied
    request_id: req-limit
  - kind: limit_hit
    request_id: req-limit
    rule_id: limit-budget.context_tokens
"#,
        )
        .expect("write scenario");

        run_scenario(ScenarioCommand {
            command: ScenarioSubcommand::Run {
                path: scenario_path,
                out_dir: Some(out_dir.clone()),
            },
        })
        .await
        .expect("scenario run succeeds");

        let decisions_report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(out_dir.join("decisions-report.json")).expect("read decisions report"),
        )
        .expect("decisions report json");
        assert_eq!(
            decisions_report.as_array().expect("decision array").len(),
            2
        );
    }

    #[tokio::test]
    async fn checked_in_scenario_examples_replay_successfully() {
        let tempdir = tempdir().expect("tempdir");
        for example in [
            "examples/scenarios/local-developer.noet.yaml",
            "examples/scenarios/team-pooled-budget.noet.yaml",
            "examples/scenarios/project-budget-fallback.noet.yaml",
            "examples/scenarios/model-denial-fallback.noet.yaml",
            "examples/scenarios/runaway-agent-limit.noet.yaml",
            "examples/scenarios/protected-adoption-pool.noet.yaml",
        ] {
            let scenario_path = PathBuf::from(example);
            let out_dir = tempdir.path().join(
                scenario_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("scenario"),
            );

            run_scenario(ScenarioCommand {
                command: ScenarioSubcommand::Run {
                    path: scenario_path.clone(),
                    out_dir: Some(out_dir.clone()),
                },
            })
            .await
            .unwrap_or_else(|error| panic!("{} should replay: {}", scenario_path.display(), error));

            assert!(
                out_dir.join("usage-report.json").exists(),
                "{} missing usage report",
                scenario_path.display()
            );
            assert!(
                out_dir.join("decisions-report.json").exists(),
                "{} missing decisions report",
                scenario_path.display()
            );
            assert!(
                out_dir.join("noether-dashboard.html").exists(),
                "{} missing dashboard",
                scenario_path.display()
            );
        }
    }

    #[tokio::test]
    async fn scenario_run_disambiguates_colliding_request_ids() {
        let tempdir = tempdir().expect("tempdir");
        let scenario_path = tempdir.path().join("colliding-request-ids.noet.yaml");
        let out_dir = tempdir.path().join("artifacts");
        std::fs::write(
            &scenario_path,
            r#"
version: 1
policy:
  version: 0
  budgets:
    - id: project-noether
      limits:
        spend:
          - id: budget-cap
            window: 30d
            mode: tumbling
            anchor:
              kind: first_seen
            max_usd: 10
            action: block
      match:
        project: noether
requests:
  - id: req/1
    authorize:
      project: noether
      provider: openai
      model: gpt-4.1
      estimated_cost_usd: 0.001
      entities: [project:noether]
    finalize:
      actual_cost_usd: 0.001
      usage:
        provider: openai
        model: gpt-4.1
        total_tokens: 100
        cost_usd: 0.001
    assertions:
      - kind: decision_outcome
        request_id: req/1
        outcome: allow
      - kind: report_json
        report: trace
        pointer: /items/0/kind
        equals: decision.allow
  - id: req 1
    authorize:
      project: noether
      provider: openai
      model: gpt-4.1
      estimated_cost_usd: 0.001
      entities: [project:noether]
    finalize:
      actual_cost_usd: 0.001
      usage:
        provider: openai
        model: gpt-4.1
        total_tokens: 100
        cost_usd: 0.001
    assertions:
      - kind: decision_outcome
        request_id: req 1
        outcome: allow
      - kind: report_contains
        report: trace
        text: request=req 1
"#,
        )
        .expect("write scenario");

        run_scenario(ScenarioCommand {
            command: ScenarioSubcommand::Run {
                path: scenario_path,
                out_dir: Some(out_dir.clone()),
            },
        })
        .await
        .expect("scenario run succeeds");

        let trace_paths: Vec<_> = std::fs::read_dir(out_dir.join("traces"))
            .expect("read traces dir")
            .map(|entry| entry.expect("trace entry").file_name())
            .collect();
        assert_eq!(trace_paths.len(), 2);
        assert!(trace_paths.iter().any(|name| name == "req~2f1.json"));
        assert!(trace_paths.iter().any(|name| name == "req~201.json"));
    }

    #[tokio::test]
    async fn simulate_command_compares_checked_in_strategies() {
        let tempdir = tempdir().expect("tempdir");
        let out_dir = tempdir.path().join("simulation-output");

        run_simulate(SimulateCommand {
            path: PathBuf::from("examples/simulations/synthetic-company.noet.yaml"),
            out_dir: Some(out_dir.clone()),
            database_url: None,
            postgres_profile: "strict".to_owned(),
        })
        .await
        .expect("simulation run succeeds");

        let report_path = out_dir.join("simulation-report.json");
        let dashboard_path = out_dir.join("simulation-dashboard.html");
        assert!(report_path.exists());
        assert!(dashboard_path.exists());
        assert!(
            out_dir
                .join("strategies/pooled-caps/noether-dashboard.html")
                .exists()
        );
        assert!(
            out_dir
                .join("strategies/protected-adoption/noether-dashboard.html")
                .exists()
        );

        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&report_path).expect("read report"))
                .expect("report json");
        let dashboard = std::fs::read_to_string(&dashboard_path).expect("read dashboard");
        assert_eq!(report["total_requests"], 337);
        assert!(dashboard.contains("Comparison summary"));
        assert!(dashboard.contains("Protected opportunity"));
        assert!(dashboard.contains("Adoption policy changed what the team could see"));
        assert!(dashboard.contains(
            "protected-adoption surfaced $51.48 of unused protected opportunity across 2 low adopters and 1 high adopters."
        ));
        let strategies = report["strategies"].as_array().expect("strategies");
        assert_eq!(strategies.len(), 2);
        let pooled = strategies
            .iter()
            .find(|strategy| strategy["id"] == "pooled-caps")
            .expect("pooled strategy");
        let adoption = strategies
            .iter()
            .find(|strategy| strategy["id"] == "protected-adoption")
            .expect("protected adoption strategy");
        for strategy in [pooled, adoption] {
            assert_eq!(strategy["total_requests"], 337);
            assert_eq!(strategy["allowed_requests"], 337);
            assert_eq!(strategy["warned_requests"], 0);
            assert_eq!(strategy["denied_requests"], 0);
            assert_eq!(strategy["fallback_count"], 337);
            assert_eq!(strategy["limit_hit_count"], 0);
            assert_eq!(strategy["useful_work_blocked_score"], 0);
            assert_eq!(strategy["runaway_spend_prevented_usd"], 0.0);
            assert_eq!(strategy["adoption_coverage"], 1.0);
            assert_eq!(strategy["fairness_score"], 0.0);
            assert_eq!(strategy["carryover_liability_usd"], 0.0);
            assert!(strategy["exhaustion_day"].is_null());
            assert_eq!(strategy["total_cost_usd"], 33.552336);
            assert_eq!(strategy["unused_budget_usd"], 566.447664);

            let model_mix = strategy["model_mix"].as_array().expect("model mix");
            assert_eq!(model_mix.len(), 2);
            assert_eq!(model_mix[0]["model_id"], "flagship");
            assert_eq!(model_mix[0]["requests"], 242);
            assert_eq!(model_mix[0]["total_cost_usd"], 32.44564000000001_f64);
            assert_eq!(model_mix[1]["model_id"], "fast");
            assert_eq!(model_mix[1]["requests"], 95);
            assert_eq!(model_mix[1]["total_cost_usd"], 1.106696);
        }
        assert_eq!(pooled["unused_protected_opportunity_usd"], 0.0);
        assert_eq!(pooled["low_adopter_count"], 0);
        assert_eq!(pooled["high_adopter_count"], 0);
        let adoption_unused = adoption["unused_protected_opportunity_usd"]
            .as_f64()
            .expect("protected opportunity as f64");
        assert!((adoption_unused - 51.475908).abs() < 1e-9);
        assert_eq!(adoption["low_adopter_count"], 2);
        assert_eq!(adoption["high_adopter_count"], 1);
    }

    #[test]
    fn decision_headline_uses_binding_limit_without_claiming_warns_were_blocked() {
        let item = TraceReportItem {
            occurred_at: Utc::now(),
            kind: "decision.warn".to_owned(),
            summary: "decision_id=dec_warn model=openai/gpt-4.1 selected_budget=team-budget"
                .to_owned(),
            trace_id: None,
            agent_run_id: None,
            entities: Vec::new(),
            routing: Some(crate::ledger::DecisionRoutingReport {
                selected_budget_id: Some("team-budget".to_owned()),
                matched_entity: None,
                selection_reason: None,
                rejected_budget_id: None,
                rejected_budget_reason: None,
                model_check: None,
                budget_window_remaining_usd: Some(50.0),
                budget_window_mode: None,
                budget_window_started_at: None,
                budget_window_ends_at: None,
            }),
            limit_hits: Some(vec![crate::ledger::DecisionLimitHitReport {
                rule_id: "team-budget.spend_window.daily-cap".to_owned(),
                reason: "projected spend $11.000000 exceeds 1d limit max $10.000000".to_owned(),
                severity: crate::contract::DecisionSeverity::Warn,
                window_id: Some("daily-cap".to_owned()),
                window_mode: Some("tumbling".to_owned()),
                window_started_at: None,
                window_ends_at: None,
                projected_spend_usd: Some(11.0),
                max_usd: Some(10.0),
                scope_entity: Some("user:alice".to_owned()),
            }]),
            binding_limit: None,
        };

        assert_eq!(
            decision_headline(&item),
            "openai/gpt-4.1 continued on team-budget under daily-cap tumbling limit"
        );
        assert!(
            decision_supporting_line(&item)
                .expect("supporting line")
                .contains("Binding limit: daily-cap tumbling limit.")
        );
    }

    #[test]
    fn decision_headline_and_model_check_humanize_model_denials() {
        let item = TraceReportItem {
            occurred_at: Utc::now(),
            kind: "decision.deny".to_owned(),
            summary: "decision_id=dec_model trace=trace-model model=openai-codex/gpt-5.4-mini rejected_budget=personal-local model_check=denied".to_owned(),
            trace_id: Some("trace-model".to_owned()),
            agent_run_id: None,
                entities: vec!["project:noether".to_owned()],
            routing: Some(crate::ledger::DecisionRoutingReport {
                selected_budget_id: None,
                matched_entity: None,
                selection_reason: None,
                rejected_budget_id: Some("personal-local".to_owned()),
                rejected_budget_reason: Some(
                    "requested provider/model is not allowed by requested budget".to_owned(),
                ),
                model_check: Some("denied".to_owned()),
                budget_window_remaining_usd: None,
                budget_window_mode: None,
                budget_window_started_at: None,
                budget_window_ends_at: None,
            }),
            limit_hits: None,
            binding_limit: None,
        };

        assert_eq!(
            decision_headline(&item),
            "openai-codex/gpt-5.4-mini was blocked by personal-local's model allowlist"
        );
        assert_eq!(
            decision_model_check_label(&item).as_deref(),
            Some("blocked by model allowlist")
        );
        assert!(
            decision_supporting_line(&item)
                .expect("supporting line")
                .contains("Attempted model openai-codex/gpt-5.4-mini is not allowed on budget personal-local.")
        );
    }
}
