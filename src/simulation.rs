use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, BudgetRule, DecisionOutcome, FinalizeReservation,
    RuleMatch, UsageObservation,
};
use crate::error::NoetError;
use crate::ledger::{
    AsyncPostgresLedgerOptions, BudgetLedger, SimulationLedgerBatch, TraceReportItem, UsageReport,
    connect_async_postgres_client,
};
use crate::policy::{PolicyFile, validate_policy};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationFile {
    pub version: u16,
    #[serde(default)]
    pub name: Option<String>,
    pub seed: u64,
    pub horizon_days: u32,
    pub company: SyntheticCompany,
    #[serde(default)]
    pub models: Vec<SimulationModel>,
    #[serde(default)]
    pub strategies: Vec<SimulationStrategy>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyntheticCompany {
    pub id: String,
    #[serde(default)]
    pub teams: Vec<SyntheticTeam>,
    #[serde(default)]
    pub projects: Vec<SyntheticProject>,
    #[serde(default)]
    pub users: Vec<SyntheticUser>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyntheticTeam {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyntheticProject {
    pub id: String,
    pub team_id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyntheticUser {
    pub id: String,
    pub team_id: String,
    #[serde(default)]
    pub project_ids: Vec<String>,
    pub profile: UsageProfile,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageProfile {
    PowerUser,
    SteadyUser,
    LowAdopter,
    Experimenter,
    LoopRisk,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationModel {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub cost_per_1k_tokens_usd: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationStrategy {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    pub policy: PolicyFile,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SyntheticDemandRequest {
    pub request_id: String,
    pub day_index: u32,
    pub subject: String,
    pub team_id: String,
    pub project_id: String,
    pub profile: UsageProfile,
    pub model_id: String,
    pub provider: String,
    pub model: String,
    pub estimated_tokens: u64,
    pub estimated_cost_usd: f64,
    pub tool_call_count: u32,
    pub useful_work_score: u32,
    pub loop_risk: bool,
    pub entities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationComparisonReport {
    pub name: Option<String>,
    pub seed: u64,
    pub horizon_days: u32,
    pub total_requests: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<SimulationTimingReport>,
    pub strategies: Vec<SimulationStrategyReport>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationStrategyReport {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub policy_moves: Vec<String>,
    pub total_requests: u64,
    pub allowed_requests: u64,
    pub warned_requests: u64,
    pub denied_requests: u64,
    pub fallback_count: u64,
    pub limit_hit_count: u64,
    pub total_cost_usd: f64,
    pub unused_budget_usd: f64,
    pub useful_work_blocked_score: u64,
    pub runaway_spend_prevented_usd: f64,
    pub adoption_coverage: f64,
    pub fairness_score: f64,
    pub unused_protected_opportunity_usd: f64,
    pub low_adopter_count: u64,
    pub high_adopter_count: u64,
    pub model_mix: Vec<SimulationModelMixEntry>,
    pub carryover_liability_usd: f64,
    pub exhaustion_day: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<SimulationDatabaseLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<SimulationStrategyTimingReport>,
    pub db_path: PathBuf,
    pub usage_report_path: PathBuf,
    pub decisions_report_path: PathBuf,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SimulationTimingReport {
    pub total_ms: f64,
    pub generate_demand_ms: f64,
    pub strategies_ms: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SimulationStrategyTimingReport {
    pub total_ms: f64,
    pub init_ms: f64,
    pub replay_ms: f64,
    #[serde(default)]
    pub persist_ms: f64,
    pub report_ms: f64,
    pub artifact_ms: f64,
}

impl SimulationStrategyReport {
    pub fn database_location(&self) -> SimulationDatabaseLocation {
        self.database
            .clone()
            .unwrap_or_else(|| SimulationDatabaseLocation::Sqlite {
                path: self.db_path.clone(),
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum SimulationDatabaseLocation {
    Sqlite { path: PathBuf },
    Postgres { url: String },
}

impl SimulationDatabaseLocation {
    pub fn cli_lines(&self, out_dir: &Path) -> Vec<(&'static str, String)> {
        match self {
            Self::Sqlite { path } => vec![
                ("db_backend", "sqlite".to_owned()),
                ("db_path", out_dir.join(path).display().to_string()),
            ],
            Self::Postgres { url } => vec![
                ("db_backend", "postgres".to_owned()),
                ("db_url", url.clone()),
            ],
        }
    }
}

#[derive(Clone)]
pub struct SimulationDatabase {
    backend: Arc<dyn SimulationBackend>,
}

impl std::fmt::Debug for SimulationDatabase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SimulationDatabase")
            .field("backend", &self.backend.name())
            .finish()
    }
}

impl SimulationDatabase {
    pub fn sqlite() -> Self {
        Self {
            backend: Arc::new(SqliteSimulationBackend),
        }
    }

    pub fn postgres(database_url: String, options: AsyncPostgresLedgerOptions) -> Self {
        Self {
            backend: Arc::new(PostgresSimulationBackend {
                database_url,
                options,
            }),
        }
    }
}

type SimulationBackendFuture =
    Pin<Box<dyn Future<Output = Result<SimulationStrategyReport, NoetError>> + Send>>;

trait SimulationBackend: Send + Sync {
    fn name(&self) -> &'static str;

    fn run_strategy(&self, context: SimulationStrategyContext) -> SimulationBackendFuture;
}

struct SimulationStrategyContext {
    file: Arc<SimulationFile>,
    strategy: SimulationStrategy,
    demand: Arc<Vec<SyntheticDemandRequest>>,
    out_dir: PathBuf,
    strategy_dir_relative: PathBuf,
}

struct SqliteSimulationBackend;

struct PostgresSimulationBackend {
    database_url: String,
    options: AsyncPostgresLedgerOptions,
}

impl SimulationBackend for SqliteSimulationBackend {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn run_strategy(&self, context: SimulationStrategyContext) -> SimulationBackendFuture {
        Box::pin(async move { run_sqlite_strategy(context) })
    }
}

impl SimulationBackend for PostgresSimulationBackend {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn run_strategy(&self, context: SimulationStrategyContext) -> SimulationBackendFuture {
        let database_url = self.database_url.clone();
        let options = self.options.clone();
        Box::pin(async move { run_postgres_strategy(context, database_url, options).await })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationModelMixEntry {
    pub model_id: String,
    pub requests: u64,
    pub total_cost_usd: f64,
}

pub fn validate_simulation(file: &SimulationFile) -> Result<(), NoetError> {
    let mut errors = Vec::new();
    if file.version != 1 {
        errors.push(format!(
            "simulation version must be 1, got {}",
            file.version
        ));
    }
    if file.company.id.trim().is_empty() {
        errors.push("simulation company id must not be empty".to_owned());
    }
    if file.horizon_days == 0 {
        errors.push("simulation horizon_days must be greater than zero".to_owned());
    }
    if file.company.teams.is_empty() {
        errors.push("simulation must define at least one team".to_owned());
    }
    if file.company.projects.is_empty() {
        errors.push("simulation must define at least one project".to_owned());
    }
    if file.company.users.is_empty() {
        errors.push("simulation must define at least one user".to_owned());
    }
    if file.models.is_empty() {
        errors.push("simulation must define at least one model".to_owned());
    }
    if file.strategies.is_empty() {
        errors.push("simulation must define at least one strategy".to_owned());
    }

    let team_ids = collect_ids(
        file.company.teams.iter().map(|team| team.id.as_str()),
        "team",
        &mut errors,
    );
    let project_ids = collect_ids(
        file.company
            .projects
            .iter()
            .map(|project| project.id.as_str()),
        "project",
        &mut errors,
    );
    let _user_ids = collect_ids(
        file.company.users.iter().map(|user| user.id.as_str()),
        "user",
        &mut errors,
    );
    let _model_ids = collect_ids(
        file.models.iter().map(|model| model.id.as_str()),
        "model",
        &mut errors,
    );
    let _strategy_ids = collect_ids(
        file.strategies.iter().map(|strategy| strategy.id.as_str()),
        "strategy",
        &mut errors,
    );

    for project in &file.company.projects {
        if !team_ids.contains(project.team_id.as_str()) {
            errors.push(format!(
                "simulation project {} references unknown team {}",
                project.id, project.team_id
            ));
        }
    }

    for user in &file.company.users {
        if !team_ids.contains(user.team_id.as_str()) {
            errors.push(format!(
                "simulation user {} references unknown team {}",
                user.id, user.team_id
            ));
        }
        if user.project_ids.is_empty() {
            errors.push(format!(
                "simulation user {} must reference at least one project",
                user.id
            ));
        }
        for project_id in &user.project_ids {
            if !project_ids.contains(project_id.as_str()) {
                errors.push(format!(
                    "simulation user {} references unknown project {}",
                    user.id, project_id
                ));
            }
        }
    }

    for model in &file.models {
        if model.provider.trim().is_empty() {
            errors.push(format!(
                "simulation model {} must not have an empty provider",
                model.id
            ));
        }
        if model.model.trim().is_empty() {
            errors.push(format!(
                "simulation model {} must not have an empty model name",
                model.id
            ));
        }
        if model.cost_per_1k_tokens_usd <= 0.0 {
            errors.push(format!(
                "simulation model {} cost_per_1k_tokens_usd must be positive",
                model.id
            ));
        }
    }

    for strategy in &file.strategies {
        if let Err(error) = validate_policy(&strategy.policy) {
            errors.push(format!(
                "simulation strategy {} policy invalid: {error}",
                strategy.id
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(NoetError::InvalidConfig(errors.join("; ")))
    }
}

pub fn generate_synthetic_demand(
    file: &SimulationFile,
) -> Result<Vec<SyntheticDemandRequest>, NoetError> {
    validate_simulation(file)?;

    let mut rng = DeterministicRng::new(file.seed);
    let mut requests = Vec::new();
    for day_index in 0..file.horizon_days {
        for user in &file.company.users {
            let request_count = profile_request_count(user.profile, &mut rng, day_index);
            for request_index in 0..request_count {
                let project_id = user.project_ids
                    [rng.next_bounded(user.project_ids.len() as u64) as usize]
                    .clone();
                let model = choose_model(file, user.profile, &mut rng);
                let estimated_tokens =
                    profile_tokens(user.profile, &mut rng, day_index, request_index);
                let tool_call_count = profile_tool_calls(user.profile, &mut rng, request_index);
                let useful_work_score =
                    profile_useful_work_score(user.profile, &mut rng, tool_call_count);
                let estimated_cost_usd =
                    (estimated_tokens as f64 / 1000.0) * model.cost_per_1k_tokens_usd;
                requests.push(SyntheticDemandRequest {
                    request_id: format!("day-{day_index:02}-{}-{}", user.id, request_index + 1),
                    day_index,
                    subject: format!("user:{}", user.id),
                    team_id: user.team_id.clone(),
                    project_id: project_id.clone(),
                    profile: user.profile,
                    model_id: model.id.clone(),
                    provider: model.provider.clone(),
                    model: model.model.clone(),
                    estimated_tokens,
                    estimated_cost_usd,
                    tool_call_count,
                    useful_work_score,
                    loop_risk: matches!(user.profile, UsageProfile::LoopRisk)
                        && (tool_call_count >= 6 || estimated_tokens >= 40_000),
                    entities: vec![
                        format!("team:{}", user.team_id),
                        format!("project:{project_id}"),
                        format!("user:{}", user.id),
                    ],
                });
            }
        }
    }
    Ok(requests)
}

fn initial_strategy_report(
    strategy: &SimulationStrategy,
    total_requests: usize,
    strategy_dir_relative: &Path,
    database: SimulationDatabaseLocation,
    db_path: PathBuf,
) -> SimulationStrategyReport {
    SimulationStrategyReport {
        id: strategy.id.clone(),
        description: strategy.description.clone(),
        policy_moves: strategy_policy_moves(&strategy.policy),
        total_requests: total_requests as u64,
        allowed_requests: 0,
        warned_requests: 0,
        denied_requests: 0,
        fallback_count: 0,
        limit_hit_count: 0,
        total_cost_usd: 0.0,
        unused_budget_usd: 0.0,
        useful_work_blocked_score: 0,
        runaway_spend_prevented_usd: 0.0,
        adoption_coverage: 0.0,
        fairness_score: 0.0,
        unused_protected_opportunity_usd: 0.0,
        low_adopter_count: 0,
        high_adopter_count: 0,
        model_mix: Vec::new(),
        carryover_liability_usd: 0.0,
        exhaustion_day: None,
        database: Some(database),
        timing: None,
        db_path,
        usage_report_path: strategy_dir_relative.join("usage-report.json"),
        decisions_report_path: strategy_dir_relative.join("decisions-report.json"),
    }
}

#[derive(Default)]
struct SimulationStrategyTotals {
    users_with_access: BTreeSet<String>,
    user_spend: BTreeMap<String, f64>,
    model_mix: BTreeMap<String, (u64, f64)>,
}

fn apply_authorize_decision_to_report(
    report: &mut SimulationStrategyReport,
    totals: &mut SimulationStrategyTotals,
    request: &SyntheticDemandRequest,
    decision: &AuthorizeDecision,
) -> bool {
    let limit_hit_count = decision
        .explanations
        .iter()
        .filter(|explanation| is_limit_rule_id(&explanation.rule_id))
        .count() as u64;
    if decision
        .explanations
        .iter()
        .any(|explanation| explanation.reason.starts_with("selected fallback budget"))
    {
        report.fallback_count += 1;
    }
    report.limit_hit_count += limit_hit_count;
    match decision.outcome {
        DecisionOutcome::Allow => {
            report.allowed_requests += 1;
            totals.users_with_access.insert(request.subject.clone());
            true
        }
        DecisionOutcome::Warn => {
            report.warned_requests += 1;
            totals.users_with_access.insert(request.subject.clone());
            true
        }
        DecisionOutcome::Deny => {
            report.denied_requests += 1;
            report.useful_work_blocked_score += request.useful_work_score as u64;
            if request.loop_risk || limit_hit_count > 0 {
                report.runaway_spend_prevented_usd += request.estimated_cost_usd;
            }
            if report.exhaustion_day.is_none()
                && decision.explanations.iter().any(|explanation| {
                    explanation.reason.contains("fixed-window limit")
                        || explanation.rule_id == "no_fallback_budget"
                })
            {
                report.exhaustion_day = Some(request.day_index);
            }
            false
        }
    }
}

fn simulation_finalize_payload(
    request: &SyntheticDemandRequest,
    strategy_id: &str,
) -> FinalizeReservation {
    FinalizeReservation {
        reservation_id: None,
        outcome: crate::contract::FinalizeOutcome::Success,
        usage: Some(UsageObservation {
            provider: Some(request.provider.clone()),
            model: Some(request.model.clone()),
            input_tokens: Some(request.estimated_tokens * 3 / 5),
            output_tokens: Some(request.estimated_tokens * 2 / 5),
            total_tokens: Some(request.estimated_tokens),
            cost_usd: Some(request.estimated_cost_usd),
            latency_ms: Some(500 + request.tool_call_count as u64 * 50),
            stop_reason: Some("stop".to_owned()),
        }),
        actual_cost_usd: Some(request.estimated_cost_usd),
        metadata: BTreeMap::from([
            (
                "trace_id".to_owned(),
                serde_json::Value::String(format!("{}:{}", strategy_id, request.request_id)),
            ),
            (
                "request_id".to_owned(),
                serde_json::Value::String(request.request_id.clone()),
            ),
            (
                "source".to_owned(),
                serde_json::Value::String("simulation".to_owned()),
            ),
        ]),
    }
}

fn record_finalized_simulation_usage(
    totals: &mut SimulationStrategyTotals,
    request: &SyntheticDemandRequest,
) {
    *totals
        .user_spend
        .entry(request.subject.clone())
        .or_insert(0.0) += request.estimated_cost_usd;
    let entry = totals
        .model_mix
        .entry(request.model_id.clone())
        .or_insert((0_u64, 0.0_f64));
    entry.0 += 1;
    entry.1 += request.estimated_cost_usd;
}

fn finish_strategy_report(
    file: &SimulationFile,
    strategy: &SimulationStrategy,
    report: &mut SimulationStrategyReport,
    usage: &UsageReport,
    totals: SimulationStrategyTotals,
) {
    report.total_cost_usd = usage.total_cost_usd;
    report.unused_budget_usd = (strategy
        .policy
        .budgets
        .iter()
        .filter_map(budget_total_cap_usd)
        .sum::<f64>()
        - usage.total_cost_usd)
        .max(0.0);
    report.adoption_coverage = if file.company.users.is_empty() {
        0.0
    } else {
        totals.users_with_access.len() as f64 / file.company.users.len() as f64
    };
    report.fairness_score = fairness_score(&file.company.users, &totals.user_spend);
    report.model_mix = totals
        .model_mix
        .into_iter()
        .map(
            |(model_id, (requests, total_cost_usd))| SimulationModelMixEntry {
                model_id,
                requests,
                total_cost_usd,
            },
        )
        .collect();
    report.model_mix.sort_by(|left, right| {
        right
            .total_cost_usd
            .partial_cmp(&left.total_cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.model_id.cmp(&right.model_id))
    });
    report.carryover_liability_usd = usage
        .protected_adoption
        .as_ref()
        .map(|adoption| adoption.carryover_liability_usd)
        .unwrap_or_default();
    report.unused_protected_opportunity_usd = usage
        .protected_adoption
        .as_ref()
        .map(|adoption| adoption.unused_protected_opportunity_usd)
        .unwrap_or_default();
    report.low_adopter_count = usage
        .protected_adoption
        .as_ref()
        .map(|adoption| adoption.low_adopters.len() as u64)
        .unwrap_or_default();
    report.high_adopter_count = usage
        .protected_adoption
        .as_ref()
        .map(|adoption| adoption.high_adopters.len() as u64)
        .unwrap_or_default();
}

pub fn compare_strategies(
    file: &SimulationFile,
    out_dir: &Path,
) -> Result<SimulationComparisonReport, NoetError> {
    validate_simulation(file)?;
    std::fs::create_dir_all(out_dir)?;

    let total_started = std::time::Instant::now();
    let demand_started = std::time::Instant::now();
    let demand = generate_synthetic_demand(file)?;
    let generate_demand_ms = elapsed_ms(demand_started);
    let strategies_started = std::time::Instant::now();
    let strategies_dir = out_dir.join("strategies");
    std::fs::create_dir_all(&strategies_dir)?;
    let mut strategies = Vec::new();
    let file = Arc::new(file.clone());
    let demand = Arc::new(demand);

    for strategy in &file.strategies {
        let strategy_slug = encode_path_component(&strategy.id, "simulation");
        let strategy_dir_relative = PathBuf::from("strategies").join(&strategy_slug);
        strategies.push(run_sqlite_strategy(SimulationStrategyContext {
            file: Arc::clone(&file),
            strategy: strategy.clone(),
            demand: Arc::clone(&demand),
            out_dir: out_dir.to_path_buf(),
            strategy_dir_relative,
        })?);
    }
    let strategies_ms = elapsed_ms(strategies_started);

    Ok(SimulationComparisonReport {
        name: file.name.clone(),
        seed: file.seed,
        horizon_days: file.horizon_days,
        total_requests: demand.len() as u64,
        timing: Some(SimulationTimingReport {
            total_ms: elapsed_ms(total_started),
            generate_demand_ms,
            strategies_ms,
        }),
        strategies,
    })
}

pub async fn compare_strategies_with_database(
    file: &SimulationFile,
    out_dir: &Path,
    database: SimulationDatabase,
) -> Result<SimulationComparisonReport, NoetError> {
    validate_simulation(file)?;
    std::fs::create_dir_all(out_dir)?;

    let total_started = std::time::Instant::now();
    let demand_started = std::time::Instant::now();
    let demand = generate_synthetic_demand(file)?;
    let generate_demand_ms = elapsed_ms(demand_started);
    let strategies_started = std::time::Instant::now();
    let strategies_dir = out_dir.join("strategies");
    std::fs::create_dir_all(&strategies_dir)?;
    let file = Arc::new(file.clone());
    let demand = Arc::new(demand);
    let mut tasks = tokio::task::JoinSet::new();

    for (index, strategy) in file.strategies.iter().cloned().enumerate() {
        let strategy_slug = encode_path_component(&strategy.id, "simulation");
        let strategy_dir_relative = PathBuf::from("strategies").join(&strategy_slug);
        let backend = Arc::clone(&database.backend);
        let file = Arc::clone(&file);
        let demand = Arc::clone(&demand);
        let out_dir = out_dir.to_path_buf();
        tasks.spawn(async move {
            let report = backend
                .run_strategy(SimulationStrategyContext {
                    file,
                    strategy,
                    demand,
                    out_dir,
                    strategy_dir_relative,
                })
                .await?;
            Ok::<_, NoetError>((index, report))
        });
    }

    let mut strategies = Vec::new();
    while let Some(result) = tasks.join_next().await {
        strategies.push(result.map_err(|error| {
            NoetError::InvalidConfig(format!("simulation strategy task failed: {error}"))
        })??);
    }
    strategies.sort_by_key(|(index, _)| *index);
    let strategies = strategies
        .into_iter()
        .map(|(_, strategy)| strategy)
        .collect::<Vec<_>>();
    let strategies_ms = elapsed_ms(strategies_started);

    Ok(SimulationComparisonReport {
        name: file.name.clone(),
        seed: file.seed,
        horizon_days: file.horizon_days,
        total_requests: demand.len() as u64,
        timing: Some(SimulationTimingReport {
            total_ms: elapsed_ms(total_started),
            generate_demand_ms,
            strategies_ms,
        }),
        strategies,
    })
}

fn elapsed_ms(started: std::time::Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn run_sqlite_strategy(
    context: SimulationStrategyContext,
) -> Result<SimulationStrategyReport, NoetError> {
    let total_started = std::time::Instant::now();
    let init_started = std::time::Instant::now();
    let strategy_dir = context.out_dir.join(&context.strategy_dir_relative);
    std::fs::create_dir_all(&strategy_dir)?;
    let db_path = strategy_dir.join("simulation.sqlite");
    if db_path.exists() {
        std::fs::remove_file(&db_path)?;
    }

    let mut ledger = BudgetLedger::open_sqlite(&db_path)?;
    let init_ms = elapsed_ms(init_started);
    let mut report = initial_strategy_report(
        &context.strategy,
        context.demand.len(),
        &context.strategy_dir_relative,
        SimulationDatabaseLocation::Sqlite {
            path: context.strategy_dir_relative.join("simulation.sqlite"),
        },
        context.strategy_dir_relative.join("simulation.sqlite"),
    );
    let mut totals = SimulationStrategyTotals::default();

    let replay_started = std::time::Instant::now();
    for request in context.demand.iter() {
        let authorize = synthetic_authorize_request(request, &context.strategy.id);
        let decision = ledger.try_authorize(Some(&context.strategy.policy), &authorize)?;
        if !apply_authorize_decision_to_report(&mut report, &mut totals, request, &decision) {
            continue;
        }

        if let Some(reservation) = &decision.reservation {
            let finalize = simulation_finalize_payload(request, &context.strategy.id);
            let _ = ledger.finalize(&reservation.id, &finalize)?;
            record_finalized_simulation_usage(&mut totals, request);
        }
    }
    let replay_ms = elapsed_ms(replay_started);

    let report_started = std::time::Instant::now();
    let usage = ledger.usage_report()?;
    let decisions = ledger.decisions_report()?;
    finish_strategy_report(
        &context.file,
        &context.strategy,
        &mut report,
        &usage,
        totals,
    );
    let report_ms = elapsed_ms(report_started);
    let artifact_started = std::time::Instant::now();
    write_strategy_artifacts(&context.out_dir, &report, &usage, &decisions)?;
    let artifact_ms = elapsed_ms(artifact_started);
    report.timing = Some(SimulationStrategyTimingReport {
        total_ms: elapsed_ms(total_started),
        init_ms,
        replay_ms,
        persist_ms: 0.0,
        report_ms,
        artifact_ms,
    });
    Ok(report)
}

async fn run_postgres_strategy(
    context: SimulationStrategyContext,
    database_url: String,
    _options: AsyncPostgresLedgerOptions,
) -> Result<SimulationStrategyReport, NoetError> {
    let total_started = std::time::Instant::now();
    let init_started = std::time::Instant::now();
    let strategy_slug = encode_path_component(&context.strategy.id, "simulation");
    let strategy_dir = context.out_dir.join(&context.strategy_dir_relative);
    std::fs::create_dir_all(&strategy_dir)?;

    let schema = simulation_postgres_schema(&strategy_slug);
    create_postgres_schema(&database_url, &schema).await?;
    let scoped_url = postgres_url_with_search_path(&database_url, &schema);
    tokio::task::spawn_blocking(move || {
        let mut ledger = BudgetLedger::default();
        let mut batch = SimulationLedgerBatch::default();
        let init_ms = elapsed_ms(init_started);
        let mut report = initial_strategy_report(
            &context.strategy,
            context.demand.len(),
            &context.strategy_dir_relative,
            SimulationDatabaseLocation::Postgres {
                url: redact_database_url(&scoped_url),
            },
            context.strategy_dir_relative.join("postgres"),
        );
        let mut totals = SimulationStrategyTotals::default();

        let replay_started = std::time::Instant::now();
        for request in context.demand.iter() {
            let authorize = synthetic_authorize_request(request, &context.strategy.id);
            let decision = ledger.try_authorize(Some(&context.strategy.policy), &authorize)?;
            ledger.capture_simulation_decision(
                &mut batch,
                Some(&context.strategy.policy),
                &authorize,
                &decision,
            )?;
            if !apply_authorize_decision_to_report(&mut report, &mut totals, request, &decision) {
                continue;
            }

            if let Some(reservation) = &decision.reservation {
                let finalize = simulation_finalize_payload(request, &context.strategy.id);
                let reservation = ledger.finalize(&reservation.id, &finalize)?;
                ledger.capture_simulation_finalization(&mut batch, &reservation, &finalize)?;
                record_finalized_simulation_usage(&mut totals, request);
            }
        }
        let replay_ms = elapsed_ms(replay_started);

        let persist_started = std::time::Instant::now();
        ledger.persist_simulation_batch_to_postgres(&scoped_url, &batch)?;
        let persist_ms = elapsed_ms(persist_started);

        let report_started = std::time::Instant::now();
        let report_ledger = BudgetLedger::open_postgres(&scoped_url)?;
        let usage = report_ledger.usage_report()?;
        let decisions = report_ledger.decisions_report()?;
        finish_strategy_report(
            &context.file,
            &context.strategy,
            &mut report,
            &usage,
            totals,
        );
        let report_ms = elapsed_ms(report_started);
        let artifact_started = std::time::Instant::now();
        write_strategy_artifacts(&context.out_dir, &report, &usage, &decisions)?;
        let artifact_ms = elapsed_ms(artifact_started);
        report.timing = Some(SimulationStrategyTimingReport {
            total_ms: elapsed_ms(total_started),
            init_ms,
            replay_ms,
            persist_ms,
            report_ms,
            artifact_ms,
        });
        Ok(report)
    })
    .await
    .map_err(|err| NoetError::InvalidConfig(format!("Postgres simulation task failed: {err}")))?
}

fn write_strategy_artifacts(
    out_dir: &Path,
    report: &SimulationStrategyReport,
    usage: &UsageReport,
    decisions: &[TraceReportItem],
) -> Result<(), NoetError> {
    std::fs::write(
        out_dir.join(&report.usage_report_path),
        serde_json::to_vec_pretty(usage)?,
    )?;
    std::fs::write(
        out_dir.join(&report.decisions_report_path),
        serde_json::to_vec_pretty(decisions)?,
    )?;
    Ok(())
}

async fn create_postgres_schema(database_url: &str, schema: &str) -> Result<(), NoetError> {
    let client = connect_async_postgres_client(database_url)
        .await
        .map_err(|err| NoetError::InvalidConfig(format!("PostgreSQL connection failed: {err}")))?;
    client
        .execute(
            &format!(
                "CREATE SCHEMA IF NOT EXISTS {}",
                postgres_ident_literal(schema)
            ),
            &[],
        )
        .await
        .map_err(|err| {
            NoetError::InvalidConfig(format!("PostgreSQL schema creation failed: {err}"))
        })?;
    drop(client);
    Ok(())
}

fn simulation_postgres_schema(strategy_slug: &str) -> String {
    let normalized = strategy_slug
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!(
        "noet_sim_{}_{}",
        normalized.trim_matches('_'),
        Uuid::new_v4().as_simple()
    )
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let separator = if database_url.contains('?') { '&' } else { '?' };
    format!("{database_url}{separator}options=-csearch_path%3D{schema}")
}

fn postgres_ident_literal(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn redact_database_url(database_url: &str) -> String {
    match url::Url::parse(database_url) {
        Ok(mut url) => {
            if url.password().is_some() {
                let _ = url.set_password(Some("redacted"));
            }
            url.to_string()
        }
        Err(_) => database_url.to_owned(),
    }
}

fn strategy_policy_moves(policy: &PolicyFile) -> Vec<String> {
    let budget_count = policy.budgets.len();
    let total_limit = policy
        .budgets
        .iter()
        .filter_map(budget_total_cap_usd)
        .sum::<f64>();
    let mut moves = vec![format!(
        "{} budget{} · ${total_limit:.2} total cap · scope {}",
        budget_count,
        if budget_count == 1 { "" } else { "s" },
        summarize_budget_scope(policy)
    )];
    let mut has_explicit_controls = false;

    for budget in &policy.budgets {
        if let Some(allocation) = &budget.allocation
            && allocation.standard == "protected_adoption_pool"
        {
            has_explicit_controls = true;
            let by = allocation.by.as_deref().unwrap_or("entity");
            let protected_amount = allocation.protected_amount_usd.unwrap_or_default();
            let window = allocation.window.as_deref().unwrap_or("window");
            let carryover = allocation
                .carryover
                .as_ref()
                .map(|carryover| {
                    format!(
                        "{:.0}% carryover capped at ${:.2}",
                        carryover.percent.unwrap_or_default(),
                        carryover.cap_usd.unwrap_or_default()
                    )
                })
                .unwrap_or_else(|| "no carryover".to_owned());
            moves.push(format!(
                "{} reserves ${protected_amount:.2} per {by} in each {window} window ({carryover}).",
                budget.id
            ));
        }

        if let Some(limit) = &budget.limits.request_cost {
            has_explicit_controls = true;
            moves.push(format!(
                "{} {} requests above ${:.2} estimated cost.",
                budget.id,
                limit_action_label(limit.action),
                limit.max_usd
            ));
        }

        if let Some(limit) = &budget.limits.context_tokens {
            has_explicit_controls = true;
            moves.push(format!(
                "{} {} requests above {} context tokens.",
                budget.id,
                limit_action_label(limit.action),
                limit.max_tokens
            ));
        }

        for limit in &budget.limits.spend {
            has_explicit_controls = true;
            moves.push(format!(
                "{} {} bursts above ${:.2} in {}.",
                budget.id,
                limit_action_label(limit.action),
                limit.max_usd,
                limit.window
            ));
        }

        if budget.limits.tool_calls.is_some()
            || budget.limits.agent_steps.is_some()
            || budget.limits.retries.is_some()
        {
            has_explicit_controls = true;
            let mut controls = Vec::new();
            if let Some(max_tool_calls) = budget.limits.tool_calls {
                controls.push(format!("tool calls <= {max_tool_calls}"));
            }
            if let Some(max_agent_steps) = budget.limits.agent_steps {
                controls.push(format!("agent steps <= {max_agent_steps}"));
            }
            if let Some(max_retries) = budget.limits.retries {
                controls.push(format!("retries <= {max_retries}"));
            }
            moves.push(format!("{} enforces {}.", budget.id, controls.join(", ")));
        }
    }

    if !has_explicit_controls {
        moves.push("No extra controls beyond pooled caps and standard routing.".to_owned());
    }

    moves
}

fn budget_total_cap_usd(budget: &BudgetRule) -> Option<f64> {
    budget
        .limits
        .spend
        .iter()
        .filter_map(|limit| {
            crate::policy::parse_limit_window(&limit.window)
                .map(|window| (window.num_seconds(), limit.max_usd))
        })
        .max_by_key(|(seconds, _)| *seconds)
        .map(|(_, max_usd)| max_usd)
}

fn summarize_budget_scope(policy: &PolicyFile) -> String {
    let mut scopes = BTreeSet::new();
    for budget in &policy.budgets {
        collect_rule_match_scopes(&budget.rule_match, &mut scopes);
    }
    if scopes.is_empty() {
        return "all requests".to_owned();
    }
    let mut scope: Vec<_> = scopes.into_iter().collect();
    if scope.len() > 3 {
        let remaining = scope.len() - 3;
        scope.truncate(3);
        format!("{} +{} more", scope.join(", "), remaining)
    } else {
        scope.join(", ")
    }
}

fn collect_rule_match_scopes(rule_match: &RuleMatch, scopes: &mut BTreeSet<String>) {
    if let Some(project) = rule_match.project.as_deref() {
        scopes.insert(format!("project:{project}"));
    }
    if let Some(user) = rule_match.user.as_deref() {
        scopes.insert(format!("user:{user}"));
    }
    if let Some(subject) = rule_match.subject.as_deref() {
        scopes.insert(if subject.contains(':') {
            subject.to_owned()
        } else {
            format!("user:{subject}")
        });
    }
    for (kind, value) in [
        ("team", rule_match.team.as_deref()),
        ("group", rule_match.group.as_deref()),
        ("org", rule_match.org.as_deref()),
        ("workflow", rule_match.workflow.as_deref()),
        ("surface", rule_match.surface.as_deref()),
    ] {
        if let Some(value) = value {
            scopes.insert(format!("{kind}:{value}"));
        }
    }
    for nested in &rule_match.any {
        collect_rule_match_scopes(nested, scopes);
    }
}

fn limit_action_label(action: crate::contract::PolicyAction) -> &'static str {
    match action {
        crate::contract::PolicyAction::Allow => "allows",
        crate::contract::PolicyAction::Warn => "warns on",
        crate::contract::PolicyAction::Block => "blocks",
        crate::contract::PolicyAction::Ask => "asks approval for",
    }
}

fn collect_ids<'a>(
    ids: impl Iterator<Item = &'a str>,
    kind: &str,
    errors: &mut Vec<String>,
) -> BTreeSet<&'a str> {
    let mut unique = BTreeSet::new();
    for id in ids {
        if id.trim().is_empty() {
            errors.push(format!("simulation {kind} id must not be empty"));
        } else if !unique.insert(id) {
            errors.push(format!("duplicate simulation {kind} id {id}"));
        }
    }
    unique
}

fn choose_model<'a>(
    file: &'a SimulationFile,
    profile: UsageProfile,
    rng: &mut DeterministicRng,
) -> &'a SimulationModel {
    let default = &file.models[0];
    let alt = file.models.get(1).unwrap_or(default);
    match profile {
        UsageProfile::PowerUser => {
            if rng.next_bounded(10) < 7 {
                default
            } else {
                alt
            }
        }
        UsageProfile::SteadyUser => {
            if rng.next_bounded(10) < 6 {
                alt
            } else {
                default
            }
        }
        UsageProfile::LowAdopter => alt,
        UsageProfile::Experimenter => {
            if rng.next_bounded(2) == 0 {
                default
            } else {
                alt
            }
        }
        UsageProfile::LoopRisk => default,
    }
}

fn profile_request_count(profile: UsageProfile, rng: &mut DeterministicRng, day_index: u32) -> u32 {
    match profile {
        UsageProfile::PowerUser => 3 + rng.next_bounded(3) as u32,
        UsageProfile::SteadyUser => 1 + rng.next_bounded(2) as u32,
        UsageProfile::LowAdopter => {
            if (day_index + rng.next_bounded(10) as u32).is_multiple_of(4) {
                1
            } else {
                0
            }
        }
        UsageProfile::Experimenter => 1 + rng.next_bounded(3) as u32,
        UsageProfile::LoopRisk => 2 + rng.next_bounded(3) as u32,
    }
}

fn profile_tokens(
    profile: UsageProfile,
    rng: &mut DeterministicRng,
    day_index: u32,
    request_index: u32,
) -> u64 {
    match profile {
        UsageProfile::PowerUser => 7_500 + rng.next_bounded(4_500),
        UsageProfile::SteadyUser => 2_500 + rng.next_bounded(2_500),
        UsageProfile::LowAdopter => 800 + rng.next_bounded(1_200),
        UsageProfile::Experimenter => 1_500 + rng.next_bounded(6_500),
        UsageProfile::LoopRisk => {
            if (day_index + request_index).is_multiple_of(3) {
                40_000 + rng.next_bounded(20_000)
            } else {
                5_000 + rng.next_bounded(7_000)
            }
        }
    }
}

fn profile_tool_calls(
    profile: UsageProfile,
    rng: &mut DeterministicRng,
    request_index: u32,
) -> u32 {
    match profile {
        UsageProfile::PowerUser => 2 + rng.next_bounded(3) as u32,
        UsageProfile::SteadyUser => 1 + rng.next_bounded(2) as u32,
        UsageProfile::LowAdopter => rng.next_bounded(2) as u32,
        UsageProfile::Experimenter => 1 + rng.next_bounded(4) as u32,
        UsageProfile::LoopRisk => {
            if request_index.is_multiple_of(2) {
                6 + rng.next_bounded(5) as u32
            } else {
                3 + rng.next_bounded(4) as u32
            }
        }
    }
}

fn profile_useful_work_score(
    profile: UsageProfile,
    rng: &mut DeterministicRng,
    tool_call_count: u32,
) -> u32 {
    let base = match profile {
        UsageProfile::PowerUser => 85,
        UsageProfile::SteadyUser => 70,
        UsageProfile::LowAdopter => 45,
        UsageProfile::Experimenter => 60,
        UsageProfile::LoopRisk => 35,
    };
    base + rng.next_bounded(15) as u32 + tool_call_count.min(5)
}

#[derive(Clone, Debug)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn next_bounded(&mut self, upper: u64) -> u64 {
        if upper <= 1 {
            0
        } else {
            self.next_u64() % upper
        }
    }
}

fn synthetic_authorize_request(
    request: &SyntheticDemandRequest,
    strategy_id: &str,
) -> AuthorizeRequest {
    AuthorizeRequest {
        budget_id: None,
        entities: request.entities.clone(),
        subject: Some(request.subject.clone()),
        project: Some(request.project_id.clone()),
        provider: Some(request.provider.clone()),
        model: Some(request.model.clone()),
        estimated_tokens: Some(request.estimated_tokens),
        estimated_cost_usd: Some(request.estimated_cost_usd),
        metadata: BTreeMap::from([
            (
                "trace_id".to_owned(),
                serde_json::Value::String(format!("{}:{}", strategy_id, request.request_id)),
            ),
            (
                "request_id".to_owned(),
                serde_json::Value::String(request.request_id.clone()),
            ),
            (
                "session_id".to_owned(),
                serde_json::Value::String(format!("simulation:{strategy_id}")),
            ),
        ]),
    }
}

fn is_limit_rule_id(rule_id: &str) -> bool {
    rule_id.contains(".request_cost")
        || rule_id.contains(".context_tokens")
        || rule_id.contains(".spend_window.")
}

pub(crate) fn encode_path_component(value: &str, fallback: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(*byte as char);
            }
            _ => {
                use std::fmt::Write as _;
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

fn fairness_score(users: &[SyntheticUser], user_spend: &BTreeMap<String, f64>) -> f64 {
    if users.is_empty() {
        return 0.0;
    }
    let values: Vec<f64> = users
        .iter()
        .map(|user| {
            user_spend
                .get(&format!("user:{}", user.id))
                .copied()
                .unwrap_or(0.0)
        })
        .collect();
    let total: f64 = values.iter().sum();
    if total == 0.0 {
        return 1.0;
    }
    let mean = total / values.len() as f64;
    let mean_abs_deviation =
        values.iter().map(|value| (value - mean).abs()).sum::<f64>() / values.len() as f64;
    (1.0 - (mean_abs_deviation / mean)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse_checked_in_simulation(path: &str) -> SimulationFile {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let content = std::fs::read_to_string(manifest_dir.join(path))
            .expect("checked-in simulation example is readable");
        serde_yaml::from_str(&content).expect("checked-in simulation example parses")
    }

    fn compare_checked_in_simulation(path: &str) -> SimulationComparisonReport {
        let simulation = parse_checked_in_simulation(path);
        let tempdir = tempfile::tempdir().expect("tempdir");
        compare_strategies(&simulation, tempdir.path())
            .expect("checked-in simulation comparison succeeds")
    }

    #[test]
    fn parses_and_validates_simulation_schema_v1() {
        let simulation: SimulationFile = serde_yaml::from_str(
            r#"
version: 1
name: benchmark company
seed: 42
horizon_days: 30
company:
  id: example-co
  teams:
    - id: platform
    - id: product
  projects:
    - id: editor
      team_id: product
    - id: noether
      team_id: platform
  users:
    - id: alice
      team_id: platform
      project_ids: [noether]
      profile: power_user
    - id: bob
      team_id: product
      project_ids: [editor]
      profile: experimenter
models:
  - id: flagship
    provider: openai
    model: gpt-4.1
    cost_per_1k_tokens_usd: 0.01
strategies:
  - id: pooled
    policy:
      version: 0
      budgets:
        - id: team-platform
          limits:
            spend:
              - id: budget-cap
                window: 30d
                mode: tumbling
                anchor:
                  kind: first_seen
                max_usd: 250
                action: block
          match:
            team: platform
"#,
        )
        .expect("simulation parses");

        validate_simulation(&simulation).expect("simulation is valid");
        assert_eq!(simulation.company.users.len(), 2);
        assert_eq!(simulation.company.users[0].profile, UsageProfile::PowerUser);
    }

    #[test]
    fn rejects_invalid_simulation_schema_v1() {
        let simulation: SimulationFile = serde_yaml::from_str(
            r#"
version: 2
seed: 9
horizon_days: 0
company:
  id: ""
  teams:
    - id: platform
    - id: platform
  projects:
    - id: editor
      team_id: missing-team
  users:
    - id: alice
      team_id: platform
      project_ids: [missing-project]
      profile: low_adopter
models:
  - id: cheap
    provider: ""
    model: ""
    cost_per_1k_tokens_usd: 0
strategies:
  - id: pooled
    policy:
      version: 0
      budgets:
        - id: ""
          limits:
            spend:
              - id: budget-cap
                by: global
                window: 30d
                mode: tumbling
                anchor:
                  kind: first_seen
                max_usd: 0
                action: block
"#,
        )
        .expect("simulation parses");

        let error = validate_simulation(&simulation).expect_err("simulation should be invalid");
        let message = error.to_string();
        assert!(message.contains("simulation version must be 1"));
        assert!(message.contains("simulation company id must not be empty"));
        assert!(message.contains("simulation horizon_days must be greater than zero"));
        assert!(message.contains("duplicate simulation team id platform"));
        assert!(message.contains("references unknown team missing-team"));
        assert!(message.contains("references unknown project missing-project"));
        assert!(message.contains("must not have an empty provider"));
        assert!(message.contains("cost_per_1k_tokens_usd must be positive"));
        assert!(message.contains("simulation strategy pooled policy invalid"));
    }

    #[test]
    fn checked_in_simulation_examples_validate() {
        for (path, expected_strategies) in [
            ("examples/simulations/synthetic-company.noet.yaml", 2),
            ("examples/simulations/runaway-pressure.noet.yaml", 2),
            ("examples/simulations/adoption-pressure.noet.yaml", 2),
        ] {
            let simulation = parse_checked_in_simulation(path);

            validate_simulation(&simulation).expect("example simulation is valid");
            assert_eq!(simulation.strategies.len(), expected_strategies);
        }
    }

    #[test]
    fn runaway_pressure_example_locks_limit_tradeoff() {
        let report =
            compare_checked_in_simulation("examples/simulations/runaway-pressure.noet.yaml");

        assert_eq!(report.total_requests, 115);
        let pooled = report
            .strategies
            .iter()
            .find(|strategy| strategy.id == "pooled without limit")
            .expect("pooled strategy");
        let guarded = report
            .strategies
            .iter()
            .find(|strategy| strategy.id == "limited team budget")
            .expect("guarded strategy");

        assert_eq!(pooled.exhaustion_day, None);
        assert_eq!(guarded.exhaustion_day, None);
        assert_eq!(guarded.limit_hit_count, guarded.denied_requests);
        assert!(
            pooled
                .policy_moves
                .iter()
                .any(|entry| entry.contains("blocks bursts above $12.00 in 30d"))
        );
        assert!(
            guarded
                .policy_moves
                .iter()
                .any(|entry| entry.contains("bursts above $1.20 in 5m"))
        );
        assert!(guarded.denied_requests > pooled.denied_requests);
        assert!(guarded.total_cost_usd < pooled.total_cost_usd);
        assert!(guarded.unused_budget_usd > pooled.unused_budget_usd);
        assert!(guarded.runaway_spend_prevented_usd > pooled.runaway_spend_prevented_usd);
        assert!(guarded.fairness_score > pooled.fairness_score);
    }

    #[test]
    fn adoption_pressure_example_locks_protected_adoption_tradeoff() {
        let report =
            compare_checked_in_simulation("examples/simulations/adoption-pressure.noet.yaml");

        assert_eq!(report.total_requests, 293);
        let pooled = report
            .strategies
            .iter()
            .find(|strategy| strategy.id == "pooled cap")
            .expect("pooled strategy");
        let protected = report
            .strategies
            .iter()
            .find(|strategy| strategy.id == "protected adoption")
            .expect("protected adoption strategy");

        assert_eq!(pooled.low_adopter_count, 0);
        assert_eq!(pooled.high_adopter_count, 0);
        assert_eq!(protected.low_adopter_count, 3);
        assert_eq!(protected.high_adopter_count, 5);
        assert!(
            protected
                .policy_moves
                .iter()
                .any(|entry| entry.contains("reserves $0.40 per user in each monthly window"))
        );
        assert!(protected.unused_protected_opportunity_usd > 1.0);
        assert_eq!(protected.total_cost_usd, pooled.total_cost_usd);
        assert_eq!(protected.denied_requests, pooled.denied_requests);
        assert_eq!(protected.allowed_requests, pooled.allowed_requests);
        assert!(protected.fallback_count <= pooled.fallback_count);
    }

    #[test]
    fn synthetic_demand_is_deterministic_for_a_seed() {
        let simulation: SimulationFile = serde_yaml::from_str(include_str!(
            "../examples/simulations/synthetic-company.noet.yaml"
        ))
        .expect("example simulation parses");

        let first = generate_synthetic_demand(&simulation).expect("first demand");
        let second = generate_synthetic_demand(&simulation).expect("second demand");
        assert_eq!(first, second);

        let mut changed = simulation.clone();
        changed.seed += 1;
        let different = generate_synthetic_demand(&changed).expect("different seed demand");
        assert_ne!(first, different);
    }

    #[test]
    fn synthetic_demand_reflects_profile_shapes() {
        let simulation: SimulationFile = serde_yaml::from_str(include_str!(
            "../examples/simulations/synthetic-company.noet.yaml"
        ))
        .expect("example simulation parses");
        let demand = generate_synthetic_demand(&simulation).expect("demand");

        let power_count = demand
            .iter()
            .filter(|request| request.subject == "user:alice")
            .count();
        let steady_count = demand
            .iter()
            .filter(|request| request.subject == "user:ben")
            .count();
        let low_count = demand
            .iter()
            .filter(|request| request.subject == "user:chloe")
            .count();
        assert!(power_count > steady_count);
        assert!(steady_count > low_count);

        let experimenter_models: BTreeSet<&str> = demand
            .iter()
            .filter(|request| request.subject == "user:diego")
            .map(|request| request.model_id.as_str())
            .collect();
        assert!(experimenter_models.len() >= 2);

        let loop_risk_requests: Vec<&SyntheticDemandRequest> = demand
            .iter()
            .filter(|request| request.subject == "user:eva")
            .collect();
        assert!(loop_risk_requests.iter().any(|request| request.loop_risk));
        assert!(
            loop_risk_requests
                .iter()
                .any(|request| request.tool_call_count >= 6 || request.estimated_tokens >= 40_000)
        );
    }

    #[test]
    fn compare_strategies_disambiguates_colliding_strategy_ids() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let simulation: SimulationFile = serde_yaml::from_str(
            r#"
version: 1
seed: 7
horizon_days: 1
company:
  id: example
  teams:
    - id: platform
  projects:
    - id: noether
      team_id: platform
  users:
    - id: alice
      team_id: platform
      project_ids: [noether]
      profile: steady_user
models:
  - id: flagship
    provider: openai
    model: gpt-4.1
    cost_per_1k_tokens_usd: 0.01
strategies:
  - id: team/a
    policy:
      version: 0
      budgets:
        - id: team-platform
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
            team: platform
  - id: team a
    policy:
      version: 0
      budgets:
        - id: team-platform
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
            team: platform
"#,
        )
        .expect("simulation parses");

        let report = compare_strategies(&simulation, tempdir.path()).expect("compare strategies");
        assert_eq!(report.strategies.len(), 2);
        assert!(matches!(
            report.strategies[0].database,
            Some(SimulationDatabaseLocation::Sqlite { .. })
        ));
        assert!(matches!(
            report.strategies[1].database,
            Some(SimulationDatabaseLocation::Sqlite { .. })
        ));
        assert_ne!(report.strategies[0].db_path, report.strategies[1].db_path);
        assert!(report.strategies[0].db_path.is_relative());
        assert!(report.strategies[1].db_path.is_relative());
        assert_ne!(
            report.strategies[0].usage_report_path,
            report.strategies[1].usage_report_path
        );
        assert!(report.strategies[0].usage_report_path.is_relative());
        assert!(report.strategies[1].usage_report_path.is_relative());
        assert_ne!(
            report.strategies[0].decisions_report_path,
            report.strategies[1].decisions_report_path
        );
        assert!(report.strategies[0].decisions_report_path.is_relative());
        assert!(report.strategies[1].decisions_report_path.is_relative());
    }

    #[tokio::test]
    #[ignore = "requires NOET_TEST_POSTGRES_URL and an isolated PostgreSQL database"]
    async fn compare_strategies_supports_postgres_backend() {
        let database_url = std::env::var("NOET_TEST_POSTGRES_URL").expect("NOET_TEST_POSTGRES_URL");
        let simulation =
            parse_checked_in_simulation("examples/simulations/synthetic-company.noet.yaml");
        let tempdir = tempfile::tempdir().expect("tempdir");

        let report = compare_strategies_with_database(
            &simulation,
            tempdir.path(),
            SimulationDatabase::postgres(
                database_url.clone(),
                AsyncPostgresLedgerOptions::strict(),
            ),
        )
        .await
        .expect("postgres simulation comparison succeeds");

        assert_eq!(report.total_requests, 337);
        assert_eq!(report.strategies.len(), simulation.strategies.len());
        for strategy in &report.strategies {
            let Some(SimulationDatabaseLocation::Postgres { url }) = &strategy.database else {
                panic!("strategy should report postgres database location");
            };
            if let Ok(original_url) = url::Url::parse(&database_url)
                && original_url.password().is_some()
            {
                let reported_url = url::Url::parse(url).expect("reported database URL parses");
                assert_ne!(reported_url.password(), original_url.password());
            }
            assert!(tempdir.path().join(&strategy.usage_report_path).exists());
            assert!(
                tempdir
                    .path()
                    .join(&strategy.decisions_report_path)
                    .exists()
            );
            cleanup_postgres_simulation_schema(&database_url, url).await;
        }
    }

    async fn cleanup_postgres_simulation_schema(database_url: &str, report_url: &str) {
        let Some(schema) = postgres_schema_from_report_url(report_url) else {
            return;
        };
        let Ok(admin) = connect_async_postgres_client(database_url).await else {
            return;
        };
        let _ = admin
            .batch_execute(&format!(
                r#"
                SET lock_timeout = '2s';
                DROP SCHEMA IF EXISTS {} CASCADE;
                "#,
                postgres_ident_literal(&schema)
            ))
            .await;
    }

    fn postgres_schema_from_report_url(report_url: &str) -> Option<String> {
        let url = url::Url::parse(report_url).ok()?;
        url.query_pairs().find_map(|(key, value)| {
            (key == "options")
                .then(|| value.strip_prefix("-csearch_path=").map(str::to_owned))
                .flatten()
        })
    }
}
