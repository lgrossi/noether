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
use crate::ledger::{BudgetLedger, TraceReport, TraceReportItem, UsageReport};
use crate::policy::load_policy;
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
        Command::Scenario(command) => run_scenario(command).await,
        Command::Simulate(command) => run_simulate(command).await,
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
        let strategy_dashboard_path = strategy
            .db_path
            .parent()
            .unwrap_or(&out_dir)
            .join("noether-dashboard.html");
        let ledger = BudgetLedger::open_sqlite(&strategy.db_path)?;
        let usage = ledger.usage_report()?;
        let decisions = ledger.decisions_report()?;
        fs::write(
            &strategy_dashboard_path,
            render_dashboard(&usage, &decisions, None, &[]),
        )
        .await?;
        println!("strategy\t{}", strategy.id);
        println!("db_path\t{}", strategy.db_path.display());
        println!("usage_report\t{}", strategy.usage_report_path.display());
        println!(
            "decisions_report\t{}",
            strategy.decisions_report_path.display()
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
        ScenarioAssertion::GuardHit {
            request_id,
            rule_id,
        } => match decision_reports.get(request_id.as_str()) {
            Some(item)
                if item
                    .guard_hits
                    .as_ref()
                    .is_some_and(|hits| hits.iter().any(|hit| hit.rule_id == *rule_id)) => {}
            Some(_) => failures.push(format!("request {request_id} expected guard hit {rule_id}")),
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
    let tool_count = activity
        .iter()
        .filter(|item| is_tool_kind(&item.kind))
        .count();
    let agent_count = activity
        .iter()
        .filter(|item| is_agent_kind(&item.kind))
        .count();
    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether dashboard</title>");
    html.push_str(
        "<style>
        :root { color-scheme: dark; --bg:#0f172a; --panel:#111c33; --muted:#94a3b8; --text:#e5edf7; --line:#263449; --good:#22c55e; --warn:#f59e0b; --bad:#ef4444; --blue:#38bdf8; --violet:#a78bfa; }
        * { box-sizing: border-box; }
        body { margin:0; font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; background:radial-gradient(circle at top left,#172554,#0f172a 42%); color:var(--text); }
        main { max-width:1180px; margin:0 auto; padding:32px 20px 48px; }
        h1 { margin:0 0 4px; font-size:34px; letter-spacing:-0.04em; }
        h2 { margin:28px 0 12px; font-size:20px; }
        .sub { color:var(--muted); margin-bottom:24px; }
        .grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(210px,1fr)); }
        .card, .panel { background:rgba(17,28,51,.88); border:1px solid var(--line); border-radius:18px; box-shadow:0 18px 55px rgba(0,0,0,.22); }
        .card { padding:18px; }
        .label { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .value { font-size:30px; font-weight:800; margin-top:6px; letter-spacing:-0.03em; }
        .hint { color:var(--muted); margin-top:4px; }
        .panel { padding:18px; margin-top:14px; overflow:hidden; }
        .bar { height:12px; display:flex; overflow:hidden; border-radius:999px; background:#1e293b; margin:10px 0; }
        .in { background:var(--blue); }
        .out { background:var(--violet); }
        .cache { background:var(--warn); }
        .legend { display:flex; gap:16px; flex-wrap:wrap; color:var(--muted); font-size:13px; }
        .dot { display:inline-block; width:9px; height:9px; border-radius:50%; margin-right:6px; }
        .timeline { list-style:none; margin:0; padding:0; }
        .event { display:grid; grid-template-columns:165px 210px 1fr; gap:12px; padding:13px 0; border-top:1px solid var(--line); align-items:start; }
        .event:first-child { border-top:0; }
        .time, .summary { color:var(--muted); }
        .kind { font-weight:700; }
        .pill { display:inline-flex; align-items:center; border-radius:999px; padding:4px 9px; background:#1e293b; border:1px solid var(--line); font-size:13px; }
        .ok { color:var(--good); } .warn { color:var(--warn); } .bad { color:var(--bad); }
        table { width:100%; border-collapse:collapse; }
        th, td { text-align:left; padding:10px 8px; border-top:1px solid var(--line); vertical-align:top; }
        th { color:var(--muted); font-weight:600; font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .empty { color:var(--muted); padding:18px; border:1px dashed var(--line); border-radius:14px; }
        @media (max-width:760px) { .event { grid-template-columns:1fr; gap:2px; } h1 { font-size:28px; } }
        </style>",
    );
    html.push_str("</head><body><main>");
    html.push_str("<h1>Noether run dashboard</h1>");
    html.push_str("<div class=\"sub\">Readable local view of decisions, cost, usage, and trace events. Raw hook logs are not needed here.</div>");

    html.push_str("<section class=\"grid\">");
    metric_card(
        &mut html,
        "Spend",
        &format_money(usage.total_cost_usd),
        "finalized local ledger cost",
    );
    metric_card(
        &mut html,
        "Tokens",
        &compact_number(totals.total_tokens),
        &format!(
            "{} input / {} output",
            compact_number(totals.input_tokens),
            compact_number(totals.output_tokens)
        ),
    );
    metric_card(
        &mut html,
        "Reservations",
        &totals.reservations.to_string(),
        &format!(
            "{} finalized, {} active",
            totals.finalized_reservations, totals.active_reservations
        ),
    );
    metric_card(
        &mut html,
        "Latest decision",
        latest_decision
            .map(|item| decision_label(&item.kind))
            .unwrap_or("none"),
        latest_decision
            .map(|item| item.summary.as_str())
            .unwrap_or("no authorization decisions yet"),
    );
    metric_card(
        &mut html,
        "Tools",
        &tool_count.to_string(),
        "tool calls/results observed for the featured run",
    );
    metric_card(
        &mut html,
        "Agent activity",
        &agent_count.to_string(),
        "provider, message, turn, and agent lifecycle events",
    );
    if let Some(adoption) = &usage.protected_adoption {
        metric_card(
            &mut html,
            "Protected opportunity",
            &format_money(adoption.unused_protected_opportunity_usd),
            "remaining current protected grant this window",
        );
        metric_card(
            &mut html,
            "Carryover liability",
            &format_money(adoption.carryover_liability_usd),
            "carryover reserved for future protected use",
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
    }
    html.push_str("</section>");

    token_mix_panel(&mut html, &totals);
    usage_rows_panel(&mut html, usage);
    protected_adoption_panel(&mut html, usage);
    tools_panel(&mut html, &activity);
    agent_activity_panel(&mut html, &activity);
    skill_context_panel(&mut html, &activity);
    decisions_panel(&mut html, decisions);
    risky_runs_panel(&mut html, decisions);
    lifecycle_guardrails_panel(&mut html, trace);
    timeline_panel(&mut html, trace, observations);

    html.push_str("</main></body></html>");
    html
}

fn render_simulation_dashboard(report: &crate::simulation::SimulationComparisonReport) -> String {
    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether simulation dashboard</title>");
    html.push_str(
        "<style>
        :root { color-scheme: dark; --bg:#0f172a; --panel:#111c33; --muted:#94a3b8; --text:#e5edf7; --line:#263449; --blue:#38bdf8; }
        body { margin:0; font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; background:#0f172a; color:var(--text); }
        main { max-width:1180px; margin:0 auto; padding:32px 20px 48px; }
        .grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(220px,1fr)); }
        .card, .panel { background:rgba(17,28,51,.88); border:1px solid var(--line); border-radius:18px; }
        .card { padding:18px; }
        .panel { padding:18px; margin-top:16px; }
        .label { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .value { font-size:30px; font-weight:800; margin-top:6px; }
        table { width:100%; border-collapse:collapse; }
        th, td { text-align:left; padding:10px 8px; border-top:1px solid var(--line); vertical-align:top; }
        th { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        code { color:var(--blue); }
        </style>",
    );
    html.push_str("</head><body><main>");
    let title = report.name.as_deref().unwrap_or("Simulation comparison");
    let _ = write!(
        html,
        "<h1>{}</h1><p>Seed <code>{}</code> over {} simulated day(s).</p>",
        escape_html(title),
        report.seed,
        report.horizon_days
    );
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
    let highest_spend = report
        .strategies
        .iter()
        .map(|strategy| strategy.total_cost_usd)
        .fold(0.0_f64, f64::max);
    metric_card(
        &mut html,
        "Highest spend",
        &format_money(highest_spend),
        "largest simulated finalized cost among strategies",
    );
    html.push_str("</section>");

    let mut showcase_notes = Vec::new();
    for strategy in &report.strategies {
        if let Some(day) = strategy.exhaustion_day {
            showcase_notes.push(format!(
                "{} exhausted shared budget on day {}.",
                strategy.id, day
            ));
        }
        if strategy.guard_hit_count > 0 {
            showcase_notes.push(format!(
                "{} blocked {} guarded requests, prevented {}, and left {} unused.",
                strategy.id,
                strategy.guard_hit_count,
                format_money(strategy.runaway_spend_prevented_usd),
                format_money(strategy.unused_budget_usd)
            ));
        }
        if strategy.unused_protected_opportunity_usd > 0.0
            || strategy.low_adopter_count > 0
            || strategy.high_adopter_count > 0
        {
            showcase_notes.push(format!(
                "{} surfaced {} of unused protected opportunity across {} low adopters and {} high adopters.",
                strategy.id,
                format_money(strategy.unused_protected_opportunity_usd),
                strategy.low_adopter_count,
                strategy.high_adopter_count
            ));
        }
    }
    if !showcase_notes.is_empty() {
        html.push_str("<section class=\"panel\"><h2>Showcase evidence</h2><ul>");
        for note in showcase_notes {
            let _ = write!(html, "<li>{}</li>", escape_html(&note));
        }
        html.push_str("</ul></section>");
    }

    html.push_str("<section class=\"panel\"><h2>Strategy comparison</h2><table><thead><tr><th>Strategy</th><th>Spend</th><th>Unused budget</th><th>Denied</th><th>Fallbacks</th><th>Guard hits</th><th>Blocked work</th><th>Runaway prevented</th><th>Coverage</th><th>Fairness</th><th>Protected opportunity</th><th>Low adopters</th><th>High adopters</th><th>Carryover</th><th>Exhaustion</th></tr></thead><tbody>");
    for strategy in &report.strategies {
        let exhaustion = strategy
            .exhaustion_day
            .map(|day| day.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.2}</td><td>{:.2}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape_html(&strategy.id),
            format_money(strategy.total_cost_usd),
            format_money(strategy.unused_budget_usd),
            strategy.denied_requests,
            strategy.fallback_count,
            strategy.guard_hit_count,
            strategy.useful_work_blocked_score,
            format_money(strategy.runaway_spend_prevented_usd),
            strategy.adoption_coverage,
            strategy.fairness_score,
            format_money(strategy.unused_protected_opportunity_usd),
            strategy.low_adopter_count,
            strategy.high_adopter_count,
            format_money(strategy.carryover_liability_usd),
            escape_html(&exhaustion)
        );
    }
    html.push_str("</tbody></table></section>");

    html.push_str("<section class=\"panel\"><h2>Model mix</h2><table><thead><tr><th>Strategy</th><th>Model</th><th>Requests</th><th>Cost</th></tr></thead><tbody>");
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
    html.push_str("</tbody></table></section>");
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

fn metric_card(html: &mut String, label: &str, value: &str, hint: &str) {
    let _ = write!(
        html,
        "<article class=\"card\"><div class=\"label\">{}</div><div class=\"value\">{}</div><div class=\"hint\">{}</div></article>",
        escape_html(label),
        escape_html(value),
        escape_html(hint)
    );
}

fn token_mix_panel(html: &mut String, totals: &UsageTotals) {
    html.push_str("<section class=\"panel\"><h2>Token mix</h2>");
    if totals.total_tokens == 0 {
        html.push_str("<div class=\"empty\">No finalized token usage yet.</div></section>");
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

fn usage_rows_panel(html: &mut String, usage: &UsageReport) {
    html.push_str("<section class=\"panel\"><h2>Where the spend went</h2>");
    if usage.rows.is_empty() {
        html.push_str("<div class=\"empty\">No usage has been finalized yet.</div></section>");
        return;
    }
    html.push_str("<table><thead><tr><th>Project</th><th>Provider / model</th><th>Subject</th><th>Cost</th><th>Tokens</th><th>Status</th></tr></thead><tbody>");
    for row in &usage.rows {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}<br><span class=\"summary\">{}</span></td><td>{}</td><td>{}</td><td>{}</td><td>{} finalized / {} active</td></tr>",
            escape_html(row.project.as_deref().unwrap_or("-")),
            escape_html(row.provider.as_deref().unwrap_or("-")),
            escape_html(row.model.as_deref().unwrap_or("-")),
            escape_html(row.subject.as_deref().unwrap_or("-")),
            format_money(row.total_cost_usd),
            compact_number(row.total_tokens),
            row.finalized_reservations,
            row.active_reservations
        );
    }
    html.push_str("</tbody></table></section>");
}

fn protected_adoption_panel(html: &mut String, usage: &UsageReport) {
    let Some(adoption) = &usage.protected_adoption else {
        return;
    };
    html.push_str("<section class=\"panel\"><h2>Adoption health</h2>");
    let _ = write!(
        html,
        "<div class=\"legend\"><span>Protected opportunity {}</span><span>Carryover liability {}</span><span>Low adopters {}</span><span>Top consumers {}</span></div>",
        format_money(adoption.unused_protected_opportunity_usd),
        format_money(adoption.carryover_liability_usd),
        adoption.low_adopters.len(),
        adoption.high_adopters.len()
    );

    html.push_str("<h3>Low adopters</h3>");
    if adoption.low_adopters.is_empty() {
        html.push_str(
            "<div class=\"empty\">No low adopters were detected in the protected adoption buckets.</div>",
        );
    } else {
        html.push_str("<table><thead><tr><th>Budget</th><th>Entity</th><th>Protected opportunity</th><th>Carryover</th><th>Current usage</th></tr></thead><tbody>");
        for entity in &adoption.low_adopters {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&entity.budget_id),
                escape_html(&entity.entity_key),
                format_money(entity.current_grant_usd),
                format_money(entity.carryover_usd),
                format_money(entity.used_current_grant_usd)
            );
        }
        html.push_str("</tbody></table>");
    }

    html.push_str("<h3>Top consumers</h3>");
    if adoption.high_adopters.is_empty() {
        html.push_str(
            "<div class=\"empty\">No high protected-budget consumers were detected yet.</div>",
        );
    } else {
        html.push_str("<table><thead><tr><th>Budget</th><th>Entity</th><th>Used current grant</th><th>Carryover</th><th>Protected amount</th></tr></thead><tbody>");
        for entity in &adoption.high_adopters {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&entity.budget_id),
                escape_html(&entity.entity_key),
                format_money(entity.used_current_grant_usd),
                format_money(entity.carryover_usd),
                format_money(entity.protected_amount_usd)
            );
        }
        html.push_str("</tbody></table>");
    }
    html.push_str("</section>");
}

fn tools_panel(html: &mut String, activity: &[&TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Tool usage</h2>");
    let tools: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_tool_kind(&item.kind))
        .collect();
    if tools.is_empty() {
        html.push_str(
            "<div class=\"empty\">No tool calls or tool results were observed for this run yet. If Pi did not use tools, this is expected.</div></section>",
        );
        return;
    }
    html.push_str("<table><thead><tr><th>When</th><th>Tool event</th><th>What happened</th></tr></thead><tbody>");
    for item in tools {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn agent_activity_panel(html: &mut String, activity: &[&TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Agent activity</h2>");
    let agent_events: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_agent_kind(&item.kind))
        .collect();
    if agent_events.is_empty() {
        html.push_str(
            "<div class=\"empty\">No Pi agent lifecycle events were observed yet. This usually means the run came from the vertical demo or Pi did not emit lifecycle hooks for this trace.</div></section>",
        );
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Agent event</th><th>Signal</th></tr></thead><tbody>",
    );
    for item in agent_events {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn skill_context_panel(html: &mut String, activity: &[&TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Skills and context</h2>");
    let context_events: Vec<_> = activity
        .iter()
        .copied()
        .filter(|item| is_skill_context_kind(&item.kind))
        .collect();
    if context_events.is_empty() {
        html.push_str(
            "<div class=\"empty\">No skill/context event was observed yet. When Pi provides agent context, this section will show selected tools, skills, and context-file summaries without prompt text.</div></section>",
        );
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Context event</th><th>Summary</th></tr></thead><tbody>",
    );
    for item in context_events {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn decisions_panel(html: &mut String, decisions: &[TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Recent decisions</h2>");
    if decisions.is_empty() {
        html.push_str("<div class=\"empty\">No authorization decisions yet.</div></section>");
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Outcome</th><th>Summary</th></tr></thead><tbody>",
    );
    for item in decisions.iter().take(8) {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            outcome_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn risky_runs_panel(html: &mut String, decisions: &[TraceReportItem]) {
    html.push_str("<section class=\"panel\"><h2>Risky runs</h2>");
    let risky: Vec<_> = decisions
        .iter()
        .filter(|item| {
            item.guard_hits
                .as_ref()
                .is_some_and(|hits| !hits.is_empty())
        })
        .collect();
    if risky.is_empty() {
        html.push_str(
            "<div class=\"empty\">No guard hits were recorded for recent decisions.</div></section>",
        );
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Outcome</th><th>Guard hits</th></tr></thead><tbody>",
    );
    for item in risky {
        let hits = item
            .guard_hits
            .as_ref()
            .into_iter()
            .flatten()
            .map(|hit| format!("{}: {}", hit.rule_id, hit.reason))
            .collect::<Vec<_>>()
            .join("; ");
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            outcome_pill(&item.kind),
            escape_html(&hits)
        );
    }
    html.push_str("</tbody></table></section>");
}

fn lifecycle_guardrails_panel(html: &mut String, trace: Option<&TraceReport>) {
    html.push_str("<section class=\"panel\"><h2>Lifecycle guardrails</h2>");
    let items: Vec<&TraceReportItem> = trace
        .map(|trace| {
            trace
                .items
                .iter()
                .filter(|item| item.kind.starts_with("guard.report_only."))
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        html.push_str(
            "<div class=\"empty\">No lifecycle-backed report-only guard detections were recorded.</div></section>",
        );
        return;
    }
    html.push_str(
        "<table><thead><tr><th>When</th><th>Lifecycle guard</th><th>Detection</th></tr></thead><tbody>",
    );
    for item in items {
        let _ = write!(
            html,
            "<tr><td>{}</td><td>{}</td><td class=\"summary\">{}</td></tr>",
            escape_html(&short_time(item)),
            event_pill(&item.kind),
            escape_html(&item.summary)
        );
    }
    html.push_str("</tbody></table></section>");
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

fn short_time(item: &TraceReportItem) -> String {
    item.occurred_at.format("%Y-%m-%d %H:%M:%S").to_string()
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

    fn compare_checked_in_simulation(
        path: &str,
    ) -> crate::simulation::SimulationComparisonReport {
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
            routing: None,
            guard_hits: None,
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
        assert!(html.contains("Adoption health"));
    }

    #[test]
    fn dashboard_renders_risky_run_section_for_guard_hits() {
        let usage = UsageReport {
            total_cost_usd: 0.0,
            rows: Vec::new(),
            protected_adoption: None,
        };
        let decisions = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "decision.deny".to_owned(),
            summary: "decision_id=dec_guard guard_hits=dev-budget.max_context_tokens".to_owned(),
            routing: None,
            guard_hits: Some(vec![crate::ledger::DecisionGuardHitReport {
                rule_id: "dev-budget.max_context_tokens".to_owned(),
                reason: "estimated context tokens 1200 exceed enforced guard max 1000".to_owned(),
                severity: crate::contract::DecisionSeverity::Deny,
            }]),
        }];

        let html = render_dashboard(&usage, &decisions, None, &[]);

        assert!(html.contains("Risky runs"));
        assert!(html.contains("dev-budget.max_context_tokens"));
    }

    #[test]
    fn dashboard_renders_lifecycle_guardrail_section() {
        let usage = UsageReport {
            total_cost_usd: 0.0,
            rows: Vec::new(),
            protected_adoption: None,
        };
        let trace = TraceReport {
            trace_id: "trace-lifecycle".to_owned(),
            items: vec![TraceReportItem {
                occurred_at: Utc::now(),
                kind: "guard.report_only.tool_calls".to_owned(),
                summary: "tool_calls=12 max_tool_calls=10 reporting_only=true source=pi.tool_call"
                    .to_owned(),
                routing: None,
                guard_hits: None,
            }],
        };

        let html = render_dashboard(&usage, &[], Some(&trace), &[]);

        assert!(html.contains("Lifecycle guardrails"));
        assert!(html.contains("guard.report_only.tool_calls"));
    }

    #[test]
    fn trace_report_human_output_has_stable_header_and_rows() {
        let report = TraceReport {
            trace_id: "trace-1".to_owned(),
            items: vec![TraceReportItem {
                occurred_at: Utc::now(),
                kind: "decision.allow".to_owned(),
                summary: "decision_id=dec_1".to_owned(),
                routing: None,
                guard_hits: None,
            }],
        };

        let lines = render_trace_report_lines(&report);

        assert_eq!(lines[0], "trace\ttrace-1");
        assert_eq!(lines[1], "occurred_at\tkind\tsummary");
        assert!(lines[2].contains("\tdecision.allow\tdecision_id=dec_1"));
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
            routing: None,
            guard_hits: None,
        }];
        let trace = TraceReport {
            trace_id: "trace-1".to_owned(),
            items: vec![TraceReportItem {
                occurred_at: Utc::now(),
                kind: "tool.observed".to_owned(),
                summary: "name=bash success=true".to_owned(),
                routing: None,
                guard_hits: None,
            }],
        };
        let observations = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "pi.turn_end".to_owned(),
            summary: "turn=1".to_owned(),
            routing: None,
            guard_hits: None,
        }];

        let html = render_dashboard(&usage, &decisions, Some(&trace), &observations);

        for marker in [
            "Spend",
            "Tokens",
            "Recent decisions",
            "Tool usage",
            "Agent activity",
            "Run timeline",
        ] {
            assert!(html.contains(marker), "missing dashboard marker: {marker}");
        }
    }

    #[test]
    fn simulation_dashboard_renders_showcase_tradeoff_markers() {
        let runaway_report = compare_checked_in_simulation(
            "examples/simulations/runaway-pressure.noet.yaml",
        );
        let runaway_html = render_simulation_dashboard(&runaway_report);
        assert!(runaway_html.contains("Comparison summary"));
        assert!(runaway_html.contains("Guardrails changed the budget story"));
        assert!(runaway_html.contains("guarded team budget blocked 107 guarded requests"));
        assert!(runaway_html.contains("pooled without guard exhausted shared budget on day 3."));

        let adoption_report = compare_checked_in_simulation(
            "examples/simulations/adoption-pressure.noet.yaml",
        );
        let adoption_html = render_simulation_dashboard(&adoption_report);
        assert!(adoption_html.contains("Comparison summary"));
        assert!(adoption_html.contains("Adoption policy changed what the team could see"));
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
            routing: None,
            guard_hits: None,
        }];
        let trace = TraceReport {
            trace_id: "trace-pi".to_owned(),
            items: vec![
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.agent_context".to_owned(),
                    summary: "selected_tools=read,bash skills=diagnose context_files=AGENTS.md".to_owned(),
                    routing: None,
                    guard_hits: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.provider_call.started".to_owned(),
                    summary: "provider=openai-codex model=gpt-demo shape=input_count=1".to_owned(),
                    routing: None,
                    guard_hits: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.tool_call".to_owned(),
                    summary: "tool_name=bash input_summary.command.length=42".to_owned(),
                    routing: None,
                    guard_hits: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "tool.observed".to_owned(),
                    summary: "name=bash success=true duration_ms=42".to_owned(),
                    routing: None,
                    guard_hits: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.message_end".to_owned(),
                    summary: "provider=openai-codex model=gpt-demo tokens=1080 cost=0.001900".to_owned(),
                    routing: None,
                    guard_hits: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.turn_end".to_owned(),
                    summary: "turn=1 usage=(provider=openai-codex model=gpt-demo tokens=1080 cost=0.001900)".to_owned(),
                    routing: None,
                    guard_hits: None,
                },
                TraceReportItem {
                    occurred_at: now,
                    kind: "pi.agent_end".to_owned(),
                    summary: "messages=2".to_owned(),
                    routing: None,
                    guard_hits: None,
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
    }

    #[test]
    fn items_report_human_output_has_stable_header_and_rows() {
        let items = vec![TraceReportItem {
            occurred_at: Utc::now(),
            kind: "tool.observed".to_owned(),
            summary: "name=bash success=true".to_owned(),
            routing: None,
            guard_hits: None,
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
                assert_eq!(args.db_path, PathBuf::from(".noet/noether.sqlite"));
                assert!(args.upstream.is_none());
                assert!(args.routes.is_none());
                assert!(args.policy.is_none());
                assert_eq!(args.decision_mode, DecisionMode::DryRun);
            }
            _ => panic!("expected serve command"),
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
      limit_usd: 10
      eligible:
        entities: [project:noether]
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
      limit_usd: 10
      eligible:
        entities: [project:noether]
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
    async fn scenario_run_supports_fallback_denial_and_guard_hit_assertions() {
        let tempdir = tempdir().expect("tempdir");
        let scenario_path = tempdir.path().join("routing-and-guards.noet.yaml");
        let out_dir = tempdir.path().join("artifacts");
        std::fs::write(
            &scenario_path,
            r#"
version: 1
name: routing and guards
policy:
  version: 0
  budgets:
    - id: project-budget
      limit_usd: 10
      eligible:
        entities: [project:noether]
    - id: team-budget
      limit_usd: 20
      eligible:
        entities: [team:eng]
    - id: guard-budget
      limit_usd: 5
      eligible:
        entities: [project:guarded]
      guards:
        max_context_tokens:
          max_tokens: 1000
          effect: deny
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
  - id: req-guard
    authorize:
      project: guarded
      provider: openai
      model: gpt-4.1
      estimated_tokens: 1200
      entities: [project:guarded]
    denial:
      rule_id: guard-budget.max_context_tokens
      reason_contains: exceed enforced guard max 1000
assertions:
  - kind: fallback
    request_id: req-fallback
    requested_budget_id: missing-budget
    selected_budget_id: project-budget
    matched_entity: project:noether
  - kind: denied
    request_id: req-guard
  - kind: guard_hit
    request_id: req-guard
    rule_id: guard-budget.max_context_tokens
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
            "examples/scenarios/runaway-agent-guard.noet.yaml",
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
      limit_usd: 10
      eligible:
        entities: [project:noether]
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
            assert_eq!(strategy["guard_hit_count"], 0);
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
}
