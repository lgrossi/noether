use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::Value;
use tokio::fs;

use crate::contract::{AuthorizeDecision, DecisionMode, FinalizeReservation, TraceEvent};
use crate::error::NoetError;
use crate::fixture::{list_fixture_paths, read_fixture};
use crate::ledger::{
    AsyncPostgresLedgerOptions, BudgetLedger, TraceReport, TraceReportItem, UsageReport,
};
use crate::local::{
    DEFAULT_LOCAL_BIND, ensure_local_runtime_layout, read_local_sidecar_owner,
    write_local_sidecar_owner,
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
use crate::simulation::{SimulationFile, compare_strategies, validate_simulation};

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
    /// Run Noether with the standard local `.noether/` runtime layout.
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

    /// PostgreSQL durability/latency profile: strict or performance.
    #[arg(long, env = "NOET_POSTGRES_PROFILE", default_value = "strict")]
    postgres_profile: String,

    /// Number of async PostgreSQL connections to use for hot-path writes.
    #[arg(long, env = "NOET_POSTGRES_POOL_SIZE", default_value_t = 4)]
    postgres_pool_size: usize,

    /// Override whether PostgreSQL finalization persistence is queued after updating in-memory state.
    #[arg(
        long,
        env = "NOET_POSTGRES_ASYNC_FINALIZE",
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    postgres_async_finalize: Option<bool>,

    /// Bounded queue size for async PostgreSQL finalize persistence.
    #[arg(
        long,
        env = "NOET_POSTGRES_FINALIZE_QUEUE_CAPACITY",
        default_value_t = 1024
    )]
    postgres_finalize_queue_capacity: usize,

    /// Per-connection PostgreSQL synchronous_commit setting: on, off, local, remote_write, remote_apply.
    #[arg(long, env = "NOET_POSTGRES_SYNCHRONOUS_COMMIT")]
    postgres_synchronous_commit: Option<String>,

    /// Emit debug logs with PostgreSQL hot-path stage timings.
    #[arg(long, env = "NOET_POSTGRES_STAGE_TIMING", default_value_t = false)]
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
struct LocalCommand {
    #[command(subcommand)]
    command: LocalSubcommand,
}

#[derive(Subcommand)]
enum LocalSubcommand {
    /// Start the local sidecar with repo-local `.noether/` defaults.
    Up(LocalUpArgs),
    /// Print repo-local sidecar owner state.
    Status(LocalStatusArgs),
}

#[derive(Parser)]
struct LocalUpArgs {
    /// Repo root that should contain the `.noether/` runtime home.
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
    /// Repo root that should contain the `.noether/` runtime home.
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
}

pub async fn run() -> Result<(), NoetError> {
    let cli = Cli::parse();

    match cli.command {
        Command::Serve(args) => {
            let policy_path = args.policy.clone();
            let postgres_options = postgres_options_from_serve_args(&args)?;
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
                decision_mode: args.decision_mode,
            })
            .await
        }
        Command::Local(command) => run_local(command).await,
        Command::Policy(command) => run_policy(command).await,
        Command::Fixtures(command) => run_fixtures(command).await,
        Command::Report(command) => run_report(command).await,
        Command::Scenario(command) => run_scenario(command).await,
        Command::Simulate(command) => run_simulate(command).await,
    }
}

fn postgres_options_from_serve_args(
    args: &ServeArgs,
) -> Result<AsyncPostgresLedgerOptions, NoetError> {
    let mut options = AsyncPostgresLedgerOptions::from_profile(&args.postgres_profile)?;
    options.pool_size = args.postgres_pool_size.max(1);
    if let Some(async_finalize) = args.postgres_async_finalize {
        options.async_finalize = async_finalize;
    }
    options.finalize_queue_capacity = args.postgres_finalize_queue_capacity.max(1);
    if let Some(synchronous_commit) = args.postgres_synchronous_commit.clone() {
        options.synchronous_commit = Some(synchronous_commit);
    }
    if args.postgres_stage_timing {
        options.stage_timing = true;
    }
    Ok(options)
}

async fn run_local(command: LocalCommand) -> Result<(), NoetError> {
    match command.command {
        LocalSubcommand::Up(args) => {
            let layout = ensure_local_runtime_layout(&args.root).await?;
            write_local_sidecar_owner(&layout, &args.bind.to_string()).await?;
            let policy = load_policy(&layout.policy_path).await?;
            let routes = match args.routes {
                Some(path) => load_proxy_routes(&path).await?.routes,
                None => Vec::new(),
            };
            serve(ServeConfig {
                bind: args.bind,
                fixture_dir: layout.fixture_dir,
                simulation_dir: layout.simulation_dir,
                db_path: layout.db_path,
                database_url: None,
                postgres_options: AsyncPostgresLedgerOptions::default(),
                upstream: args.upstream,
                routes,
                policy_path: Some(layout.policy_path),
                policy: Some(policy),
                decision_mode: args.decision_mode,
            })
            .await
        }
        LocalSubcommand::Status(args) => {
            match read_local_sidecar_owner(&args.root).await? {
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
    }
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
    let out_dir = command.out_dir.unwrap_or_else(|| {
        PathBuf::from(".noet/simulations").join(simulation_output_slug(&command.path))
    });
    let report = compare_strategies(&simulation, &out_dir)?;
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
    for strategy in &report.strategies {
        let strategy_db_path = out_dir.join(&strategy.db_path);
        let strategy_usage_report_path = out_dir.join(&strategy.usage_report_path);
        let strategy_decisions_report_path = out_dir.join(&strategy.decisions_report_path);
        let strategy_dashboard_path = strategy_db_path
            .parent()
            .unwrap_or(&out_dir)
            .join("noether-dashboard.html");
        let ledger = BudgetLedger::open_sqlite(&strategy_db_path)?;
        let usage = ledger.usage_report()?;
        let decisions = ledger.decisions_report()?;
        fs::write(
            &strategy_dashboard_path,
            render_dashboard(&usage, &decisions, None, &[]),
        )
        .await?;
        println!("strategy\t{}", strategy.id);
        println!("db_path\t{}", strategy_db_path.display());
        println!("usage_report\t{}", strategy_usage_report_path.display());
        println!(
            "decisions_report\t{}",
            strategy_decisions_report_path.display()
        );
        println!("dashboard\t{}", strategy_dashboard_path.display());
    }
    Ok(())
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

    let db_path = output_dir.join("noether.sqlite");
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

    for assertion in &scenario.assertions {
        evaluate_scenario_assertion(
            assertion,
            None,
            &decision_reports,
            &usage_json,
            &decisions_json,
            trace_reports,
            &usage_text,
            &decisions_text,
            &trace_texts,
            dashboard,
            &mut failures,
        );
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
                &decision_reports,
                &usage_json,
                &decisions_json,
                trace_reports,
                &usage_text,
                &decisions_text,
                &trace_texts,
                dashboard,
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

fn evaluate_scenario_assertion(
    assertion: &ScenarioAssertion,
    default_request_id: Option<&str>,
    decision_reports: &BTreeMap<String, &TraceReportItem>,
    usage_json: &Value,
    decisions_json: &Value,
    trace_reports: &BTreeMap<String, TraceReport>,
    usage_text: &str,
    decisions_text: &str,
    trace_texts: &BTreeMap<String, String>,
    dashboard: &str,
    failures: &mut Vec<String>,
) {
    match assertion {
        ScenarioAssertion::DecisionOutcome {
            request_id,
            outcome,
        } => match decision_reports.get(request_id.as_str()) {
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
        } => match decision_reports.get(request_id.as_str()) {
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
        ScenarioAssertion::Denied { request_id } => match decision_reports.get(request_id.as_str())
        {
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
        },
        ScenarioAssertion::TotalCostUsd { amount_usd } => {
            if (usage_json["total_cost_usd"].as_f64().unwrap_or_default() - amount_usd).abs() > 1e-9
            {
                failures.push(format!(
                    "expected total_cost_usd {:.6} but saw {:.6}",
                    amount_usd,
                    usage_json["total_cost_usd"].as_f64().unwrap_or_default()
                ));
            }
        }
        ScenarioAssertion::LimitHit {
            request_id,
            rule_id,
        } => match decision_reports.get(request_id.as_str()) {
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
            decision_reports,
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
                usage_json,
                decisions_json,
                trace_reports,
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
                usage_text,
                decisions_text,
                trace_texts,
            )
            .is_some_and(|report_text| report_text.contains(text))
            {
                failures.push(format!("report output {:?} missing {text}", report));
            }
        }
        ScenarioAssertion::DashboardContains { text } => {
            if !dashboard.contains(text) {
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

fn render_dashboard(
    usage: &UsageReport,
    decisions: &[TraceReportItem],
    trace: Option<&TraceReport>,
    observations: &[TraceReportItem],
) -> String {
    let totals = usage_totals(usage);
    let latest_decision = decisions.first();
    let activity = dashboard_activity(trace, observations);
    let decision_stats = decision_stats(decisions);
    let tool_count = activity
        .iter()
        .filter(|item| is_tool_kind(&item.kind))
        .count();
    let agent_count = activity
        .iter()
        .filter(|item| is_agent_kind(&item.kind))
        .count();
    let skill_context_count = activity
        .iter()
        .filter(|item| is_skill_context_kind(&item.kind))
        .count();
    let lifecycle_limits = trace
        .map(|trace| {
            trace
                .items
                .iter()
                .filter(|item| item.kind.starts_with("limit.report_only."))
                .count()
        })
        .unwrap_or_default();
    let token_hint = token_hint(&totals);
    let latest_decision_hint = latest_decision
        .map(latest_decision_hint)
        .unwrap_or_else(|| "no authorization decisions yet".to_owned());
    let (story_title, story_lead, story_points) = run_story(
        usage,
        decisions,
        &decision_stats,
        &activity,
        latest_decision,
    );

    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether dashboard</title>");
    html.push_str(dashboard_styles());
    html.push_str("</head><body><main>");
    html.push_str("<h1>Noether run dashboard</h1>");
    html.push_str("<div class=\"sub\">Readable local view of decisions, cost, usage, and trace events. Raw hook logs are not needed here.</div>");

    html.push_str("<section class=\"overview\">");
    let _ = write!(
        html,
        "<article class=\"panel story\"><div class=\"eyebrow\">Outcome summary</div><h2>{}</h2><p class=\"lead\">{}</p>",
        escape_html(&story_title),
        escape_html(&story_lead)
    );
    if !story_points.is_empty() {
        html.push_str("<ul class=\"insight-list\">");
        for point in story_points {
            let _ = write!(html, "<li>{}</li>", escape_html(&point));
        }
        html.push_str("</ul>");
    }
    html.push_str("</article>");

    html.push_str("<section class=\"metric-grid\">");
    metric_card(
        &mut html,
        "Finalized spend",
        &format_money(usage.total_cost_usd),
        "what actually landed in the local ledger",
    );
    metric_card(
        &mut html,
        "Tokens",
        &compact_number(totals.total_tokens),
        &token_hint,
    );
    metric_card(
        &mut html,
        "Latest decision",
        latest_decision
            .map(|item| decision_label(&item.kind))
            .unwrap_or("none"),
        &latest_decision_hint,
    );
    metric_card(
        &mut html,
        "Decision mix",
        &format!(
            "{} allow · {} warn · {} deny",
            decision_stats.allow, decision_stats.warn, decision_stats.deny
        ),
        "how often policy allowed, warned, or blocked work",
    );
    if tool_count > 0 || agent_count > 0 || skill_context_count > 0 {
        metric_card(
            &mut html,
            "Run evidence",
            &format!(
                "{} tools · {} agent · {} context",
                tool_count, agent_count, skill_context_count
            ),
            "activity surfaced alongside decisions and budget outcomes",
        );
    } else {
        metric_card(
            &mut html,
            "Visible spend rows",
            &usage.rows.len().to_string(),
            "finalized usage rows that explain where cost landed",
        );
    }
    if let Some(adoption) = &usage.protected_adoption {
        metric_card(
            &mut html,
            "Protected opportunity",
            &format_money(adoption.unused_protected_opportunity_usd),
            "unused current protected grant this window",
        );
        metric_card(
            &mut html,
            "Adoption health",
            &format!(
                "{} low / {} high",
                adoption.low_adopters.len(),
                adoption.high_adopters.len()
            ),
            "simple view of underuse versus heavy protected-budget use",
        );
    } else {
        metric_card(
            &mut html,
            "Limit hits",
            &decision_stats.limit_hits.to_string(),
            "budget limits that fired across recent decisions",
        );
    }
    html.push_str("</section>");
    html.push_str("</section>");

    let has_policy_story =
        !decisions.is_empty() || decision_stats.limit_hits > 0 || lifecycle_limits > 0;
    let has_spend_breakdown = usage.rows.iter().any(|row| row.total_cost_usd > 0.0);
    let has_spend_story = has_spend_breakdown
        || totals.total_tokens > 0
        || !usage.rows.is_empty()
        || usage.protected_adoption.is_some();
    let has_run_evidence = trace.is_some()
        || !observations.is_empty()
        || tool_count > 0
        || agent_count > 0
        || skill_context_count > 0;

    if has_policy_story {
        html.push_str("<section class=\"section-block\">");
        section_header(
            &mut html,
            "Policy",
            "Policy decisions",
            "This section shows how Noether routed work, what it blocked, and the policy evidence behind each outcome.",
        );
        if !decisions.is_empty() {
            html.push_str("<section class=\"split\">");
            decision_flow_panel(&mut html, &decision_stats);
            decisions_panel(&mut html, decisions);
            html.push_str("</section>");
            budget_routing_panel(&mut html, decisions);
        }
        if decision_stats.limit_hits > 0 || lifecycle_limits > 0 {
            html.push_str("<section class=\"split\">");
            if decision_stats.limit_hits > 0 {
                risky_runs_panel(&mut html, decisions);
            }
            if lifecycle_limits > 0 {
                lifecycle_limits_panel(&mut html, trace);
            }
            html.push_str("</section>");
        }
        html.push_str("</section>");
    }

    if has_spend_story {
        html.push_str("<section class=\"section-block\">");
        section_header(
            &mut html,
            "Spend",
            "Spend and adoption",
            "Visual-first cost and adoption views show where finalized usage landed and who still has room to use protected budget.",
        );
        if has_spend_breakdown || totals.total_tokens > 0 {
            html.push_str("<section class=\"split\">");
            if has_spend_breakdown {
                spend_breakdown_panel(&mut html, usage);
            }
            if totals.total_tokens > 0 {
                token_mix_panel(&mut html, &totals);
            }
            html.push_str("</section>");
        }
        if usage.protected_adoption.is_some() {
            adoption_snapshot_panel(&mut html, usage);
            protected_adoption_panel(&mut html, usage);
        }
        if !usage.rows.is_empty() {
            usage_rows_panel(&mut html, usage);
        }
        html.push_str("</section>");
    }

    if has_run_evidence {
        html.push_str("<section class=\"section-block\">");
        section_header(
            &mut html,
            "Evidence",
            "Run evidence",
            "Trace events, tool activity, and agent lifecycle signals explain how the run unfolded without exposing raw prompt logs.",
        );
        html.push_str("<section class=\"split\">");
        if trace.is_some() || !observations.is_empty() {
            timeline_panel(&mut html, trace, observations);
        }
        if tool_count > 0 || agent_count > 0 || skill_context_count > 0 {
            html.push_str("<div class=\"stack-panels\">");
            if tool_count > 0 {
                tools_panel(&mut html, &activity);
            }
            if agent_count > 0 {
                agent_activity_panel(&mut html, &activity);
            }
            if skill_context_count > 0 {
                skill_context_panel(&mut html, &activity);
            }
            html.push_str("</div>");
        }
        html.push_str("</section>");
        html.push_str("</section>");
    }

    html.push_str("</main></body></html>");
    html
}

fn render_simulation_dashboard(report: &crate::simulation::SimulationComparisonReport) -> String {
    let title = report.name.as_deref().unwrap_or("Simulation comparison");
    let (story_title, story_lead, story_points) = simulation_story(report);
    let spend_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.total_cost_usd,
                format_money(strategy.total_cost_usd),
            )
        })
        .collect();
    let denied_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.denied_requests as f64,
                compact_number(strategy.denied_requests),
            )
        })
        .collect();
    let runaway_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.runaway_spend_prevented_usd,
                format_money(strategy.runaway_spend_prevented_usd),
            )
        })
        .collect();
    let adoption_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.unused_protected_opportunity_usd,
                format_money(strategy.unused_protected_opportunity_usd),
            )
        })
        .collect();
    let fairness_values: Vec<(String, f64, String)> = report
        .strategies
        .iter()
        .map(|strategy| {
            (
                strategy.id.clone(),
                strategy.fairness_score,
                format!("{:.2}", strategy.fairness_score),
            )
        })
        .collect();
    let highest_spend = report
        .strategies
        .iter()
        .map(|strategy| strategy.total_cost_usd)
        .fold(0.0_f64, f64::max);
    let max_runaway_prevented = report
        .strategies
        .iter()
        .map(|strategy| strategy.runaway_spend_prevented_usd)
        .fold(0.0_f64, f64::max);
    let max_protected_opportunity = report
        .strategies
        .iter()
        .map(|strategy| strategy.unused_protected_opportunity_usd)
        .fold(0.0_f64, f64::max);

    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether dashboard</title>");
    html.push_str(dashboard_styles());
    html.push_str("</head><body><main>");
    html.push_str("<h1>Noether dashboard</h1>");
    let _ = write!(
        html,
        "<div class=\"sub\">Comparison view · {} · seed <code>{}</code> over {} simulated day(s).</div>",
        escape_html(title),
        report.seed,
        report.horizon_days
    );

    html.push_str("<section class=\"hero\">");
    let _ = write!(
        html,
        "<article class=\"panel story\"><div class=\"eyebrow\">Comparison summary</div><h2>{}</h2><p class=\"lead\">{}</p>",
        escape_html(&story_title),
        escape_html(&story_lead)
    );
    if !story_points.is_empty() {
        html.push_str("<ul class=\"insight-list\">");
        for point in story_points {
            let _ = write!(html, "<li>{}</li>", escape_html(&point));
        }
        html.push_str("</ul>");
    }
    html.push_str("</article>");

    html.push_str("<section class=\"grid\">");
    metric_card(
        &mut html,
        "Strategies",
        &report.strategies.len().to_string(),
        "policy variants compared over identical demand",
    );
    metric_card(
        &mut html,
        "Total requests",
        &compact_number(report.total_requests),
        "synthetic authorize/finalize opportunities",
    );
    metric_card(
        &mut html,
        "Highest spend",
        &format_money(highest_spend),
        "largest simulated finalized cost among strategies",
    );
    if max_runaway_prevented > 0.0 {
        metric_card(
            &mut html,
            "Runaway prevented",
            &format_money(max_runaway_prevented),
            "best budget-limit outcome across compared strategies",
        );
    } else if max_protected_opportunity > 0.0 {
        metric_card(
            &mut html,
            "Protected opportunity",
            &format_money(max_protected_opportunity),
            "unused adoption budget surfaced by the strongest strategy",
        );
    } else {
        metric_card(
            &mut html,
            "Best fairness",
            &format!(
                "{:.2}",
                report
                    .strategies
                    .iter()
                    .map(|strategy| strategy.fairness_score)
                    .fold(0.0_f64, f64::max)
            ),
            "highest fairness score across compared strategies",
        );
    }
    html.push_str("</section>");
    html.push_str("</section>");

    strategy_scorecards_panel(&mut html, report);

    html.push_str("<section class=\"panel\"><h2>Strategy comparison</h2><p class=\"summary\">These comparisons use the same simulated demand. The bars make the tradeoffs visible before the evidence table.</p>");
    metric_compare_block(
        &mut html,
        "Finalized spend",
        "How much budget actually landed.",
        &spend_values,
        ComparisonEmphasis::Neutral,
    );
    metric_compare_block(
        &mut html,
        "Denied requests",
        "How restrictive each strategy became.",
        &denied_values,
        ComparisonEmphasis::Neutral,
    );
    if runaway_values.iter().any(|(_, value, _)| *value > 0.0) {
        metric_compare_block(
            &mut html,
            "Runaway prevented",
            "Higher means the strategy intercepted more risky spend before it landed.",
            &runaway_values,
            ComparisonEmphasis::HigherBetter,
        );
    }
    if adoption_values.iter().any(|(_, value, _)| *value > 0.0) {
        metric_compare_block(
            &mut html,
            "Protected opportunity",
            "Higher means the strategy surfaced more explicit room for low adopters.",
            &adoption_values,
            ComparisonEmphasis::HigherBetter,
        );
    }
    metric_compare_block(
        &mut html,
        "Fairness score",
        "Higher means spend was distributed more evenly across the simulated users.",
        &fairness_values,
        ComparisonEmphasis::HigherBetter,
    );
    html.push_str("</section>");

    simulation_evidence_table(&mut html, report);
    html.push_str("<section class=\"panel\"><h2>Model mix</h2><div class=\"table-wrap\"><table><thead><tr><th>Strategy</th><th>Model</th><th>Requests</th><th>Cost</th></tr></thead><tbody>");
    for strategy in &report.strategies {
        for mix in &strategy.model_mix {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&strategy.id),
                escape_html(&mix.model_id),
                mix.requests,
                format_money(mix.total_cost_usd)
            );
        }
    }
    html.push_str("</tbody></table></div></section>");
    html.push_str("</main></body></html>");
    html
}

#[derive(Default)]
struct UsageTotals {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
    reservations: u64,
    active_reservations: u64,
    finalized_reservations: u64,
}

fn usage_totals(usage: &UsageReport) -> UsageTotals {
    let mut totals = UsageTotals::default();
    for row in &usage.rows {
        totals.input_tokens += row.input_tokens;
        totals.output_tokens += row.output_tokens;
        totals.cache_read_tokens += row.cache_read_tokens;
        totals.cache_write_tokens += row.cache_write_tokens;
        totals.total_tokens += row.total_tokens;
        totals.reservations += row.reservations;
        totals.active_reservations += row.active_reservations;
        totals.finalized_reservations += row.finalized_reservations;
    }
    totals
}

fn dashboard_activity<'a>(
    trace: Option<&'a TraceReport>,
    observations: &'a [TraceReportItem],
) -> Vec<&'a TraceReportItem> {
    trace
        .map(|trace| trace.items.iter().collect())
        .unwrap_or_else(|| observations.iter().collect())
}

#[derive(Default)]
struct DecisionStats {
    allow: u64,
    warn: u64,
    deny: u64,
    limit_hits: u64,
}

impl DecisionStats {
    fn total(&self) -> u64 {
        self.allow + self.warn + self.deny
    }
}

enum ComparisonEmphasis {
    Neutral,
    HigherBetter,
}

fn dashboard_styles() -> &'static str {
    r#"<style>
        :root { color-scheme: dark; --bg:#0f172a; --panel:#111c33; --muted:#94a3b8; --text:#e5edf7; --line:#263449; --good:#22c55e; --warn:#f59e0b; --bad:#ef4444; --blue:#38bdf8; --violet:#a78bfa; --slate:#64748b; }
        * { box-sizing: border-box; }
        body { margin:0; font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; background:radial-gradient(circle at top left,#172554,#0f172a 42%); color:var(--text); }
        main { max-width:1180px; margin:0 auto; padding:32px 20px 48px; }
        h1 { margin:0 0 4px; font-size:34px; letter-spacing:-0.04em; }
        h2 { margin:0 0 12px; font-size:24px; letter-spacing:-0.03em; }
        h3 { margin:20px 0 10px; font-size:16px; }
        code { color:var(--blue); }
        .sub, .summary, .hint { color:var(--muted); }
        .sub { margin-bottom:24px; }
        .overview { display:grid; gap:14px; grid-template-columns:1fr; align-items:start; margin-bottom:14px; }
        .metric-grid { display:grid; gap:14px; grid-template-columns:repeat(3,minmax(0,1fr)); align-content:start; }
        .split { display:grid; gap:14px; grid-template-columns:repeat(2,minmax(0,1fr)); align-items:start; }
        .stack-panels { display:grid; gap:14px; }
        .section-block { margin-top:28px; }
        .section-header { margin:0 0 14px; }
        .section-name { font-size:24px; font-weight:800; letter-spacing:-0.03em; color:#f8fbff; }
        .section-header .summary { margin:4px 0 0; max-width:72ch; }
        .story { padding:22px; }
        .eyebrow, .label { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .lead { font-size:18px; margin:0; color:#dbe7f4; }
        .grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(210px,1fr)); }
        .card, .panel { background:rgba(17,28,51,.88); border:1px solid var(--line); border-radius:18px; box-shadow:0 18px 55px rgba(0,0,0,.22); }
        .card { padding:18px; }
        .panel { padding:18px; margin-top:14px; overflow:hidden; }
        .overview > .panel, .overview > .metric-grid > .card, .overview > .metric-grid > .panel, .split > .panel, .split > .stack-panels { margin-top:0; }
        .value { font-size:30px; font-weight:800; margin-top:6px; letter-spacing:-0.03em; }
        .value.small { font-size:24px; }
        .insight-list { margin:14px 0 0 18px; padding:0; }
        .insight-list li { margin:6px 0; }
        .bar { height:12px; display:flex; overflow:hidden; border-radius:999px; background:#1e293b; margin:10px 0; }
        .track { height:12px; width:100%; border-radius:999px; background:#1e293b; overflow:hidden; }
        .fill { height:100%; border-radius:999px; }
        .fill.good, .dot.good, .segment.good { background:var(--good); }
        .fill.warn, .dot.warn, .segment.warn { background:var(--warn); }
        .fill.bad, .dot.bad, .segment.bad { background:var(--bad); }
        .fill.blue, .dot.blue, .segment.blue, .in { background:var(--blue); }
        .fill.violet, .dot.violet, .segment.violet, .out { background:var(--violet); }
        .fill.slate, .dot.slate, .segment.slate, .cache { background:var(--slate); }
        .legend { display:flex; gap:16px; flex-wrap:wrap; color:var(--muted); font-size:13px; }
        .dot { display:inline-block; width:9px; height:9px; border-radius:50%; margin-right:6px; }
        .table-wrap { overflow:auto; }
        table { width:100%; border-collapse:collapse; }
        th, td { text-align:left; padding:10px 8px; border-top:1px solid var(--line); vertical-align:top; }
        th { color:var(--muted); font-weight:600; font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .pill { display:inline-flex; align-items:center; border-radius:999px; padding:4px 9px; background:#1e293b; border:1px solid var(--line); font-size:13px; }
        .meta-pill { display:inline-flex; align-items:center; gap:6px; border-radius:999px; padding:4px 10px; background:rgba(30,41,59,.8); border:1px solid rgba(148,163,184,.18); color:#dbe7f4; font-size:12px; }
        .ok { color:var(--good); } .warn { color:var(--warn); } .bad { color:var(--bad); }
        .compare-group { margin-top:18px; }
        .compare-title { margin-bottom:4px; font-weight:700; }
        .compare-row { display:grid; grid-template-columns:minmax(0,220px) minmax(0,1fr) auto; gap:12px; align-items:center; padding:10px 0; border-top:1px solid var(--line); }
        .compare-row:first-of-type { border-top:0; }
        .compare-label strong { display:block; }
        .metric-value { font-weight:700; white-space:nowrap; }
        .score-grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(250px,1fr)); }
        .score-list { list-style:none; margin:12px 0 0; padding:0; }
        .score-list li { margin:6px 0; color:var(--muted); }
        .section-intro { margin-top:4px; color:var(--muted); }
        details.evidence { margin-top:8px; }
        details.evidence summary { cursor:pointer; color:var(--muted); }
        .entry-list { display:grid; gap:12px; }
        .entry-card { padding:16px; border-radius:16px; border:1px solid rgba(148,163,184,.15); background:rgba(15,23,42,.45); }
        .entry-top { display:flex; justify-content:space-between; gap:12px; align-items:flex-start; flex-wrap:wrap; }
        .entry-title { margin-top:8px; font-size:18px; font-weight:700; color:#eef6ff; letter-spacing:-0.02em; }
        .meta-row { display:flex; gap:8px; flex-wrap:wrap; margin-top:10px; }
        .fact-grid { display:grid; gap:10px; grid-template-columns:repeat(auto-fit,minmax(140px,1fr)); margin-top:12px; }
        .fact { padding:10px 12px; border-radius:12px; border:1px solid rgba(148,163,184,.12); background:rgba(30,41,59,.45); }
        .fact-label { display:block; margin-bottom:3px; color:var(--muted); font-size:11px; text-transform:uppercase; letter-spacing:.08em; }
        .fact-value { color:#f8fbff; font-weight:700; }
        .entity-grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(250px,1fr)); }
        .entity-card { padding:18px; border-radius:16px; border:1px solid rgba(148,163,184,.15); background:rgba(15,23,42,.45); }
        .entity-card.accent-good { box-shadow:inset 0 0 0 1px rgba(34,197,94,.18); }
        .entity-card.accent-violet { box-shadow:inset 0 0 0 1px rgba(167,139,250,.18); }
        .inline-stats { display:flex; gap:14px; flex-wrap:wrap; margin-top:10px; color:var(--muted); font-size:13px; }
        .timeline { list-style:none; margin:0; padding:0; }
        .event { display:grid; grid-template-columns:165px 210px 1fr; gap:12px; padding:13px 0; border-top:1px solid var(--line); align-items:start; }
        .event:first-child { border-top:0; }
        .time { color:var(--muted); }
        .kind { font-weight:700; }
        .stack { height:14px; display:flex; border-radius:999px; overflow:hidden; background:#1e293b; margin:12px 0; }
        @media (max-width:1100px) { .metric-grid { grid-template-columns:repeat(2,minmax(0,1fr)); } }
        @media (max-width:900px) { .overview, .split, .metric-grid { grid-template-columns:1fr; } }
        @media (max-width:760px) { .event, .compare-row { grid-template-columns:1fr; gap:6px; } h1 { font-size:28px; } .section-name { font-size:22px; } }
        </style>"#
}

fn section_header(html: &mut String, eyebrow: &str, title: &str, summary: &str) {
    let _ = write!(
        html,
        "<div class=\"section-header\"><div class=\"eyebrow\">{}</div><div class=\"section-name\">{}</div><p class=\"summary\">{}</p></div>",
        escape_html(eyebrow),
        escape_html(title),
        escape_html(summary)
    );
}

fn fact_block(html: &mut String, label: &str, value: &str) {
    let _ = write!(
        html,
        "<div class=\"fact\"><span class=\"fact-label\">{}</span><span class=\"fact-value\">{}</span></div>",
        escape_html(label),
        escape_html(value)
    );
}

fn decision_stats(decisions: &[TraceReportItem]) -> DecisionStats {
    let mut stats = DecisionStats::default();
    for item in decisions {
        if item.kind.ends_with(".deny") {
            stats.deny += 1;
        } else if item.kind.ends_with(".warn") {
            stats.warn += 1;
        } else if item.kind.ends_with(".allow") {
            stats.allow += 1;
        }
        stats.limit_hits += item
            .limit_hits
            .as_ref()
            .map(|hits| hits.len() as u64)
            .unwrap_or(0);
    }
    stats
}

fn run_story(
    usage: &UsageReport,
    decisions: &[TraceReportItem],
    stats: &DecisionStats,
    activity: &[&TraceReportItem],
    latest_decision: Option<&TraceReportItem>,
) -> (String, String, Vec<String>) {
    let title = if stats.deny > 0 && stats.limit_hits > 0 {
        "Risky spend was blocked before it landed".to_owned()
    } else if usage.total_cost_usd > 0.0 {
        format!("This run finalized {}", format_money(usage.total_cost_usd))
    } else if stats.allow + stats.warn > 0 {
        "Work was authorized, but no finalized spend landed yet".to_owned()
    } else {
        "Noether is waiting for meaningful run evidence".to_owned()
    };
    let lead = if stats.deny > 0 && stats.limit_hits > 0 {
        format!(
            "{} request(s) were denied and {} limit hit(s) fired. Finalized spend stayed at {}.",
            stats.deny,
            stats.limit_hits,
            format_money(usage.total_cost_usd)
        )
    } else if usage.total_cost_usd > 0.0 {
        format!(
            "{} decision(s) produced {} finalized reservation(s) across {} visible spend row(s).",
            stats.total(),
            usage
                .rows
                .iter()
                .map(|row| row.finalized_reservations)
                .sum::<u64>(),
            usage.rows.len()
        )
    } else if let Some(item) = latest_decision {
        format!("Latest decision: {}.", item.summary)
    } else {
        "No authorization decisions, trace events, or finalized usage have been captured yet."
            .to_owned()
    };

    let mut points = Vec::new();
    if let Some(top_row) = usage
        .rows
        .iter()
        .max_by(|left, right| left.total_cost_usd.total_cmp(&right.total_cost_usd))
    {
        points.push(format!(
            "Most visible spend went to {} for {} / {}.",
            top_row
                .project
                .as_deref()
                .unwrap_or("an unattributed project"),
            top_row.provider.as_deref().unwrap_or("unknown provider"),
            top_row.model.as_deref().unwrap_or("unknown model")
        ));
    }
    if let Some(item) = latest_decision {
        if let Some(detail) = decision_supporting_line(item) {
            points.push(detail);
        }
    }
    if stats.warn > 0 {
        points.push(format!(
            "{} decision(s) were warned instead of blocked, which means work continued under policy pressure.",
            stats.warn
        ));
    }
    if let Some(adoption) = &usage.protected_adoption {
        if adoption.unused_protected_opportunity_usd > 0.0 {
            points.push(format!(
                "{} of protected opportunity is still available across {} low adopters.",
                format_money(adoption.unused_protected_opportunity_usd),
                adoption.low_adopters.len()
            ));
        }
    }
    let tool_events = activity
        .iter()
        .filter(|item| is_tool_kind(&item.kind))
        .count();
    if tool_events > 0 {
        points.push(format!(
            "{} tool event(s) were captured, so this view includes actual workflow evidence beyond model billing.",
            tool_events
        ));
    }
    if points.is_empty() && !decisions.is_empty() {
        points.push(
            "Recent decision cards carry the routing, model, and limit evidence for this run."
                .to_owned(),
        );
    }
    (title, lead, points)
}

fn simulation_story(
    report: &crate::simulation::SimulationComparisonReport,
) -> (String, String, Vec<String>) {
    let mut notes = Vec::new();
    for strategy in &report.strategies {
        if let Some(day) = strategy.exhaustion_day {
            notes.push(format!(
                "{} exhausted shared budget on day {}.",
                strategy.id, day
            ));
        }
        if strategy.limit_hit_count > 0 {
            notes.push(format!(
                "{} blocked {} limit-hit requests, prevented {}, and left {} unused.",
                strategy.id,
                strategy.limit_hit_count,
                format_money(strategy.runaway_spend_prevented_usd),
                format_money(strategy.unused_budget_usd)
            ));
        }
        if strategy.unused_protected_opportunity_usd > 0.0
            || strategy.low_adopter_count > 0
            || strategy.high_adopter_count > 0
        {
            notes.push(format!(
                "{} surfaced {} of unused protected opportunity across {} low adopters and {} high adopters.",
                strategy.id,
                format_money(strategy.unused_protected_opportunity_usd),
                strategy.low_adopter_count,
                strategy.high_adopter_count
            ));
        }
    }

    let title = if report
        .strategies
        .iter()
        .any(|strategy| strategy.limit_hit_count > 0)
    {
        "Budget limits changed the spend story".to_owned()
    } else if report
        .strategies
        .iter()
        .any(|strategy| strategy.unused_protected_opportunity_usd > 0.0)
    {
        "Adoption policy changed what the team could see".to_owned()
    } else {
        "Policy choices changed the outcome under identical demand".to_owned()
    };
    let lead = format!(
        "{} strategy variants processed {} simulated requests with the same synthetic demand.",
        report.strategies.len(),
        compact_number(report.total_requests)
    );
    (title, lead, notes)
}

fn metric_card(html: &mut String, label: &str, value: &str, hint: &str) {
    let _ = write!(
        html,
        "<article class=\"card\"><div class=\"label\">{}</div><div class=\"value\">{}</div><div class=\"hint\">{}</div></article>",
        escape_html(label),
        escape_html(value),
        escape_html(hint)
    );
}

fn meta_pill(html: &mut String, value: &str) {
    let _ = write!(
        html,
        "<span class=\"meta-pill\">{}</span>",
        escape_html(value)
    );
}

fn fact_block_if_some(html: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        fact_block(html, label, value);
    }
}

fn details_block(html: &mut String, summary: &str, evidence: &str) {
    let _ = write!(
        html,
        "<details class=\"evidence\"><summary>{}</summary><div class=\"summary\">{}</div></details>",
        escape_html(summary),
        escape_html(evidence)
    );
}

fn routing_evidence_present(item: &TraceReportItem) -> bool {
    decision_budget(item).is_some()
        || decision_request(item).is_some()
        || decision_remaining_budget(item).is_some()
        || decision_estimated_cost(item).is_some()
        || decision_matched_entity(item).is_some()
        || decision_model_check(item).is_some()
        || item
            .limit_hits
            .as_ref()
            .is_some_and(|hits| !hits.is_empty())
}

fn compare_row(
    html: &mut String,
    label: &str,
    detail: &str,
    value: &str,
    ratio: f64,
    fill_class: &str,
) {
    let _ = write!(
        html,
        "<div class=\"compare-row\"><div class=\"compare-label\"><strong>{}</strong><div class=\"summary\">{}</div></div><div class=\"track\"><div class=\"fill {}\" style=\"width:{:.2}%\"></div></div><div class=\"metric-value\">{}</div></div>",
        escape_html(label),
        escape_html(detail),
        fill_class,
        ratio.clamp(0.0, 100.0),
        escape_html(value)
    );
}

fn decision_flow_panel(html: &mut String, stats: &DecisionStats) {
    let total = stats.total();
    if total == 0 {
        return;
    }
    let allow = percent(stats.allow, total);
    let warn = percent(stats.warn, total);
    let deny = percent(stats.deny, total);
    let posture = if stats.deny > 0 {
        "Budget limits actively stopped risky work."
    } else if stats.warn > 0 {
        "Policy allowed work to continue under pressure."
    } else {
        "Policy stayed in an allow-first posture."
    };
    html.push_str("<section class=\"panel\"><h2>Budget posture</h2><p class=\"summary\">Start here for the policy shape of the run: what continued, what continued under warning, and what was blocked before spend landed.</p>");
    let _ = write!(
        html,
        "<div class=\"entry-title\">{}</div>",
        escape_html(posture)
    );
    let _ = write!(
        html,
        "<div class=\"stack\"><div class=\"segment good\" style=\"width:{allow:.2}%\"></div><div class=\"segment warn\" style=\"width:{warn:.2}%\"></div><div class=\"segment bad\" style=\"width:{deny:.2}%\"></div></div>"
    );
    let _ = write!(
        html,
        "<div class=\"legend\"><span><span class=\"dot good\"></span>allow {}</span><span><span class=\"dot warn\"></span>warn {}</span><span><span class=\"dot bad\"></span>deny {}</span><span>limit hits {}</span></div>",
        stats.allow, stats.warn, stats.deny, stats.limit_hits
    );
    html.push_str("<div class=\"fact-grid\">");
    fact_block(html, "Decisions observed", &total.to_string());
    fact_block(html, "Limit hits", &stats.limit_hits.to_string());
    fact_block(html, "Allowed share", &format!("{allow:.0}%"));
    fact_block(html, "Blocked share", &format!("{deny:.0}%"));
    html.push_str("</div>");
    html.push_str("</section>");
}

fn spend_breakdown_panel(html: &mut String, usage: &UsageReport) {
    let max_cost = usage
        .rows
        .iter()
        .map(|row| row.total_cost_usd)
        .fold(0.0_f64, f64::max);
    if max_cost <= 0.0 {
        return;
    }
    html.push_str("<section class=\"panel\"><h2>Where the spend went</h2><p class=\"summary\">The tallest bars show where finalized cost concentrated, so you can see whether one project, model, or subject dominated the run.</p>");
    for row in usage.rows.iter().take(6) {
        let ratio = if max_cost == 0.0 {
            0.0
        } else {
            (row.total_cost_usd / max_cost) * 100.0
        };
        compare_row(
            html,
            row.project.as_deref().unwrap_or("-"),
            &format!(
                "{} / {} · {}",
                row.provider.as_deref().unwrap_or("-"),
                row.model.as_deref().unwrap_or("-"),
                row.subject.as_deref().unwrap_or("-")
            ),
            &format_money(row.total_cost_usd),
            ratio,
            "blue",
        );
    }
    html.push_str("</section>");
}

fn metric_compare_block(
    html: &mut String,
    title: &str,
    hint: &str,
    values: &[(String, f64, String)],
    emphasis: ComparisonEmphasis,
) {
    let max_value = values
        .iter()
        .map(|(_, value, _)| *value)
        .fold(0.0_f64, f64::max);
    let best_value = values
        .iter()
        .map(|(_, value, _)| *value)
        .fold(0.0_f64, f64::max);
    html.push_str("<div class=\"compare-group\">");
    let _ = write!(
        html,
        "<div class=\"compare-title\">{}</div><div class=\"summary\">{}</div>",
        escape_html(title),
        escape_html(hint)
    );
    for (label, value, display) in values {
        let ratio = if max_value == 0.0 {
            0.0
        } else {
            (*value / max_value) * 100.0
        };
        let fill = match emphasis {
            ComparisonEmphasis::Neutral => "blue",
            ComparisonEmphasis::HigherBetter if (*value - best_value).abs() < 1e-9 => "good",
            ComparisonEmphasis::HigherBetter => "violet",
        };
        compare_row(html, label, "", display, ratio, fill);
    }
    html.push_str("</div>");
}

fn strategy_scorecards_panel(
    html: &mut String,
    report: &crate::simulation::SimulationComparisonReport,
) {
    html.push_str("<section class=\"panel\"><h2>Strategy scorecards</h2><p class=\"summary\">Each card tells the story of one strategy before you drop into the evidence table.</p><div class=\"score-grid\">");
    for strategy in &report.strategies {
        let exhaustion = strategy
            .exhaustion_day
            .map(|day| format!("budget exhausted on day {day}"))
            .unwrap_or_else(|| "budget stayed available through the horizon".to_owned());
        let _ = write!(
            html,
            "<article class=\"card\"><div class=\"label\">{}</div><div class=\"value\">{}</div><div class=\"hint\">{} allowed · {} denied · fairness {:.2}</div><ul class=\"score-list\"><li>{}</li><li>Unused budget: {}</li><li>Runaway prevented: {}</li>",
            escape_html(&strategy.id),
            format_money(strategy.total_cost_usd),
            strategy.allowed_requests,
            strategy.denied_requests,
            strategy.fairness_score,
            escape_html(&exhaustion),
            format_money(strategy.unused_budget_usd),
            format_money(strategy.runaway_spend_prevented_usd),
        );
        if strategy.unused_protected_opportunity_usd > 0.0
            || strategy.low_adopter_count > 0
            || strategy.high_adopter_count > 0
        {
            let _ = write!(
                html,
                "<li>Protected opportunity: {} across {} low adopters and {} high adopters.</li>",
                format_money(strategy.unused_protected_opportunity_usd),
                strategy.low_adopter_count,
                strategy.high_adopter_count
            );
        }
        html.push_str("</ul></article>");
    }
    html.push_str("</div></section>");
}

fn simulation_evidence_table(
    html: &mut String,
    report: &crate::simulation::SimulationComparisonReport,
) {
    html.push_str("<section class=\"panel\"><h2>Detailed comparison</h2><div class=\"table-wrap\"><table><thead><tr><th>Strategy</th><th>Spend</th><th>Denied</th><th>Runaway prevented</th><th>Protected opportunity</th><th>Fairness</th><th>Exhaustion</th></tr></thead><tbody>");
    for strategy in &report.strategies {
        let exhaustion = strategy
            .exhaustion_day
            .map(|day| day.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.2}</td><td>{}</td></tr>",
            escape_html(&strategy.id),
            format_money(strategy.total_cost_usd),
            strategy.denied_requests,
            format_money(strategy.runaway_spend_prevented_usd),
            format_money(strategy.unused_protected_opportunity_usd),
            strategy.fairness_score,
            escape_html(&exhaustion)
        );
    }
    html.push_str("</tbody></table></div></section>");
}

fn token_mix_panel(html: &mut String, totals: &UsageTotals) {
    html.push_str("<section class=\"panel\"><h2>Token mix</h2>");
    if totals.total_tokens == 0 {
        html.push_str("<div class=\"empty\">No finalized token usage yet.</div></section>");
        return;
    }
    if totals.input_tokens == 0
        && totals.output_tokens == 0
        && totals.cache_read_tokens == 0
        && totals.cache_write_tokens == 0
    {
        let _ = write!(
            html,
            "<p class=\"summary\">Only the total token count was finalized for this run, so Noether cannot yet split it into input, output, or cache categories.</p><div class=\"bar\"><div class=\"fill slate\" style=\"width:100%\"></div></div><div class=\"legend\"><span><span class=\"dot slate\"></span>total {}</span></div>",
            compact_number(totals.total_tokens)
        );
        html.push_str("</section>");
        return;
    }
    let input = percent(totals.input_tokens, totals.total_tokens);
    let output = percent(totals.output_tokens, totals.total_tokens);
    let cache = percent(
        totals.cache_read_tokens + totals.cache_write_tokens,
        totals.total_tokens,
    );
    let _ = write!(
        html,
        "<div class=\"bar\"><div class=\"in\" style=\"width:{input:.2}%\"></div><div class=\"out\" style=\"width:{output:.2}%\"></div><div class=\"cache\" style=\"width:{cache:.2}%\"></div></div>"
    );
    let _ = write!(
        html,
        "<div class=\"legend\"><span><span class=\"dot in\"></span>input {}</span><span><span class=\"dot out\"></span>output {}</span><span><span class=\"dot cache\"></span>cache {}</span></div>",
        compact_number(totals.input_tokens),
        compact_number(totals.output_tokens),
        compact_number(totals.cache_read_tokens + totals.cache_write_tokens)
    );
    html.push_str("</section>");
}

fn adoption_snapshot_panel(html: &mut String, usage: &UsageReport) {
    let Some(adoption) = &usage.protected_adoption else {
        return;
    };
    html.push_str("<section class=\"panel\"><h2>Adoption snapshot</h2><p class=\"summary\">Protected budget only matters if it reaches under-users without hiding where heavy protected usage is already concentrated.</p><div class=\"grid\">");
    metric_card(
        html,
        "Protected opportunity remaining",
        &format_money(adoption.unused_protected_opportunity_usd),
        "current protected grant that still has room to be used",
    );
    metric_card(
        html,
        "Carryover liability",
        &format_money(adoption.carryover_liability_usd),
        "unused protected grant that can roll forward into the next window",
    );
    metric_card(
        html,
        "Low adopters",
        &adoption.low_adopters.len().to_string(),
        "people or teams with meaningful protected room left",
    );
    metric_card(
        html,
        "Top consumers",
        &adoption.high_adopters.len().to_string(),
        "people or teams already consuming most of the protected pool",
    );
    html.push_str("</div></section>");
}

fn usage_rows_panel(html: &mut String, usage: &UsageReport) {
    html.push_str("<section class=\"panel\"><h2>Spend evidence</h2><p class=\"summary\">These cards keep row-level billing evidence readable without forcing a wide ledger table.</p>");
    if usage.rows.is_empty() {
        html.push_str("<div class=\"empty\">No usage has been finalized yet.</div></section>");
        return;
    }
    html.push_str("<div class=\"entry-list\">");
    for row in usage.rows.iter().take(8) {
        let title = row
            .project
            .as_deref()
            .or(row.subject.as_deref())
            .unwrap_or("Unattributed spend");
        let summary = format!(
            "{} finalized {} on {} / {} across {} token(s).",
            row.subject.as_deref().unwrap_or("This row"),
            format_money(row.total_cost_usd),
            row.provider.as_deref().unwrap_or("unknown provider"),
            row.model.as_deref().unwrap_or("unknown model"),
            compact_number(row.total_tokens)
        );
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">Finalized spend row</div><div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div>",
            escape_html(title),
            escape_html(&summary)
        );
        html.push_str("<div class=\"meta-row\">");
        meta_pill(html, &format_money(row.total_cost_usd));
        meta_pill(
            html,
            &format!("{} tokens", compact_number(row.total_tokens)),
        );
        html.push_str("</div></div><div class=\"fact-grid\">");
        fact_block_if_some(html, "Project", row.project.as_deref());
        fact_block_if_some(html, "Subject", row.subject.as_deref());
        fact_block_if_some(html, "Provider", row.provider.as_deref());
        fact_block_if_some(html, "Model", row.model.as_deref());
        fact_block(html, "Finalized", &row.finalized_reservations.to_string());
        fact_block(html, "Active", &row.active_reservations.to_string());
        html.push_str("</div></article>");
    }
    html.push_str("</div></section>");
}

fn protected_adoption_cards_panel(
    html: &mut String,
    title: &str,
    summary: &str,
    entries: &[crate::ledger::ProtectedAdoptionEntityReport],
    accent: &str,
    opportunity_label: &str,
) {
    if entries.is_empty() {
        return;
    }
    let _ = write!(
        html,
        "<section class=\"panel\"><h2>{}</h2><p class=\"summary\">{}</p><div class=\"entity-grid\">",
        escape_html(title),
        escape_html(summary)
    );
    for entity in entries {
        let _ = write!(
            html,
            "<article class=\"entity-card {}\"><div class=\"eyebrow\">{}</div><div class=\"entry-title\">{}</div>",
            escape_html(accent),
            escape_html(&entity.budget_id),
            escape_html(&entity.entity_key)
        );
        let lead = if accent == "accent-violet" {
            format!(
                "{} still available from the current protected grant after only {} of visible use.",
                format_money(entity.current_grant_usd),
                format_money(entity.used_current_grant_usd)
            )
        } else {
            format!(
                "{} has already been used from a protected amount of {}.",
                format_money(entity.used_current_grant_usd),
                format_money(entity.protected_amount_usd)
            )
        };
        let _ = write!(html, "<p class=\"summary\">{}</p>", escape_html(&lead));
        html.push_str("<div class=\"fact-grid\">");
        fact_block(
            html,
            opportunity_label,
            &format_money(entity.current_grant_usd),
        );
        fact_block(html, "Carryover", &format_money(entity.carryover_usd));
        fact_block(
            html,
            "Current usage",
            &format_money(entity.used_current_grant_usd),
        );
        fact_block(
            html,
            "Protected amount",
            &format_money(entity.protected_amount_usd),
        );
        html.push_str("</div></article>");
    }
    html.push_str("</div></section>");
}

fn protected_adoption_panel(html: &mut String, usage: &UsageReport) {
    let Some(adoption) = &usage.protected_adoption else {
        return;
    };
    protected_adoption_cards_panel(
        html,
        "Protected opportunity remaining",
        "These are the people or teams who still have meaningful protected budget available and may need enablement rather than stricter caps.",
        &adoption.low_adopters,
        "accent-violet",
        "Opportunity left",
    );
    protected_adoption_cards_panel(
        html,
        "Top consumers",
        "These are the heaviest protected-budget consumers in the current window, which helps separate healthy adoption from concentrated usage.",
        &adoption.high_adopters,
        "accent-good",
        "Current grant left",
    );
}

fn event_entries_panel(
    html: &mut String,
    title: &str,
    summary: &str,
    items: &[&TraceReportItem],
    empty_message: &str,
) {
    let _ = write!(
        html,
        "<section class=\"panel\"><h2>{}</h2><p class=\"summary\">{}</p>",
        escape_html(title),
        escape_html(summary)
    );
    if items.is_empty() {
        let _ = write!(
            html,
            "<div class=\"empty\">{}</div></section>",
            escape_html(empty_message)
        );
        return;
    }
    html.push_str("<div class=\"entry-list\">");
    for item in items.iter().take(8) {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div><div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div>",
            escape_html(&short_time(item)),
            escape_html(&item.kind),
            escape_html(&item.summary)
        );
        html.push_str("<div class=\"meta-row\">");
        meta_pill(html, &item.kind);
        html.push_str("</div></div>");
        details_block(html, "Show exact event evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn tools_panel(html: &mut String, activity: &[&TraceReportItem]) {
    let tools: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_tool_kind(&item.kind))
        .collect();
    event_entries_panel(
        html,
        "Tool usage",
        "Tool cards show what Pi invoked and what landed back in the trace without exposing raw prompt logs.",
        &tools,
        "No tool calls or tool results were observed for this run yet. If Pi did not use tools, this is expected.",
    );
}

fn agent_activity_panel(html: &mut String, activity: &[&TraceReportItem]) {
    let agent_events: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_agent_kind(&item.kind))
        .collect();
    event_entries_panel(
        html,
        "Agent activity",
        "Lifecycle cards show how the agent progressed through provider calls, turn boundaries, and final completion.",
        &agent_events,
        "No Pi agent lifecycle events were observed yet. This usually means the run came from the vertical demo or Pi did not emit lifecycle hooks for this trace.",
    );
}

fn skill_context_panel(html: &mut String, activity: &[&TraceReportItem]) {
    let context_events: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_skill_context_kind(&item.kind))
        .collect();
    event_entries_panel(
        html,
        "Skills and context",
        "Context cards show the skills, tools, and repo context Pi carried into the run without leaking prompt content.",
        &context_events,
        "No skill/context event was observed yet. When Pi provides agent context, this section will show selected tools, skills, and context-file summaries without prompt text.",
    );
}

fn decisions_panel(html: &mut String, decisions: &[TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Decision narrative</h2><p class=\"summary\">Readable cards come first; exact ledger fields stay collapsed underneath each decision.</p>");
    if decisions.is_empty() {
        html.push_str("<div class=\"empty\">No authorization decisions yet.</div></section>");
        return;
    }
    html.push_str("<div class=\"entry-list\">");
    for item in decisions.iter().take(8) {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div>{}<div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div>",
            escape_html(&short_time(item)),
            outcome_pill(&item.kind),
            escape_html(&decision_headline(item)),
            escape_html(&latest_decision_hint(item))
        );
        html.push_str("<div class=\"meta-row\">");
        if let Some(budget) = decision_budget(item) {
            meta_pill(html, &budget);
        }
        if let Some(model) = decision_model(item) {
            meta_pill(html, &model);
        }
        if let Some(request) = decision_request(item) {
            meta_pill(html, &request);
        }
        html.push_str("</div></div><div class=\"fact-grid\">");
        fact_block_if_some(html, "Budget", decision_budget(item).as_deref());
        fact_block_if_some(
            html,
            "Matched entity",
            decision_matched_entity(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Budget-window remaining",
            decision_remaining_budget(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Estimated cost",
            decision_estimated_cost(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Model check",
            decision_model_check_label(item).as_deref(),
        );
        html.push_str("</div>");
        if let Some(hits) = &item.limit_hits {
            html.push_str("<ul class=\"score-list\">");
            for hit in hits {
                let _ = write!(
                    html,
                    "<li><strong>{}</strong> - {}</li>",
                    escape_html(&hit.rule_id),
                    escape_html(&hit.reason)
                );
            }
            html.push_str("</ul>");
        }
        details_block(html, "Show exact decision evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn budget_routing_panel(html: &mut String, decisions: &[TraceReportItem]) {
    let routed: Vec<_> = decisions
        .iter()
        .filter(|item| routing_evidence_present(item))
        .take(6)
        .collect();
    if routed.is_empty() {
        return;
    }
    html.push_str("<section class=\"panel\"><h2>Budget routing</h2><p class=\"summary\">This layer explains why Noether chose a budget, how much room was left, and what fallback or model checks shaped the decision.</p><div class=\"entry-list\">");
    for item in routed {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let title = decision_budget(item)
            .map(|budget| {
                format!(
                    "{} landed on {}",
                    decision_model(item).unwrap_or_else(|| "Request".to_owned()),
                    budget
                )
            })
            .unwrap_or_else(|| decision_headline(item));
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div><div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div>",
            escape_html(&short_time(item)),
            escape_html(&title),
            escape_html(&decision_supporting_line(item).unwrap_or_else(|| {
                "No additional routing explanation was recorded.".to_owned()
            }))
        );
        html.push_str("<div class=\"meta-row\">");
        if let Some(request) = decision_request(item) {
            meta_pill(html, &request);
        }
        if let Some(entity) = decision_matched_entity(item) {
            meta_pill(html, &entity);
        }
        html.push_str("</div></div><div class=\"fact-grid\">");
        fact_block_if_some(html, "Budget", decision_budget(item).as_deref());
        fact_block_if_some(
            html,
            "Matched entity",
            decision_matched_entity(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Estimated cost",
            decision_estimated_cost(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Budget-window remaining",
            decision_remaining_budget(item).as_deref(),
        );
        fact_block_if_some(
            html,
            "Model check",
            decision_model_check_label(item).as_deref(),
        );
        html.push_str("</div>");
        details_block(html, "Show exact routing evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn risky_runs_panel(html: &mut String, decisions: &[TraceReportItem]) {
    let risky: Vec<_> = decisions
        .iter()
        .filter(|item| {
            item.limit_hits
                .as_ref()
                .is_some_and(|hits| !hits.is_empty())
        })
        .collect();
    if risky.is_empty() {
        return;
    }
    html.push_str("<section class=\"panel\"><h2>Risky runs</h2><p class=\"summary\">These decisions hit budget limits. Read the plain-language reason first, then expand the exact ledger evidence if needed.</p><div class=\"entry-list\">");
    for item in risky {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div>{}<div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div></div>",
            escape_html(&short_time(item)),
            outcome_pill(&item.kind),
            escape_html(&decision_headline(item)),
            escape_html(&latest_decision_hint(item))
        );
        html.push_str("<ul class=\"score-list\">");
        for hit in item.limit_hits.as_ref().into_iter().flatten() {
            let _ = write!(
                html,
                "<li><strong>{}</strong> - {}</li>",
                escape_html(&limit_hit_name(hit)),
                escape_html(&hit.reason)
            );
        }
        html.push_str("</ul>");
        details_block(html, "Show exact limit evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn lifecycle_limits_panel(html: &mut String, trace: Option<&TraceReport>) {
    let items: Vec<&TraceReportItem> = trace
        .map(|trace| {
            trace
                .items
                .iter()
                .filter(|item| item.kind.starts_with("limit.report_only."))
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        return;
    }
    html.push_str(
        "<section class=\"panel\"><h2>Lifecycle limits (report-only)</h2><p class=\"summary\">These lifecycle signals were detected after the run emitted events. They are audit evidence, not proof that Noether blocked the action before it happened.</p><div class=\"entry-list\">",
    );
    for item in items {
        html.push_str("<article class=\"entry-card\"><div class=\"entry-top\"><div>");
        let _ = write!(
            html,
            "<div class=\"eyebrow\">{}</div><div class=\"entry-title\">{}</div><p class=\"summary\">{}</p></div><div class=\"meta-row\">",
            escape_html(&short_time(item)),
            escape_html(&item.kind),
            escape_html(&item.summary)
        );
        meta_pill(html, "report-only");
        meta_pill(html, &item.kind);
        html.push_str("</div></div>");
        details_block(html, "Show exact lifecycle evidence", &item.summary);
        html.push_str("</article>");
    }
    html.push_str("</div></section>");
}

fn timeline_panel(
    html: &mut String,
    trace: Option<&TraceReport>,
    observations: &[TraceReportItem],
) {
    html.push_str("<section class=\"panel\"><h2>Run timeline</h2>");
    let items: Vec<&TraceReportItem> = trace
        .map(|trace| trace.items.iter().collect())
        .unwrap_or_else(|| observations.iter().take(12).collect());
    if items.is_empty() {
        html.push_str("<div class=\"empty\">No trace or observation events yet.</div></section>");
        return;
    }
    if let Some(trace) = trace {
        let _ = write!(
            html,
            "<div class=\"hint\">Featured trace: <code>{}</code></div>",
            escape_html(&trace.trace_id)
        );
    }
    html.push_str("<ol class=\"timeline\">");
    for item in items {
        let _ = write!(
            html,
            "<li class=\"event\"><div class=\"time\">{}</div><div class=\"kind\">{}</div><div class=\"summary\">{}</div></li>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</ol></section>");
}

fn is_tool_kind(kind: &str) -> bool {
    kind == "tool.observed" || kind == "pi.tool_call"
}

fn is_agent_kind(kind: &str) -> bool {
    matches!(
        kind,
        "pi.provider_call.started"
            | "pi.message_end"
            | "pi.stream_summary"
            | "pi.turn_end"
            | "pi.agent_end"
            | "pi.authorize"
            | "pi.authorize_error"
    )
}

fn is_skill_context_kind(kind: &str) -> bool {
    kind == "pi.agent_context"
}

fn outcome_pill(kind: &str) -> String {
    let class = if kind.ends_with(".deny") {
        "bad"
    } else if kind.ends_with(".warn") {
        "warn"
    } else {
        "ok"
    };
    format!(
        "<span class=\"pill {class}\">{}</span>",
        escape_html(decision_label(kind))
    )
}

fn event_pill(kind: &str) -> String {
    format!("<span class=\"pill\">{}</span>", escape_html(kind))
}

fn decision_label(kind: &str) -> &'static str {
    if kind.ends_with(".deny") {
        "deny"
    } else if kind.ends_with(".warn") {
        "warn"
    } else if kind.ends_with(".allow") {
        "allow"
    } else {
        "unknown"
    }
}

fn summary_value(summary: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    summary
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix).map(ToOwned::to_owned))
}

fn formatted_summary_money(summary: &str, key: &str) -> Option<String> {
    summary_value(summary, key).map(|raw| raw.parse::<f64>().map(format_money).unwrap_or(raw))
}

fn decision_budget(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.selected_budget_id.clone())
        .or_else(|| summary_value(&item.summary, "selected_budget"))
        .or_else(|| {
            item.routing
                .as_ref()
                .and_then(|routing| routing.matched_entity.clone())
        })
        .or_else(|| summary_value(&item.summary, "matched_entity"))
}

fn decision_model(item: &TraceReportItem) -> Option<String> {
    summary_value(&item.summary, "model")
}

fn decision_request(item: &TraceReportItem) -> Option<String> {
    summary_value(&item.summary, "request")
}

fn decision_action(item: &TraceReportItem) -> Option<String> {
    summary_value(&item.summary, "action")
}

fn decision_remaining_budget(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.budget_window_remaining_usd)
        .map(format_money)
        .or_else(|| formatted_summary_money(&item.summary, "budget_window_remaining"))
}

fn decision_estimated_cost(item: &TraceReportItem) -> Option<String> {
    formatted_summary_money(&item.summary, "estimated_cost")
}

fn decision_matched_entity(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.matched_entity.clone())
        .or_else(|| summary_value(&item.summary, "matched_entity"))
}

fn decision_model_check(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.model_check.clone())
        .or_else(|| summary_value(&item.summary, "model_check"))
}

fn decision_rejected_budget(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.rejected_budget_id.clone())
        .or_else(|| summary_value(&item.summary, "rejected_budget"))
}

fn decision_rejected_reason(item: &TraceReportItem) -> Option<String> {
    item.routing
        .as_ref()
        .and_then(|routing| routing.rejected_budget_reason.clone())
}

fn decision_is_model_denial(item: &TraceReportItem) -> bool {
    decision_model_check(item).as_deref() == Some("denied")
        && decision_rejected_reason(item)
            .as_deref()
            .is_some_and(|reason| reason.contains("provider/model is not allowed"))
}

fn decision_model_check_label(item: &TraceReportItem) -> Option<String> {
    let raw = decision_model_check(item)?;
    if decision_is_model_denial(item) {
        return Some("blocked by model allowlist".to_owned());
    }
    if let Some(budget) = raw.strip_prefix("allowed:") {
        return Some(format!("allowed on {budget}"));
    }
    Some(raw)
}

fn limit_hit_name(hit: &crate::ledger::DecisionLimitHitReport) -> String {
    if let Some(window_id) = &hit.window_id {
        return match hit.window_mode.as_deref() {
            Some(mode) => format!("{window_id} {mode} limit"),
            None => format!("{window_id} limit"),
        };
    }

    match hit
        .rule_id
        .rsplit('.')
        .next()
        .unwrap_or(hit.rule_id.as_str())
    {
        "context_tokens" => "context limit".to_owned(),
        "request_cost" => "request-cost limit".to_owned(),
        "tool_calls" => "tool-call limit".to_owned(),
        "agent_steps" => "agent-step limit".to_owned(),
        "retries" => "retry limit".to_owned(),
        _ => format!("{} limit", hit.rule_id),
    }
}

fn decision_binding_limit(
    item: &TraceReportItem,
) -> Option<&crate::ledger::DecisionLimitHitReport> {
    item.limit_hits
        .as_deref()
        .and_then(crate::ledger::binding_limit_hit)
}

fn decision_headline(item: &TraceReportItem) -> String {
    let model = decision_model(item).unwrap_or_else(|| "the requested model".to_owned());
    if let Some(hit) = decision_binding_limit(item) {
        let limit_name = limit_hit_name(hit);
        if hit.severity == crate::contract::DecisionSeverity::Deny {
            format!("{model} was blocked by {limit_name}")
        } else if let Some(budget) = decision_budget(item) {
            format!("{model} continued on {budget} under {limit_name}")
        } else {
            format!("{model} continued under {limit_name}")
        }
    } else if decision_action(item).as_deref() == Some("ask") {
        if let Some(budget) = decision_budget(item).or_else(|| decision_rejected_budget(item)) {
            format!("{model} required approval on {budget}")
        } else {
            format!("{model} required approval")
        }
    } else if item.kind.ends_with(".deny") {
        if decision_is_model_denial(item) {
            if let Some(budget) = decision_rejected_budget(item) {
                format!("{model} was blocked by {budget}'s model allowlist")
            } else {
                format!("{model} was blocked by the model allowlist")
            }
        } else if let Some(budget) =
            decision_budget(item).or_else(|| decision_rejected_budget(item))
        {
            format!("{model} was blocked on {budget}")
        } else {
            format!("{model} was blocked")
        }
    } else if item.kind.ends_with(".warn") {
        if let Some(budget) = decision_budget(item) {
            format!("{model} continued on {budget} with a warning")
        } else {
            format!("{model} continued with a warning")
        }
    } else if let Some(budget) = decision_budget(item) {
        format!("{model} was approved on {budget}")
    } else {
        format!("{model} was approved")
    }
}

fn decision_supporting_line(item: &TraceReportItem) -> Option<String> {
    if let Some(hit) = decision_binding_limit(item) {
        return Some(format!(
            "Binding limit: {}. {}",
            limit_hit_name(hit),
            hit.reason
        ));
    }

    if decision_is_model_denial(item) {
        let model = decision_model(item).unwrap_or_else(|| "the requested model".to_owned());
        let mut line = match decision_rejected_budget(item) {
            Some(budget) => format!("Attempted model {model} is not allowed on budget {budget}."),
            None => format!("Attempted model {model} is not allowed by the active budget policy."),
        };
        if item
            .routing
            .as_ref()
            .and_then(|routing| routing.selected_budget_id.as_ref())
            .is_none()
        {
            line.push_str(" No fallback budget could satisfy the request.");
        }
        return Some(line);
    }

    if decision_action(item).as_deref() == Some("ask") {
        let mut line = "Noether required approval before this request could proceed.".to_owned();
        if let Some(reason) = decision_rejected_reason(item) {
            line.push_str(&format!(" {reason}."));
        }
        return Some(line);
    }

    if item.kind.ends_with(".deny") {
        if let Some(reason) = decision_rejected_reason(item) {
            let mut line = match decision_rejected_budget(item) {
                Some(budget) => format!("Budget {budget} rejected the request: {reason}."),
                None => format!("Noether blocked the request: {reason}."),
            };
            if let Some(remaining) = item
                .routing
                .as_ref()
                .and_then(|routing| routing.budget_window_remaining_usd)
            {
                line.push_str(&format!(
                    " Recorded budget-window remaining at evaluation time: {}.",
                    format_money(remaining)
                ));
            }
            return Some(line);
        }
    }

    item.routing.as_ref().map(|routing| {
        let mut line = routing.selection_reason.clone().unwrap_or_else(|| {
            "Noether selected the best available budget for this request.".to_owned()
        });
        if let Some(entity) = &routing.matched_entity {
            line.push_str(&format!(" Matched entity: {entity}."));
        }
        if let Some(remaining) = routing.budget_window_remaining_usd {
            line.push_str(&format!(
                " Selected budget-window remaining: {}.",
                format_money(remaining)
            ));
            line.push_str(" Tighter limits can still bind sooner.");
        }
        line
    })
}

fn short_time(item: &TraceReportItem) -> String {
    item.occurred_at.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn token_hint(totals: &UsageTotals) -> String {
    if totals.total_tokens == 0 {
        "no finalized token usage".to_owned()
    } else if totals.input_tokens == 0
        && totals.output_tokens == 0
        && totals.cache_read_tokens == 0
        && totals.cache_write_tokens == 0
    {
        "finalized token total was recorded without an input/output split".to_owned()
    } else {
        format!(
            "{} input / {} output",
            compact_number(totals.input_tokens),
            compact_number(totals.output_tokens)
        )
    }
}

fn latest_decision_hint(item: &TraceReportItem) -> String {
    decision_supporting_line(item).unwrap_or_else(|| decision_headline(item))
}

fn compact_number(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn format_money(value: f64) -> String {
    if value == 0.0 {
        "$0".to_owned()
    } else if value < 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

fn percent(value: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (value as f64 / total as f64) * 100.0
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use std::path::Path;
    use tempfile::tempdir;

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
            "protected adoption surfaced $1.11 of unused protected opportunity across 3 low adopters and 5 high adopters."
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
