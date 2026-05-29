use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use uuid::Uuid;

use crate::capture::capture;
use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, DecisionMode, DecisionOutcome, FinalizeReservation,
    Reservation, SpendWindowMode, TraceEvent,
};
use crate::error::NoetError;
use crate::ledger::{
    AsyncPostgresLedger, AsyncPostgresLedgerOptions, BudgetLedger, ReplaySpendSeed, TraceReportItem,
};
use crate::noether_app;
use crate::openapi;
use crate::policy::PolicyFile;
use crate::proxy::ProxyRoute;
use crate::reporting;
use crate::simulation::SimulationComparisonReport;

#[derive(Clone)]
pub struct AppState {
    pub fixture_dir: PathBuf,
    pub simulation_dir: PathBuf,
    pub policy_proposal_path: PathBuf,
    pub upstream: Option<url::Url>,
    pub routes: Vec<ProxyRoute>,
    pub client: reqwest::Client,
    pub policy: PolicyRuntime,
    pub decision_mode: DecisionMode,
    pub ledger: Arc<Mutex<BudgetLedger>>,
    pub ledger_backend: LedgerBackend,
    pub report_updates: broadcast::Sender<ReportUpdate>,
    replay_jobs: Arc<Mutex<BTreeMap<String, AppReplayJob>>>,
}

#[derive(Clone)]
pub enum LedgerBackend {
    InMemory,
    SQLite {
        path: PathBuf,
    },
    Postgres {
        database_url: String,
        ledger: AsyncPostgresLedger,
    },
}

impl LedgerBackend {
    fn name(&self) -> &'static str {
        match self {
            Self::InMemory => "in_memory",
            Self::SQLite { .. } => "sqlite",
            Self::Postgres { .. } => "postgres",
        }
    }
}

#[allow(dead_code)]
fn assert_app_state_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AppState>();
}

#[derive(Debug, Serialize)]
struct AppPolicyResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    source: String,
    policy: PolicyFile,
    decision_mode: DecisionMode,
    rule_stats: Vec<AppRuleStat>,
    suggestions: Vec<AppSuggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    proposal: Option<AppPolicyProposal>,
}

#[derive(Debug, Serialize)]
struct AppPolicyProposal {
    path: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct AppPolicyEnforceRequest {
    #[serde(default)]
    confirm_replay: bool,
}

#[derive(Debug, Serialize)]
struct AppPolicyRollbackResponse {
    policy: AppPolicyResponse,
    restored_from: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    decision_mode: DecisionMode,
    policy_loaded: bool,
    upstream_configured: bool,
    route_count: usize,
    ledger_backend: &'static str,
    postgres_async_finalize_failures: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AppRuleStat {
    rule: String,
    allow: u64,
    warn: u64,
    deny: u64,
    ask: u64,
    limit_hits: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_model: Option<String>,
}

#[derive(Debug, Serialize)]
struct AppSuggestion {
    id: String,
    title: String,
    body: String,
    rule: String,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    apply_label: Option<String>,
    evidence: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AppPolicyUpdate {
    source: String,
}

#[derive(Debug, Serialize)]
struct AppPolicyApplyResponse {
    policy: AppPolicyResponse,
    applied: String,
}

#[derive(Debug, Serialize)]
struct AppRunsResponse {
    runs: Vec<AppRunRow>,
    totals: AppRunTotals,
    filtered_total: u64,
    next_offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct AppRunsQuery {
    #[serde(default = "default_app_runs_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
    decision: Option<String>,
    rule: Option<String>,
    q: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct AppRunTotals {
    runs: u64,
    allow: u64,
    warn: u64,
    deny: u64,
    ask: u64,
    limit_hits: u64,
    spend_usd: f64,
    tokens: u64,
}

#[derive(Clone, Debug, Serialize)]
struct AppRunRow {
    occurred_at: chrono::DateTime<chrono::Utc>,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_run_id: Option<String>,
    decision: String,
    summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_check: Option<String>,
    limit_hits: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matched_entity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    timeline: Vec<AppRunTimelineItem>,
}

#[derive(Clone, Debug, Serialize)]
struct AppRunTimelineItem {
    occurred_at: chrono::DateTime<chrono::Utc>,
    kind: String,
    summary: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    fields: BTreeMap<String, String>,
}

fn default_app_runs_limit() -> usize {
    80
}

const APP_REPLAY_HISTORY_WINDOW_DAYS: i64 = 30;
const APP_REPLAY_PREVIEW_REQUEST_CAP: usize = 5_000;
const APP_REPLAY_CHANGED_RUNS_CAP: usize = 100;

#[derive(Clone, Copy, Debug, Default)]
struct AppRunUsage {
    cost_usd: f64,
    tokens: u64,
    request_count: u64,
}

#[derive(Clone, Debug, Default)]
struct ReplayRunAggregate {
    run_id: String,
    trace_id: Option<String>,
    baseline_decision: String,
    proposed_decision: String,
    cost_usd: f64,
    tokens: u64,
    rule: Option<String>,
    summary: String,
}

#[derive(Default)]
struct AppRuleEvidence {
    reasons: std::collections::BTreeMap<String, u64>,
    models: std::collections::BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Serialize)]
struct AppReplayResponse {
    baseline: AppRunTotals,
    has_proposed_policy: bool,
    message: String,
    history_window_days: i64,
    history_window_start: chrono::DateTime<chrono::Utc>,
    history_window_end: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    proposal: Option<AppReplayProposal>,
}

#[derive(Clone, Debug, Serialize)]
struct AppReplayScope {
    mode: String,
    request_cap: Option<usize>,
    requests_replayed: usize,
    total_requests_in_window: usize,
    has_more_history: bool,
    changed_runs_cap: usize,
    changed_runs_returned: usize,
    changed_runs_total: usize,
    full_replay_available: bool,
    window_seeded: bool,
}

#[derive(Clone, Debug, Serialize)]
struct AppReplayProposal {
    path: String,
    mode: String,
    can_enforce: bool,
    explanation: String,
    changed_lines: u64,
    added_lines: u64,
    removed_lines: u64,
    proposed: AppRunTotals,
    changed_runs: Vec<AppReplayChangedRun>,
    recommendations: Vec<AppReplayRecommendation>,
    spend_delta_usd: f64,
    preview: Vec<AppReplayDiffLine>,
    scope: AppReplayScope,
}

#[derive(Clone, Debug, Serialize)]
struct AppReplayRecommendation {
    title: String,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<String>,
    action: String,
}

#[derive(Clone, Debug, Serialize)]
struct AppReplayChangedRun {
    run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(rename = "from")]
    from_decision: String,
    #[serde(rename = "to")]
    to_decision: String,
    cost_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<String>,
    summary: String,
}

#[derive(Clone, Debug, Serialize)]
struct AppReplayJobResponse {
    id: String,
    status: String,
    history_window_days: i64,
    created_at: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<AppReplayResponse>,
}

#[derive(Clone, Debug)]
struct AppReplayJob {
    status: String,
    history_window_days: i64,
    created_at: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    error: Option<String>,
    result: Option<AppReplayResponse>,
}

#[derive(Clone, Debug, Serialize)]
struct AppReplayDiffLine {
    kind: String,
    line: String,
}

#[derive(Clone)]
pub struct PolicyRuntime {
    state: Arc<Mutex<PolicyRuntimeState>>,
}

enum PolicyRuntimeState {
    Static(Option<Arc<PolicyFile>>),
    Reloadable(ReloadablePolicyState),
}

struct ReloadablePolicyState {
    active: Option<Arc<PolicyFile>>,
    source_path: PathBuf,
    last_observed_source: PolicySourceSnapshot,
    last_observed_file: Option<PolicyFileSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PolicySourceSnapshot {
    Bytes(Vec<u8>),
    ReadError(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PolicyFileSnapshot {
    len: u64,
    modified: Option<SystemTime>,
}

impl PolicyRuntime {
    fn static_policy(policy: Option<PolicyFile>) -> Self {
        Self {
            state: Arc::new(Mutex::new(PolicyRuntimeState::Static(policy.map(Arc::new)))),
        }
    }

    fn reloadable(source_path: PathBuf, policy: PolicyFile, source_bytes: Vec<u8>) -> Self {
        Self {
            state: Arc::new(Mutex::new(PolicyRuntimeState::Reloadable(
                ReloadablePolicyState {
                    active: Some(Arc::new(policy)),
                    source_path,
                    last_observed_source: PolicySourceSnapshot::Bytes(source_bytes),
                    last_observed_file: None,
                },
            ))),
        }
    }

    async fn current(&self) -> Option<Arc<PolicyFile>> {
        let mut state = self.state.lock().await;
        match &mut *state {
            PolicyRuntimeState::Static(policy) => policy.clone(),
            PolicyRuntimeState::Reloadable(reloadable) => {
                reloadable.refresh().await;
                reloadable.active.clone()
            }
        }
    }

    async fn source(&self) -> Option<(Option<PathBuf>, String, Arc<PolicyFile>)> {
        let mut state = self.state.lock().await;
        match &mut *state {
            PolicyRuntimeState::Static(policy) => {
                let policy = policy.clone()?;
                let source = serde_yaml::to_string(policy.as_ref()).ok()?;
                Some((None, source, policy))
            }
            PolicyRuntimeState::Reloadable(reloadable) => {
                reloadable.refresh().await;
                let policy = reloadable.active.clone()?;
                let source = match &reloadable.last_observed_source {
                    PolicySourceSnapshot::Bytes(bytes) => {
                        String::from_utf8_lossy(bytes).into_owned()
                    }
                    PolicySourceSnapshot::ReadError(_) => {
                        serde_yaml::to_string(policy.as_ref()).ok()?
                    }
                };
                Some((Some(reloadable.source_path.clone()), source, policy))
            }
        }
    }
}

impl PolicyRuntime {
    async fn update_source(
        &self,
        source: String,
    ) -> Result<(Option<PathBuf>, PolicyFile), NoetError> {
        let policy = crate::policy::parse_policy_bytes(source.as_bytes())?;
        let mut state = self.state.lock().await;
        match &mut *state {
            PolicyRuntimeState::Static(active) => {
                *active = Some(Arc::new(policy.clone()));
                Ok((None, policy))
            }
            PolicyRuntimeState::Reloadable(reloadable) => {
                fs::write(&reloadable.source_path, source.as_bytes()).await?;
                reloadable.last_observed_source = PolicySourceSnapshot::Bytes(source.into_bytes());
                reloadable.last_observed_file = None;
                reloadable.active = Some(Arc::new(policy.clone()));
                Ok((Some(reloadable.source_path.clone()), policy))
            }
        }
    }
}

impl ReloadablePolicyState {
    async fn refresh(&mut self) {
        match fs::metadata(&self.source_path).await {
            Ok(metadata) => {
                let snapshot = PolicyFileSnapshot {
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                };
                if self.last_observed_file.as_ref() == Some(&snapshot) {
                    return;
                }
                self.last_observed_file = Some(snapshot);
            }
            Err(read_error) => {
                let snapshot = PolicySourceSnapshot::ReadError(read_error.to_string());
                if snapshot == self.last_observed_source {
                    return;
                }
                self.last_observed_file = None;
                self.last_observed_source = snapshot;
                error!(
                    policy_path = %self.source_path.display(),
                    error = %read_error,
                    "failed to reload noet policy; keeping last good policy"
                );
                return;
            }
        }
        match fs::read(&self.source_path).await {
            Ok(bytes) => {
                let snapshot = PolicySourceSnapshot::Bytes(bytes.clone());
                if snapshot == self.last_observed_source {
                    return;
                }
                self.last_observed_source = snapshot;
                match crate::policy::parse_policy_bytes(&bytes) {
                    Ok(policy) => {
                        self.active = Some(Arc::new(policy));
                        info!(policy_path = %self.source_path.display(), "reloaded noet policy");
                    }
                    Err(error) => {
                        error!(
                            policy_path = %self.source_path.display(),
                            error = %error,
                            "failed to reload noet policy; keeping last good policy"
                        );
                    }
                }
            }
            Err(read_error) => {
                let snapshot = PolicySourceSnapshot::ReadError(read_error.to_string());
                if snapshot == self.last_observed_source {
                    return;
                }
                self.last_observed_file = None;
                self.last_observed_source = snapshot;
                error!(
                    policy_path = %self.source_path.display(),
                    error = %read_error,
                    "failed to reload noet policy; keeping last good policy"
                );
            }
        }
    }
}

impl AppState {
    pub fn new(
        fixture_dir: PathBuf,
        upstream: Option<url::Url>,
        policy: Option<PolicyFile>,
        decision_mode: DecisionMode,
    ) -> Self {
        Self::with_policy_runtime(
            fixture_dir,
            upstream,
            PolicyRuntime::static_policy(policy),
            decision_mode,
        )
    }

    fn with_policy_runtime(
        fixture_dir: PathBuf,
        upstream: Option<url::Url>,
        policy: PolicyRuntime,
        decision_mode: DecisionMode,
    ) -> Self {
        let (report_updates, _) = broadcast::channel(64);
        Self {
            fixture_dir,
            simulation_dir: PathBuf::from(".noet/simulations"),
            policy_proposal_path: PathBuf::from(".noet/policy.proposed.yaml"),
            upstream,
            routes: Vec::new(),
            client: reqwest::Client::new(),
            policy,
            decision_mode,
            ledger: Arc::new(Mutex::new(BudgetLedger::default())),
            ledger_backend: LedgerBackend::InMemory,
            report_updates,
            replay_jobs: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn with_reloadable_policy(
        fixture_dir: PathBuf,
        upstream: Option<url::Url>,
        policy_path: PathBuf,
        policy: PolicyFile,
        decision_mode: DecisionMode,
    ) -> Self {
        let source_bytes = std::fs::read(&policy_path).unwrap_or_default();
        Self::with_policy_runtime(
            fixture_dir,
            upstream,
            PolicyRuntime::reloadable(policy_path, policy, source_bytes),
            decision_mode,
        )
    }

    pub async fn active_policy(&self) -> Option<Arc<PolicyFile>> {
        self.policy.current().await
    }

    pub fn ledger_backend_name(&self) -> &'static str {
        self.ledger_backend.name()
    }

    pub fn postgres_async_finalize_failures(&self) -> Option<u64> {
        match &self.ledger_backend {
            LedgerBackend::Postgres { ledger, .. } => Some(ledger.async_finalize_failures()),
            LedgerBackend::InMemory | LedgerBackend::SQLite { .. } => None,
        }
    }

    pub async fn authorize_request(
        &self,
        policy: Option<Arc<PolicyFile>>,
        request: AuthorizeRequest,
    ) -> Result<AuthorizeDecision, NoetError> {
        match &self.ledger_backend {
            LedgerBackend::Postgres { ledger, .. } => ledger.try_authorize(policy, request).await,
            LedgerBackend::InMemory | LedgerBackend::SQLite { .. } => self
                .ledger
                .lock()
                .await
                .try_authorize(policy.as_deref(), &request),
        }
    }

    pub async fn finalize_reservation(
        &self,
        reservation_id: String,
        payload: FinalizeReservation,
    ) -> Result<Reservation, NoetError> {
        match &self.ledger_backend {
            LedgerBackend::Postgres { ledger, .. } => {
                ledger.finalize(reservation_id, payload).await
            }
            LedgerBackend::InMemory | LedgerBackend::SQLite { .. } => {
                self.ledger.lock().await.finalize(&reservation_id, &payload)
            }
        }
    }

    pub async fn record_trace_event(&self, event: TraceEvent) -> Result<(), NoetError> {
        match &self.ledger_backend {
            LedgerBackend::Postgres { ledger, .. } => ledger.record_event(event).await,
            LedgerBackend::InMemory | LedgerBackend::SQLite { .. } => {
                self.ledger.lock().await.record_event(event)
            }
        }
    }

    async fn active_policy_source(&self) -> Option<(Option<PathBuf>, String, Arc<PolicyFile>)> {
        self.policy.source().await
    }

    async fn update_policy_source(
        &self,
        source: String,
    ) -> Result<(Option<PathBuf>, PolicyFile), NoetError> {
        self.policy.update_source(source).await
    }

    async fn read_ledger<T: Send>(
        &self,
        read: impl FnOnce(&BudgetLedger) -> Result<T, NoetError> + Send,
    ) -> Result<T, NoetError> {
        match &self.ledger_backend {
            LedgerBackend::SQLite { path } => {
                let ledger = BudgetLedger::open_sqlite(path)?;
                read(&ledger)
            }
            LedgerBackend::Postgres { database_url, .. } => std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        let ledger = BudgetLedger::open_postgres(database_url)?;
                        read(&ledger)
                    })
                    .join()
                    .map_err(|_| {
                        NoetError::InvalidConfig("postgres read task panicked".to_owned())
                    })?
            }),
            LedgerBackend::InMemory => {
                let ledger = self.ledger.lock().await;
                read(&ledger)
            }
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ReportUpdate {
    pub kind: &'static str,
    pub trace_id: Option<String>,
}

pub struct ServeConfig {
    pub bind: SocketAddr,
    pub fixture_dir: PathBuf,
    pub simulation_dir: PathBuf,
    pub db_path: PathBuf,
    pub database_url: Option<String>,
    pub postgres_options: AsyncPostgresLedgerOptions,
    pub upstream: Option<url::Url>,
    pub routes: Vec<ProxyRoute>,
    pub policy_path: Option<PathBuf>,
    pub policy: Option<PolicyFile>,
    pub decision_mode: DecisionMode,
}

pub async fn serve(config: ServeConfig) -> Result<(), NoetError> {
    fs::create_dir_all(&config.fixture_dir).await?;
    fs::create_dir_all(&config.simulation_dir).await?;
    if config.database_url.is_none()
        && let Some(parent) = config.db_path.parent()
    {
        fs::create_dir_all(parent).await?;
    }
    let bind = config.bind;
    let postgres_ledger = match config.database_url.as_deref() {
        Some(database_url) => Some(
            AsyncPostgresLedger::connect_with_options(
                database_url,
                config.postgres_options.clone(),
            )
            .await?,
        ),
        None => None,
    };
    let ledger = if config.database_url.is_some() {
        BudgetLedger::default()
    } else {
        BudgetLedger::open_sqlite(&config.db_path)?
    };
    let policy_proposal_path = config
        .simulation_dir
        .parent()
        .unwrap_or_else(|| Path::new(".noet"))
        .join("policy.proposed.yaml");
    let policy_runtime = match (config.policy_path, config.policy) {
        (Some(policy_path), Some(policy)) => {
            let source_bytes = fs::read(&policy_path).await?;
            PolicyRuntime::reloadable(policy_path, policy, source_bytes)
        }
        (_, policy) => PolicyRuntime::static_policy(policy),
    };
    let mut state = AppState::with_policy_runtime(
        config.fixture_dir,
        config.upstream,
        policy_runtime,
        config.decision_mode,
    );
    state.simulation_dir = config.simulation_dir;
    state.policy_proposal_path = policy_proposal_path;
    state.routes = config.routes;
    if let Some(database_url) = config.database_url {
        if let Some(postgres_ledger) = postgres_ledger {
            state.ledger_backend = LedgerBackend::Postgres {
                database_url,
                ledger: postgres_ledger,
            };
        }
    } else {
        state.ledger_backend = LedgerBackend::SQLite {
            path: config.db_path.clone(),
        };
    }
    state.ledger = Arc::new(Mutex::new(ledger));
    let app = build_router(state);

    info!(bind = %bind, "starting noet capture server");
    let listener = TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/authorize", post(authorize))
        .route("/v1/reservations/{id}/finalize", post(finalize_reservation))
        .route("/v1/events", post(record_event))
        .route("/v1/reports/usage", get(report_usage))
        .route("/v1/reports/decisions", get(report_decisions))
        .route("/v1/reports/traces/{trace_id}", get(report_trace))
        .route("/v1/reports/observations", get(report_observations))
        .route("/v1/reports/updates", get(report_updates_stream))
        .route(
            "/v1/reports/dashboard-data",
            any(deprecated_dashboard_surface),
        )
        .route("/v1/reports/dashboard", any(deprecated_dashboard_surface))
        .route("/v1/dashboard/{*path}", any(deprecated_dashboard_surface))
        .route("/v1/app/policy", get(app_policy))
        .route(
            "/v1/app/policy/proposal",
            put(update_app_policy_proposal).delete(discard_app_policy_proposal),
        )
        .route(
            "/v1/app/policy/suggestions/{suggestion_id}/apply",
            post(apply_app_policy_suggestion),
        )
        .route("/v1/app/policy/enforce", post(enforce_app_policy_proposal))
        .route("/v1/app/policy/rollback", post(rollback_app_policy))
        .route("/v1/app/runs", get(app_runs))
        .route("/v1/app/runs/{run_id}", get(app_run_detail))
        .route("/v1/app/replay", get(app_replay))
        .route("/v1/app/replay/jobs", post(start_app_replay_job))
        .route("/v1/app/replay/jobs/{job_id}", get(app_replay_job))
        .route("/v1/simulations", get(list_simulations))
        .route("/v1/simulations/{simulation_id}", get(simulation_report))
        .route(
            "/v1/simulations/{simulation_id}/dashboard",
            get(simulation_dashboard_html),
        )
        .route(
            "/v1/simulations/{simulation_id}/strategies/{strategy_id}/usage",
            get(simulation_strategy_usage),
        )
        .route(
            "/v1/simulations/{simulation_id}/strategies/{strategy_id}/decisions",
            get(simulation_strategy_decisions),
        )
        .route(
            "/v1/simulations/{simulation_id}/strategies/{strategy_id}/dashboard",
            get(simulation_strategy_dashboard_html),
        )
        .route("/simulations", get(simulations_index_html))
        .route("/", get(noether_app_html))
        .route("/policy", get(noether_app_html))
        .route("/runs", get(noether_app_html))
        .route("/replay", get(noether_app_html))
        .route("/app/app.js", get(noether_app_js))
        .route("/app/app.css", get(noether_app_css))
        .route("/app/logo.svg", get(noether_app_logo))
        .route("/app/favicon.svg", get(noether_app_favicon))
        .route("/openapi.json", get(openapi_json))
        .route("/docs", get(api_docs))
        .route("/api/docs", get(api_docs))
        .route("/dashboard", any(deprecated_dashboard_surface))
        .route("/dashboard/{*path}", any(deprecated_dashboard_surface))
        .route("/v1/chat/completions", any(capture))
        .route("/v1/messages", any(capture))
        .route("/v1/responses", any(capture))
        .route("/health", any(health))
        .fallback(any(capture))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok",
        decision_mode: state.decision_mode,
        policy_loaded: state.active_policy().await.is_some(),
        upstream_configured: state.upstream.is_some(),
        route_count: state.routes.len(),
        ledger_backend: state.ledger_backend_name(),
        postgres_async_finalize_failures: state.postgres_async_finalize_failures(),
    })
}

async fn authorize(
    State(state): State<AppState>,
    Json(request): Json<AuthorizeRequest>,
) -> Result<Json<AuthorizeDecision>, NoetError> {
    let policy = state.active_policy().await;
    let decision = state
        .authorize_request(policy.clone(), request.clone())
        .await?;
    publish_report_update(&state, "authorize", request_trace_id(&request));
    Ok(Json(decision))
}

async fn finalize_reservation(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(payload): Json<FinalizeReservation>,
) -> Result<Json<Reservation>, NoetError> {
    let trace_id = finalize_trace_id(&payload);
    let reservation = state.finalize_reservation(id, payload).await?;
    publish_report_update(&state, "finalize", trace_id);
    Ok(Json(reservation))
}

async fn record_event(
    State(state): State<AppState>,
    Json(event): Json<TraceEvent>,
) -> Result<impl IntoResponse, NoetError> {
    let trace_id = event.trace_id.clone();
    state.record_trace_event(event).await?;
    publish_report_update(&state, "event", trace_id);
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true })),
    ))
}

#[derive(Debug, Default, Deserialize)]
struct ReportQuery {
    kind: Option<String>,
    trace: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SimulationStrategyPath {
    simulation_id: String,
    strategy_id: String,
}

#[derive(Clone, Debug)]
struct SimulationArtifact {
    id: String,
    report: SimulationComparisonReport,
    dashboard_path: PathBuf,
}

#[derive(Clone, Debug)]
struct LoadedSimulationStrategy {
    usage_report_path: PathBuf,
    decisions_report_path: PathBuf,
    dashboard_path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
struct SimulationSurfaceSummary {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    seed: u64,
    horizon_days: u32,
    total_requests: u64,
    strategy_count: usize,
    report_url: String,
    dashboard_url: String,
    strategies: Vec<SimulationStrategySurfaceSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct SimulationStrategySurfaceSummary {
    id: String,
    usage_url: String,
    decisions_url: String,
    dashboard_url: String,
}

async fn report_usage(State(state): State<AppState>) -> Result<Json<serde_json::Value>, NoetError> {
    state
        .read_ledger(|ledger| {
            Ok(Json(serde_json::to_value(reporting::usage_report(
                ledger,
            )?)?))
        })
        .await
}

async fn report_decisions(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, NoetError> {
    state
        .read_ledger(|ledger| {
            Ok(Json(serde_json::to_value(reporting::decisions_report(
                ledger,
            )?)?))
        })
        .await
}

async fn report_trace(
    State(state): State<AppState>,
    AxumPath(trace_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, NoetError> {
    state
        .read_ledger(|ledger| {
            Ok(Json(serde_json::to_value(reporting::trace_report(
                ledger, &trace_id,
            )?)?))
        })
        .await
}

async fn report_observations(
    State(state): State<AppState>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<serde_json::Value>, NoetError> {
    state
        .read_ledger(|ledger| {
            Ok(Json(serde_json::to_value(reporting::observations_report(
                ledger,
                query.kind.as_deref(),
                query.trace.as_deref(),
            )?)?))
        })
        .await
}

async fn app_policy(State(state): State<AppState>) -> Result<Json<AppPolicyResponse>, NoetError> {
    let Some((path, _, policy)) = state.active_policy_source().await else {
        return Err(NoetError::NotFound("no active policy".to_owned()));
    };
    let report = state
        .read_ledger(|ledger| ledger.rule_stats_report())
        .await?;
    let rule_stats = app_rule_stats_from_report(policy.as_ref(), report);
    let suggestions = app_policy_suggestions(&rule_stats);
    let proposal = app_policy_proposal(&state.policy_proposal_path).await?;
    let source = app_display_policy_source(policy.as_ref())?;
    Ok(Json(AppPolicyResponse {
        path: path.map(|path| path.display().to_string()),
        source,
        policy: policy.as_ref().clone(),
        decision_mode: state.decision_mode,
        rule_stats,
        suggestions,
        proposal,
    }))
}

async fn update_app_policy_proposal(
    State(state): State<AppState>,
    Json(update): Json<AppPolicyUpdate>,
) -> Result<Json<AppPolicyResponse>, NoetError> {
    crate::policy::parse_policy_bytes(update.source.as_bytes())?;
    if let Some(parent) = state.policy_proposal_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&state.policy_proposal_path, update.source.as_bytes()).await?;
    let response = app_policy(State(state.clone())).await?;
    let _ = state.report_updates.send(ReportUpdate {
        kind: "policy_proposal",
        trace_id: None,
    });
    Ok(response)
}

async fn discard_app_policy_proposal(
    State(state): State<AppState>,
) -> Result<Json<AppPolicyResponse>, NoetError> {
    match fs::remove_file(&state.policy_proposal_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let response = app_policy(State(state.clone())).await?;
    let _ = state.report_updates.send(ReportUpdate {
        kind: "policy_proposal",
        trace_id: None,
    });
    Ok(response)
}

async fn apply_app_policy_suggestion(
    State(state): State<AppState>,
    AxumPath(suggestion_id): AxumPath<String>,
) -> Result<Json<AppPolicyApplyResponse>, NoetError> {
    let Some((_, active_source, policy)) = state.active_policy_source().await else {
        return Err(NoetError::NotFound("no active policy".to_owned()));
    };
    let decisions = state.read_ledger(reporting::decisions_report).await?;
    let stats = app_rule_stats(&policy, &decisions);
    let suggestions = app_policy_suggestions(&stats);
    let suggestion = suggestions
        .iter()
        .find(|suggestion| suggestion.id == suggestion_id)
        .ok_or_else(|| NoetError::NotFound(format!("suggestion {suggestion_id}")))?;
    let source = app_policy_proposal(&state.policy_proposal_path)
        .await?
        .map(|proposal| proposal.source)
        .unwrap_or(active_source);
    let updated_source = apply_suggestion_to_policy_source(&source, suggestion)?;
    if let Some(parent) = state.policy_proposal_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&state.policy_proposal_path, updated_source.as_bytes()).await?;
    let policy = app_policy(State(state.clone())).await?.0;
    Ok(Json(AppPolicyApplyResponse {
        policy,
        applied: suggestion.title.clone(),
    }))
}

async fn enforce_app_policy_proposal(
    State(state): State<AppState>,
    request: Option<Json<AppPolicyEnforceRequest>>,
) -> Result<Json<AppPolicyResponse>, NoetError> {
    let source = match fs::read_to_string(&state.policy_proposal_path).await {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(NoetError::NotFound("no policy proposal saved".to_owned()));
        }
        Err(error) => return Err(error.into()),
    };
    if !request
        .as_ref()
        .map(|request| request.confirm_replay)
        .unwrap_or(false)
    {
        return Err(NoetError::InvalidPolicy(
            "policy enforce requires confirm_replay=true after reviewing replay".to_owned(),
        ));
    }
    if let Some((_, active_source, _)) = state.active_policy_source().await {
        if active_source == source {
            return Err(NoetError::InvalidPolicy(
                "policy proposal matches active policy; nothing to enforce".to_owned(),
            ));
        }
        write_previous_policy_snapshot(&state, &active_source).await?;
        append_policy_audit(&state, "enforce", "saved draft promoted to active policy").await?;
    }
    state.update_policy_source(source).await?;
    match fs::remove_file(&state.policy_proposal_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let response = app_policy(State(state.clone())).await?;
    let _ = state.report_updates.send(ReportUpdate {
        kind: "policy",
        trace_id: None,
    });
    Ok(response)
}

async fn rollback_app_policy(
    State(state): State<AppState>,
) -> Result<Json<AppPolicyRollbackResponse>, NoetError> {
    let previous_path = policy_previous_path(&state);
    let source = match fs::read_to_string(&previous_path).await {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(NoetError::NotFound(
                "no previous policy snapshot saved".to_owned(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    state.update_policy_source(source).await?;
    append_policy_audit(&state, "rollback", "previous policy snapshot restored").await?;
    let policy = app_policy(State(state.clone())).await?.0;
    let _ = state.report_updates.send(ReportUpdate {
        kind: "policy",
        trace_id: None,
    });
    Ok(Json(AppPolicyRollbackResponse {
        policy,
        restored_from: previous_path.display().to_string(),
    }))
}

async fn app_runs(
    State(state): State<AppState>,
    Query(query): Query<AppRunsQuery>,
) -> Result<Json<AppRunsResponse>, NoetError> {
    let limit = query.limit.clamp(1, 250);
    let offset = query.offset;
    state
        .read_ledger(|ledger| {
            if app_runs_query_is_unfiltered(&query) {
                let decisions = ledger.decisions_report_for_run_page(limit, offset)?;
                let agent_run_ids = app_agent_run_ids_from_decisions(&decisions);
                let usage_by_agent_run = app_usage_by_agent_run(
                    &ledger.usage_activity_report_for_agent_runs(&agent_run_ids)?,
                );
                let runs = app_agent_runs(&decisions, &usage_by_agent_run);
                let totals = app_run_totals_from_report(ledger.run_totals_report()?);
                let filtered_total = totals.runs;
                let next_offset =
                    (offset + runs.len() < filtered_total as usize).then_some(offset + runs.len());
                return Ok(Json(AppRunsResponse {
                    runs,
                    totals,
                    filtered_total,
                    next_offset,
                }));
            }
            let decisions = reporting::decisions_report(&ledger)?;
            let usage_by_agent_run = app_usage_by_agent_run(&ledger.usage_activity_report()?);
            let all_runs = app_agent_runs(&decisions, &usage_by_agent_run);
            let totals = app_run_totals_from_rows(&all_runs);
            let filtered = all_runs
                .into_iter()
                .filter(|run| app_run_matches_query(run, &query))
                .collect::<Vec<_>>();
            let filtered_total = filtered.len() as u64;
            let runs = filtered
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>();
            let next_offset =
                (offset + runs.len() < filtered_total as usize).then_some(offset + runs.len());
            Ok(Json(AppRunsResponse {
                runs,
                totals,
                filtered_total,
                next_offset,
            }))
        })
        .await
}

fn app_runs_query_is_unfiltered(query: &AppRunsQuery) -> bool {
    query
        .decision
        .as_deref()
        .map(|value| value.is_empty() || value == "any")
        .unwrap_or(true)
        && query
            .rule
            .as_deref()
            .map(|value| value.is_empty() || value == "any")
            .unwrap_or(true)
        && query.q.as_deref().map(str::trim).unwrap_or("").is_empty()
}

async fn app_run_detail(
    State(state): State<AppState>,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<AppRunRow>, NoetError> {
    state
        .read_ledger(|ledger| {
            let decisions = reporting::decisions_report(ledger)?;
            let usage_by_agent_run = app_usage_by_agent_run(&ledger.usage_activity_report()?);
            let mut run = app_agent_runs(&decisions, &usage_by_agent_run)
                .into_iter()
                .find(|run| {
                    run.id == run_id
                        || run.agent_run_id.as_deref() == Some(run_id.as_str())
                        || run.trace_id.as_deref() == Some(run_id.as_str())
                })
                .ok_or_else(|| NoetError::NotFound(format!("run {run_id}")))?;
            run.timeline = app_run_timeline(ledger, &run)?;
            Ok(Json(run))
        })
        .await
}

async fn app_replay(State(state): State<AppState>) -> Result<Json<AppReplayResponse>, NoetError> {
    let history_window_end = chrono::Utc::now();
    let history_window_start =
        history_window_end - chrono::Duration::days(APP_REPLAY_HISTORY_WINDOW_DAYS);
    let proposal = app_policy_proposal(&state.policy_proposal_path).await?;
    let has_proposed_policy = proposal.is_some();
    let proposed_policy = proposal
        .as_ref()
        .map(|proposal| crate::policy::parse_policy_bytes(proposal.source.as_bytes()))
        .transpose()?;
    let active_source = state
        .active_policy_source()
        .await
        .map(|(_, _, policy)| app_display_policy_source(policy.as_ref()))
        .transpose()?
        .unwrap_or_default();
    if !has_proposed_policy {
        let baseline = state
            .read_ledger(|ledger| {
                Ok(app_run_totals_from_report(
                    ledger.run_totals_report_since(Some(history_window_start))?,
                ))
            })
            .await?;
        return Ok(Json(AppReplayResponse {
            baseline,
            has_proposed_policy,
            message: "No proposed policy has been saved for replay yet. Edit Policy first to create a local draft without enforcing it.".to_owned(),
            history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
            history_window_start,
            history_window_end,
            proposal: None,
        }));
    }
    let (total_requests, historical_requests, usage_by_agent_run, baseline, spend_seeds) = state
        .read_ledger(|ledger| {
            let total_requests =
                ledger.historical_authorize_request_count_since(Some(history_window_start))?;
            let historical_requests = ledger.latest_historical_authorize_requests_since(
                Some(history_window_start),
                APP_REPLAY_PREVIEW_REQUEST_CAP,
            )?;
            let spend_seeds = historical_requests
                .first()
                .zip(proposed_policy.as_ref())
                .map(|(first, policy)| {
                    app_replay_spend_seeds(ledger, policy, history_window_start, first.occurred_at)
                })
                .transpose()?
                .unwrap_or_default();
            let agent_run_ids = historical_requests
                .iter()
                .filter_map(|request| string_metadata_value(&request.request, "agent_run_id"))
                .collect::<Vec<_>>();
            let usage_by_agent_run = app_usage_by_agent_run(
                &ledger.usage_activity_report_for_agent_runs(&agent_run_ids)?,
            );
            let baseline = app_run_totals_from_report(
                ledger.run_totals_report_since(Some(history_window_start))?,
            );
            Ok((
                total_requests,
                historical_requests,
                usage_by_agent_run,
                baseline,
                spend_seeds,
            ))
        })
        .await?;
    let replay_proposal = proposal
        .as_ref()
        .map(|proposal| {
            app_replay_proposal(
                &active_source,
                proposal,
                &historical_requests,
                &usage_by_agent_run,
                &spend_seeds,
                ReplayScopeOptions {
                    mode: "preview".to_owned(),
                    request_cap: Some(APP_REPLAY_PREVIEW_REQUEST_CAP),
                    total_requests_in_window: total_requests,
                    full_replay_available: total_requests > historical_requests.len(),
                    changed_runs_cap: APP_REPLAY_CHANGED_RUNS_CAP,
                    window_seeded: !spend_seeds.is_empty(),
                },
            )
        })
        .transpose()?;
    Ok(Json(AppReplayResponse {
        baseline,
        has_proposed_policy,
        message: if has_proposed_policy {
            "A valid proposed policy is saved locally. Preview replay re-evaluated the most recent recorded authorizations in the 30-day window."
        } else {
            "No proposed policy has been saved for replay yet. Edit Policy first to create a local draft without enforcing it."
        }
        .to_owned(),
        history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
        history_window_start,
        history_window_end,
        proposal: replay_proposal,
    }))
}

async fn start_app_replay_job(
    State(state): State<AppState>,
) -> Result<Json<AppReplayJobResponse>, NoetError> {
    if app_policy_proposal(&state.policy_proposal_path)
        .await?
        .is_none()
    {
        return Err(NoetError::InvalidPolicy(
            "full replay requires a saved proposed policy".to_owned(),
        ));
    }
    let id = Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now();
    {
        let mut jobs = state.replay_jobs.lock().await;
        jobs.insert(
            id.clone(),
            AppReplayJob {
                status: "running".to_owned(),
                history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
                created_at,
                completed_at: None,
                error: None,
                result: None,
            },
        );
    }
    let jobs = state.replay_jobs.clone();
    let replay_state = state.clone();
    let job_id = id.clone();
    tokio::spawn(async move {
        let result = app_replay_full_month_response(replay_state).await;
        let mut jobs = jobs.lock().await;
        if let Some(job) = jobs.get_mut(&job_id) {
            job.completed_at = Some(chrono::Utc::now());
            match result {
                Ok(result) => {
                    job.status = "completed".to_owned();
                    job.result = Some(result);
                }
                Err(error) => {
                    job.status = "failed".to_owned();
                    job.error = Some(error.to_string());
                }
            }
        }
    });
    let jobs = state.replay_jobs.lock().await;
    let job = jobs
        .get(&id)
        .expect("job was inserted before response")
        .clone();
    Ok(Json(app_replay_job_response(id, job)))
}

async fn app_replay_job(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<AppReplayJobResponse>, NoetError> {
    let jobs = state.replay_jobs.lock().await;
    let job = jobs
        .get(&job_id)
        .cloned()
        .ok_or_else(|| NoetError::NotFound(format!("replay job {job_id}")))?;
    Ok(Json(app_replay_job_response(job_id, job)))
}

fn app_replay_job_response(id: String, job: AppReplayJob) -> AppReplayJobResponse {
    AppReplayJobResponse {
        id,
        status: job.status,
        history_window_days: job.history_window_days,
        created_at: job.created_at,
        completed_at: job.completed_at,
        error: job.error,
        result: job.result,
    }
}

async fn app_replay_full_month_response(state: AppState) -> Result<AppReplayResponse, NoetError> {
    let history_window_end = chrono::Utc::now();
    let history_window_start =
        history_window_end - chrono::Duration::days(APP_REPLAY_HISTORY_WINDOW_DAYS);
    let proposal = app_policy_proposal(&state.policy_proposal_path)
        .await?
        .ok_or_else(|| {
            NoetError::InvalidPolicy("full replay requires a saved proposed policy".to_owned())
        })?;
    let active_source = state
        .active_policy_source()
        .await
        .map(|(_, _, policy)| app_display_policy_source(policy.as_ref()))
        .transpose()?
        .unwrap_or_default();
    let (total_requests, historical_requests, usage_by_agent_run, baseline) = state
        .read_ledger(|ledger| {
            let total_requests =
                ledger.historical_authorize_request_count_since(Some(history_window_start))?;
            let historical_requests =
                ledger.historical_authorize_requests_since(Some(history_window_start))?;
            let usage_by_agent_run = app_usage_by_agent_run(
                &ledger.usage_activity_report_since(Some(history_window_start))?,
            );
            let baseline = app_run_totals_from_report(
                ledger.run_totals_report_since(Some(history_window_start))?,
            );
            Ok((
                total_requests,
                historical_requests,
                usage_by_agent_run,
                baseline,
            ))
        })
        .await?;
    let proposal = app_replay_proposal(
        &active_source,
        &proposal,
        &historical_requests,
        &usage_by_agent_run,
        &[],
        ReplayScopeOptions {
            mode: "full_month".to_owned(),
            request_cap: None,
            total_requests_in_window: total_requests,
            full_replay_available: false,
            changed_runs_cap: APP_REPLAY_CHANGED_RUNS_CAP,
            window_seeded: false,
        },
    )?;
    Ok(AppReplayResponse {
        baseline,
        has_proposed_policy: true,
        message: "Full 30-day replay completed against the saved draft policy.".to_owned(),
        history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
        history_window_start,
        history_window_end,
        proposal: Some(proposal),
    })
}

async fn app_policy_proposal(path: &Path) -> Result<Option<AppPolicyProposal>, NoetError> {
    match fs::read_to_string(path).await {
        Ok(source) => {
            let policy = crate::policy::parse_policy_bytes(source.as_bytes())?;
            Ok(Some(AppPolicyProposal {
                path: path.display().to_string(),
                source: app_display_policy_source(&policy)?,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn app_display_policy_source(policy: &PolicyFile) -> Result<String, NoetError> {
    serde_yaml::to_string(policy).map_err(NoetError::from)
}

fn app_rule_stats(policy: &PolicyFile, decisions: &[TraceReportItem]) -> Vec<AppRuleStat> {
    let mut stats = policy
        .budgets
        .iter()
        .map(|budget| {
            (
                budget.id.clone(),
                AppRuleStat {
                    rule: budget.id.clone(),
                    allow: 0,
                    warn: 0,
                    deny: 0,
                    ask: 0,
                    limit_hits: 0,
                    top_reason: None,
                    top_model: None,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut evidence = std::collections::BTreeMap::<String, AppRuleEvidence>::new();

    for item in decisions {
        let rule = item
            .routing
            .as_ref()
            .and_then(|routing| routing.selected_budget_id.clone())
            .unwrap_or_else(|| "unattributed".to_owned());
        let stat = stats.entry(rule.clone()).or_insert_with(|| AppRuleStat {
            rule,
            allow: 0,
            warn: 0,
            deny: 0,
            ask: 0,
            limit_hits: 0,
            top_reason: None,
            top_model: None,
        });
        let decision = app_decision_label(&item.kind);
        match decision.as_str() {
            "allow" => stat.allow += 1,
            "warn" => stat.warn += 1,
            "deny" => stat.deny += 1,
            "ask" => stat.ask += 1,
            _ => {}
        }
        stat.limit_hits += item
            .limit_hits
            .as_ref()
            .map(|hits| hits.len() as u64)
            .unwrap_or(0);
        if decision == "deny"
            || item
                .limit_hits
                .as_ref()
                .is_some_and(|hits| !hits.is_empty())
        {
            let evidence = evidence.entry(stat.rule.clone()).or_default();
            if let Some(reason) = app_decision_reason(item) {
                *evidence.reasons.entry(reason).or_default() += 1;
            }
            if let Some(model) = reporting::summary_value(&item.summary, "model") {
                *evidence.models.entry(model).or_default() += 1;
            }
        }
    }

    let mut stats = stats.into_values().collect::<Vec<_>>();
    for stat in &mut stats {
        if let Some(evidence) = evidence.get(&stat.rule) {
            stat.top_reason = most_common(&evidence.reasons);
            stat.top_model = most_common(&evidence.models);
        }
    }
    stats
}

fn app_rule_stats_from_report(
    policy: &PolicyFile,
    report: Vec<crate::ledger::RuleStatsReport>,
) -> Vec<AppRuleStat> {
    let mut stats = policy
        .budgets
        .iter()
        .map(|budget| {
            (
                budget.id.clone(),
                AppRuleStat {
                    rule: budget.id.clone(),
                    allow: 0,
                    warn: 0,
                    deny: 0,
                    ask: 0,
                    limit_hits: 0,
                    top_reason: None,
                    top_model: None,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for row in report {
        stats.insert(
            row.rule.clone(),
            AppRuleStat {
                rule: row.rule,
                allow: row.allow,
                warn: row.warn,
                deny: row.deny,
                ask: row.ask,
                limit_hits: row.limit_hits,
                top_reason: row.top_reason,
                top_model: row.top_model,
            },
        );
    }
    stats.into_values().collect()
}

fn app_policy_suggestions(stats: &[AppRuleStat]) -> Vec<AppSuggestion> {
    let mut suggestions = Vec::new();
    for stat in stats {
        if stat.deny > 0 {
            let mut evidence = Vec::new();
            if let Some(reason) = &stat.top_reason {
                evidence.push(format!("Reason: {reason}"));
            }
            if let Some(model) = &stat.top_model {
                evidence.push(format!("Top model: {model}"));
            }
            evidence.push(format!("Denied runs: {}", stat.deny));
            let body = match (&stat.top_reason, &stat.top_model) {
                (Some(reason), Some(model)) if reason.contains("provider/model is not allowed") => {
                    let pattern = model_ref_to_policy_pattern(model);
                    format!(
                        "If this is intended, keep the denial. If not, add {pattern} to {}.models.allow or route it to another budget, then replay.",
                        stat.rule
                    )
                }
                (Some(reason), _) => format!(
                    "Most denials are because: {reason}. Inspect affected runs, edit the specific rule if needed, then replay."
                ),
                _ => "Inspect affected runs, edit the specific rule if needed, then replay."
                    .to_owned(),
            };
            suggestions.push(AppSuggestion {
                id: format!("{}-denies", stat.rule),
                title: format!("{} blocked {} run(s)", stat.rule, stat.deny),
                body,
                rule: stat.rule.clone(),
                action: "open_runs_filtered_to_rule".to_owned(),
                apply_label: stat
                    .top_reason
                    .as_deref()
                    .filter(|reason| reason.contains("provider/model is not allowed"))
                    .and(stat.top_model.as_deref())
                    .map(|model| format!("Allow {}", model_ref_to_policy_pattern(model))),
                evidence,
            });
        } else if stat.limit_hits > 0 {
            let evidence = stat
                .top_reason
                .iter()
                .map(|reason| format!("Limit evidence: {reason}"))
                .collect::<Vec<_>>();
            suggestions.push(AppSuggestion {
                id: format!("{}-limit-hits", stat.rule),
                title: format!("{} hit limits {} time(s)", stat.rule, stat.limit_hits),
                body: "This rule is close to its boundary. Replay a stricter or roomier policy against real history.".to_owned(),
                rule: stat.rule.clone(),
                action: "replay_rule_change".to_owned(),
                apply_label: None,
                evidence,
            });
        }
    }
    suggestions.truncate(3);
    suggestions
}

fn app_decision_reason(item: &TraceReportItem) -> Option<String> {
    if let Some(hit) = item
        .binding_limit
        .as_ref()
        .or_else(|| item.limit_hits.as_ref().and_then(|hits| hits.first()))
    {
        return Some(hit.reason.clone());
    }
    let routing = item.routing.as_ref()?;
    if routing.model_check.as_deref() == Some("denied") {
        return Some("provider/model is not allowed by budget".to_owned());
    }
    routing.rejected_budget_reason.clone()
}

fn model_ref_to_policy_pattern(model_ref: &str) -> String {
    model_ref
        .split_once('/')
        .map(|(provider, model)| format!("{provider}:{model}"))
        .unwrap_or_else(|| model_ref.to_owned())
}

fn apply_suggestion_to_policy_source(
    source: &str,
    suggestion: &AppSuggestion,
) -> Result<String, NoetError> {
    let model = suggestion
        .apply_label
        .as_deref()
        .and_then(|label| label.strip_prefix("Allow "))
        .ok_or_else(|| {
            NoetError::InvalidPolicy("suggestion cannot be applied automatically".to_owned())
        })?;
    let mut policy = crate::policy::parse_policy_bytes(source.as_bytes())?;
    let budget = policy
        .budgets
        .iter_mut()
        .find(|budget| budget.id == suggestion.rule)
        .ok_or_else(|| NoetError::NotFound(format!("budget {}", suggestion.rule)))?;
    if !budget.models.allow.iter().any(|value| value == model) {
        budget.models.allow.push(model.to_owned());
        budget.models.allow.sort();
    }
    serde_yaml::to_string(&policy).map_err(NoetError::from)
}

fn most_common(values: &std::collections::BTreeMap<String, u64>) -> Option<String> {
    values
        .iter()
        .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
        .map(|(value, _)| value.clone())
}

fn app_agent_runs(
    decisions: &[TraceReportItem],
    usage_by_agent_run: &std::collections::BTreeMap<String, AppRunUsage>,
) -> Vec<AppRunRow> {
    let mut runs = std::collections::BTreeMap::<String, AppRunRow>::new();
    for item in decisions {
        let run = app_run_row(item, usage_by_agent_run);
        let key = app_run_group_key(&run);
        runs.entry(key)
            .and_modify(|existing| merge_app_run(existing, &run))
            .or_insert(run);
    }
    let mut runs = runs.into_values().collect::<Vec<_>>();
    runs.sort_by_key(|run| std::cmp::Reverse(run.occurred_at));
    runs
}

fn app_run_group_key(run: &AppRunRow) -> String {
    if let Some(agent_run_id) = run.agent_run_id.as_deref() {
        return format!("agent-run:{agent_run_id}");
    }
    if let Some(trace_id) = run.trace_id.as_deref() {
        return format!("trace-fallback:{trace_id}");
    }
    let minute_bucket = run.occurred_at.timestamp() / 60;
    format!(
        "untraced:{}:{}:{}:{minute_bucket}",
        run.decision,
        run.rule.as_deref().unwrap_or("unattributed"),
        run.model.as_deref().unwrap_or("unknown")
    )
}

fn app_run_row(
    item: &TraceReportItem,
    usage_by_agent_run: &std::collections::BTreeMap<String, AppRunUsage>,
) -> AppRunRow {
    let trace_id = item
        .trace_id
        .clone()
        .or_else(|| reporting::summary_value(&item.summary, "trace"));
    let request_id = reporting::summary_value(&item.summary, "request")
        .or_else(|| reporting::summary_value(&item.summary, "request_id"));
    let id = request_id
        .or_else(|| trace_id.clone())
        .unwrap_or_else(|| format!("{}-{}", item.kind, item.occurred_at.timestamp_millis()));
    let agent_run_id = item.agent_run_id.clone();
    let model_ref = reporting::summary_value(&item.summary, "model");
    let (provider, model) = model_ref
        .as_deref()
        .and_then(|value| value.split_once('/'))
        .map(|(provider, model)| (Some(provider.to_owned()), Some(model.to_owned())))
        .unwrap_or_else(|| (None, model_ref.clone()));
    let run_usage = agent_run_id
        .as_deref()
        .and_then(|agent_run_id| usage_by_agent_run.get(agent_run_id).copied());
    let estimated_cost = reporting::summary_value(&item.summary, "estimated_cost")
        .and_then(|value| value.parse::<f64>().ok());
    AppRunRow {
        occurred_at: item.occurred_at,
        id: agent_run_id.clone().unwrap_or(id),
        agent_run_id,
        decision: app_decision_label(&item.kind),
        summary: item.summary.clone(),
        trace_id,
        rule: item
            .routing
            .as_ref()
            .and_then(|routing| routing.selected_budget_id.clone()),
        decision_reason: app_decision_reason(item),
        model_check: item
            .routing
            .as_ref()
            .and_then(|routing| routing.model_check.clone())
            .or_else(|| reporting::summary_value(&item.summary, "model_check")),
        limit_hits: item
            .limit_hits
            .as_ref()
            .map(|hits| hits.len() as u64)
            .unwrap_or(0),
        provider,
        model,
        cost_usd: estimated_cost.or_else(|| {
            run_usage
                .map(|usage| usage.cost_usd)
                .filter(|cost| *cost > 0.0)
        }),
        estimated_tokens: reporting::summary_value(&item.summary, "estimated_tokens")
            .and_then(|value| value.parse::<u64>().ok()),
        actual_tokens: run_usage
            .map(|usage| usage.tokens)
            .filter(|tokens| *tokens > 0),
        tool_calls: reporting::summary_value(&item.summary, "tools_count")
            .and_then(|value| value.parse::<u64>().ok()),
        matched_entity: item
            .routing
            .as_ref()
            .and_then(|routing| routing.matched_entity.clone()),
        entities: item.entities.clone(),
        timeline: Vec::new(),
    }
}

fn merge_app_run(existing: &mut AppRunRow, next: &AppRunRow) {
    if next.occurred_at > existing.occurred_at {
        existing.occurred_at = next.occurred_at;
        existing.id = next.id.clone();
        existing.summary = next.summary.clone();
        existing.provider = next.provider.clone().or_else(|| existing.provider.clone());
        existing.model = next.model.clone().or_else(|| existing.model.clone());
        existing.estimated_tokens = next.estimated_tokens.or(existing.estimated_tokens);
        existing.tool_calls = next.tool_calls.or(existing.tool_calls);
    }
    existing.limit_hits += next.limit_hits;
    existing.cost_usd = existing.cost_usd.or(next.cost_usd);
    existing.actual_tokens = existing.actual_tokens.or(next.actual_tokens);
    if app_decision_rank(&next.decision) > app_decision_rank(&existing.decision) {
        existing.decision = next.decision.clone();
        existing.rule = next.rule.clone().or_else(|| existing.rule.clone());
        existing.decision_reason = next
            .decision_reason
            .clone()
            .or_else(|| existing.decision_reason.clone());
        existing.model_check = next
            .model_check
            .clone()
            .or_else(|| existing.model_check.clone());
    }
    if existing.rule.is_none() {
        existing.rule = next.rule.clone();
    }
    if existing.decision_reason.is_none() {
        existing.decision_reason = next.decision_reason.clone();
    }
    if existing.model_check.is_none() {
        existing.model_check = next.model_check.clone();
    }
    if existing.matched_entity.is_none() {
        existing.matched_entity = next.matched_entity.clone();
    }
    for entity in &next.entities {
        if !existing.entities.contains(entity) {
            existing.entities.push(entity.clone());
        }
    }
}

fn app_decision_rank(decision: &str) -> u8 {
    match decision {
        "deny" => 4,
        "ask" => 3,
        "warn" => 2,
        "allow" => 1,
        _ => 0,
    }
}

fn app_usage_by_agent_run(
    usage: &[crate::ledger::UsageActivityRecord],
) -> std::collections::BTreeMap<String, AppRunUsage> {
    let mut by_agent_run = std::collections::BTreeMap::new();
    for record in usage {
        let Some(agent_run_id) = record.agent_run_id.as_deref() else {
            continue;
        };
        let entry = by_agent_run
            .entry(agent_run_id.to_owned())
            .or_insert_with(AppRunUsage::default);
        entry.cost_usd += record.cost_usd;
        entry.tokens += record.total_tokens;
        entry.request_count += 1;
    }
    by_agent_run
}

fn app_agent_run_ids_from_decisions(decisions: &[TraceReportItem]) -> Vec<String> {
    decisions
        .iter()
        .filter_map(|decision| decision.agent_run_id.clone())
        .collect()
}

fn app_run_timeline(
    ledger: &BudgetLedger,
    run: &AppRunRow,
) -> Result<Vec<AppRunTimelineItem>, NoetError> {
    let Some(trace_id) = run.trace_id.as_deref() else {
        return Ok(vec![AppRunTimelineItem {
            occurred_at: run.occurred_at,
            kind: format!("decision.{}", run.decision),
            summary: run.summary.clone(),
            fields: app_summary_fields(&run.summary),
        }]);
    };
    let trace = ledger.trace_report(trace_id)?;
    let mut items = trace
        .items
        .into_iter()
        .filter(|item| {
            run.agent_run_id.is_none()
                || item.agent_run_id.as_deref() == run.agent_run_id.as_deref()
        })
        .map(|item| AppRunTimelineItem {
            occurred_at: item.occurred_at,
            kind: item.kind,
            fields: app_summary_fields(&item.summary),
            summary: item.summary,
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        items.push(AppRunTimelineItem {
            occurred_at: run.occurred_at,
            kind: format!("decision.{}", run.decision),
            summary: run.summary.clone(),
            fields: app_summary_fields(&run.summary),
        });
    }
    items.sort_by_key(|item| item.occurred_at);
    items.truncate(80);
    Ok(items)
}

fn app_summary_fields(summary: &str) -> BTreeMap<String, String> {
    summary
        .split_whitespace()
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn app_run_matches_query(run: &AppRunRow, query: &AppRunsQuery) -> bool {
    if let Some(decision) = query.decision.as_deref()
        && !decision.is_empty()
        && decision != "any"
        && run.decision != decision
    {
        return false;
    }
    if let Some(rule) = query.rule.as_deref()
        && !rule.is_empty()
        && rule != "any"
        && run.rule.as_deref() != Some(rule)
    {
        return false;
    }
    if let Some(q) = query.q.as_deref() {
        let q = q.trim().to_ascii_lowercase();
        if !q.is_empty()
            && ![
                run.id.as_str(),
                run.agent_run_id.as_deref().unwrap_or(""),
                run.summary.as_str(),
                run.trace_id.as_deref().unwrap_or(""),
                run.rule.as_deref().unwrap_or(""),
                run.provider.as_deref().unwrap_or(""),
                run.model.as_deref().unwrap_or(""),
                run.matched_entity.as_deref().unwrap_or(""),
            ]
            .iter()
            .any(|value| value.to_ascii_lowercase().contains(&q))
        {
            return false;
        }
    }
    true
}

fn app_replay_proposal(
    active_source: &str,
    proposal: &AppPolicyProposal,
    historical_requests: &[crate::ledger::HistoricalAuthorizeRequest],
    usage_by_agent_run: &std::collections::BTreeMap<String, AppRunUsage>,
    spend_seeds: &[ReplaySpendSeed],
    scope_options: ReplayScopeOptions,
) -> Result<AppReplayProposal, NoetError> {
    let active_lines = active_source
        .lines()
        .collect::<std::collections::BTreeSet<_>>();
    let proposal_lines = proposal
        .source
        .lines()
        .collect::<std::collections::BTreeSet<_>>();
    let mut preview = Vec::new();

    for line in active_lines.difference(&proposal_lines).take(8) {
        preview.push(AppReplayDiffLine {
            kind: "removed".to_owned(),
            line: (*line).to_owned(),
        });
    }
    for line in proposal_lines.difference(&active_lines).take(8) {
        preview.push(AppReplayDiffLine {
            kind: "added".to_owned(),
            line: (*line).to_owned(),
        });
    }

    let added_lines = proposal_lines.difference(&active_lines).count() as u64;
    let removed_lines = active_lines.difference(&proposal_lines).count() as u64;
    let changed_lines = added_lines + removed_lines;
    let proposed_policy = crate::policy::parse_policy_bytes(proposal.source.as_bytes())?;
    let (proposed, mut changed_runs, spend_delta_usd, changed_runs_total) =
        replay_historical_requests(
            &proposed_policy,
            historical_requests,
            usage_by_agent_run,
            spend_seeds,
        )?;
    let recommendations = app_replay_recommendations(&changed_runs, spend_delta_usd);
    changed_runs.truncate(scope_options.changed_runs_cap);
    let changed_runs_returned = changed_runs.len();
    let (mode, explanation) = if changed_lines == 0 {
        (
            "current_policy_backtest",
            "No pending source edit. This backtests the currently saved policy against recorded historical decisions.",
        )
    } else {
        (
            "draft_impact",
            "This compares the active policy to the saved draft by replaying recorded historical authorizations.",
        )
    };
    Ok(AppReplayProposal {
        path: proposal.path.clone(),
        mode: mode.to_owned(),
        can_enforce: changed_lines > 0,
        explanation: explanation.to_owned(),
        changed_lines,
        added_lines,
        removed_lines,
        proposed,
        changed_runs,
        recommendations,
        spend_delta_usd,
        preview,
        scope: AppReplayScope {
            mode: scope_options.mode,
            request_cap: scope_options.request_cap,
            requests_replayed: historical_requests.len(),
            total_requests_in_window: scope_options.total_requests_in_window,
            has_more_history: scope_options.total_requests_in_window > historical_requests.len(),
            changed_runs_cap: scope_options.changed_runs_cap,
            changed_runs_returned,
            changed_runs_total,
            full_replay_available: scope_options.full_replay_available,
            window_seeded: scope_options.window_seeded,
        },
    })
}

struct ReplayScopeOptions {
    mode: String,
    request_cap: Option<usize>,
    total_requests_in_window: usize,
    full_replay_available: bool,
    changed_runs_cap: usize,
    window_seeded: bool,
}

fn app_replay_spend_seeds(
    ledger: &BudgetLedger,
    proposed_policy: &PolicyFile,
    history_window_start: chrono::DateTime<chrono::Utc>,
    preview_start: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<ReplaySpendSeed>, NoetError> {
    if preview_start <= history_window_start {
        return Ok(Vec::new());
    }
    let seed_at = preview_start - chrono::Duration::nanoseconds(1);
    let mut seeds = Vec::new();
    for rule in &proposed_policy.budgets {
        for limit in &rule.limits.spend {
            let Some(window) = crate::policy::parse_limit_window(&limit.window) else {
                continue;
            };
            let limit_id = limit.id.as_deref().unwrap_or(limit.window.as_str());
            let mode = limit.mode.unwrap_or(SpendWindowMode::Tumbling);
            let since = match mode {
                SpendWindowMode::Rolling => (preview_start - window).max(history_window_start),
                SpendWindowMode::Tumbling => (preview_start - window).max(history_window_start),
            };
            let totals = ledger.spend_scope_totals(&rule.id, limit_id, since, preview_start)?;
            for total in totals {
                seeds.push(ReplaySpendSeed {
                    rule_id: rule.id.clone(),
                    limit_id: limit_id.to_owned(),
                    scope_key: total.scope_key,
                    amount_usd: total.amount_usd,
                    mode,
                    seeded_at: seed_at,
                    window_started_at: since,
                });
            }
        }
    }
    Ok(seeds)
}

fn app_replay_recommendations(
    changed_runs: &[AppReplayChangedRun],
    spend_delta_usd: f64,
) -> Vec<AppReplayRecommendation> {
    if changed_runs.is_empty() {
        return vec![AppReplayRecommendation {
            title: "Draft matches recorded history".to_owned(),
            body: "No recorded run decisions would change. This is safe from a historical-decision perspective, but it may still affect future traffic.".to_owned(),
            rule: None,
            action: "review_policy_diff".to_owned(),
        }];
    }

    let newly_blocked = changed_runs
        .iter()
        .filter(|run| run.to_decision == "deny" && run.from_decision != "deny")
        .count();
    let newly_warned = changed_runs
        .iter()
        .filter(|run| run.to_decision == "warn" && run.from_decision != "warn")
        .count();
    let newly_allowed = changed_runs
        .iter()
        .filter(|run| run.from_decision == "deny" && run.to_decision != "deny")
        .count();
    let mut by_rule = std::collections::BTreeMap::<String, (u64, f64)>::new();
    for run in changed_runs {
        let rule = run
            .rule
            .clone()
            .unwrap_or_else(|| "unattributed".to_owned());
        let entry = by_rule.entry(rule).or_default();
        entry.0 += 1;
        entry.1 += run.cost_usd;
    }
    let (rule, (count, cost)) = by_rule
        .into_iter()
        .max_by(|(_, left), (_, right)| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
        })
        .expect("changed runs are non-empty");
    let title = if newly_blocked > 0 {
        format!("{newly_blocked} run(s) would be newly blocked")
    } else if newly_warned > 0 && newly_allowed == 0 {
        format!("{newly_warned} run(s) would become warnings")
    } else if newly_warned > 0 && newly_allowed > 0 {
        format!("{newly_warned} warning(s), {newly_allowed} previously denied run(s) loosened")
    } else if newly_allowed > 0 {
        format!("{newly_allowed} previously denied run(s) would be allowed or warned")
    } else {
        format!("{count} recorded outcome(s) would change")
    };
    let body = if newly_blocked > 0 {
        format!(
            "This draft blocks traffic that previously ran. The largest affected rule is {rule}, covering ${cost:.2}. Projected spend delta is {spend_delta_usd:+.2}; inspect examples before adopting."
        )
    } else if newly_warned > 0 {
        format!(
            "This draft mostly changes enforcement posture, not spend: affected runs would warn under {rule}. Projected spend delta is {spend_delta_usd:+.2}."
        )
    } else {
        format!(
            "The largest affected rule is {rule}, covering ${cost:.2}. Projected spend delta is {spend_delta_usd:+.2}; inspect examples before adopting."
        )
    };
    vec![AppReplayRecommendation {
        title,
        body,
        rule: Some(rule),
        action: "review_changed_runs".to_owned(),
    }]
}

fn replay_historical_requests(
    proposed_policy: &PolicyFile,
    historical_requests: &[crate::ledger::HistoricalAuthorizeRequest],
    usage_by_agent_run: &std::collections::BTreeMap<String, AppRunUsage>,
    spend_seeds: &[ReplaySpendSeed],
) -> Result<(AppRunTotals, Vec<AppReplayChangedRun>, f64, usize), NoetError> {
    let mut replay_ledger = BudgetLedger::default();
    for seed in spend_seeds {
        replay_ledger.seed_replay_spend(seed.clone());
    }
    let mut runs = std::collections::BTreeMap::<String, ReplayRunAggregate>::new();

    for historical in historical_requests {
        let decision = replay_ledger.try_authorize_replay_at(
            Some(proposed_policy),
            &historical.request,
            historical.occurred_at,
        )?;
        let proposed_label = decision_outcome_label(decision.outcome);
        let baseline_label = decision_outcome_label(historical.baseline_outcome);
        let agent_run_id = string_metadata_value(&historical.request, "agent_run_id");
        let trace_id = string_metadata_value(&historical.request, "trace_id");
        let key = agent_run_id
            .clone()
            .map(|id| format!("agent-run:{id}"))
            .or_else(|| trace_id.clone().map(|id| format!("trace:{id}")))
            .unwrap_or_else(|| {
                let minute_bucket = historical.occurred_at.timestamp() / 60;
                format!(
                    "untraced:{}:{}:{}:{minute_bucket}",
                    baseline_label,
                    "unattributed",
                    historical.request.model.as_deref().unwrap_or("unknown")
                )
            });
        let run_id = agent_run_id
            .clone()
            .or_else(|| trace_id.clone())
            .unwrap_or_else(|| historical.decision_id.clone());
        let usage = agent_run_id
            .as_deref()
            .and_then(|id| usage_by_agent_run.get(id).copied());
        let entry = runs.entry(key).or_insert_with(|| ReplayRunAggregate {
            run_id,
            trace_id,
            baseline_decision: baseline_label.to_owned(),
            proposed_decision: proposed_label.to_owned(),
            cost_usd: usage.map(|usage| usage.cost_usd).unwrap_or(0.0),
            tokens: usage.map(|usage| usage.tokens).unwrap_or(0),
            rule: None,
            summary: replay_change_summary(&historical.request),
        });
        if usage.is_none() {
            entry.cost_usd += historical.request.estimated_cost_usd.unwrap_or(0.0);
            entry.tokens += historical.request.estimated_tokens.unwrap_or(0);
        }
        if app_decision_rank(baseline_label) > app_decision_rank(&entry.baseline_decision) {
            entry.baseline_decision = baseline_label.to_owned();
        }
        if app_decision_rank(proposed_label) > app_decision_rank(&entry.proposed_decision) {
            entry.proposed_decision = proposed_label.to_owned();
        }
        if entry.rule.is_none() {
            entry.rule = decision
                .explanations
                .iter()
                .find(|explanation| explanation.severity == decision.action.decision_severity())
                .map(|explanation| explanation.rule_id.clone());
        }
    }

    let mut totals = AppRunTotals {
        runs: runs.len() as u64,
        ..AppRunTotals::default()
    };
    let mut changed_runs = Vec::new();
    let mut baseline_spend = 0.0;
    let mut proposed_spend = 0.0;
    for run in runs.into_values() {
        totals.tokens += run.tokens;
        match run.proposed_decision.as_str() {
            "allow" => totals.allow += 1,
            "warn" => totals.warn += 1,
            "deny" => totals.deny += 1,
            "ask" => totals.ask += 1,
            _ => {}
        }
        if run.baseline_decision != "deny" {
            baseline_spend += run.cost_usd;
        }
        if run.proposed_decision != "deny" {
            proposed_spend += run.cost_usd;
            totals.spend_usd += run.cost_usd;
        }
        if run.baseline_decision != run.proposed_decision {
            changed_runs.push(AppReplayChangedRun {
                run_id: run.run_id,
                trace_id: run.trace_id,
                from_decision: run.baseline_decision,
                to_decision: run.proposed_decision,
                cost_usd: run.cost_usd,
                rule: run.rule,
                summary: run.summary,
            });
        }
    }
    changed_runs.sort_by(|left, right| right.cost_usd.total_cmp(&left.cost_usd));
    let changed_runs_total = changed_runs.len();
    Ok((
        totals,
        changed_runs,
        proposed_spend - baseline_spend,
        changed_runs_total,
    ))
}

fn decision_outcome_label(outcome: DecisionOutcome) -> &'static str {
    match outcome {
        DecisionOutcome::Allow => "allow",
        DecisionOutcome::Warn => "warn",
        DecisionOutcome::Deny => "deny",
    }
}

fn string_metadata_value(request: &AuthorizeRequest, key: &str) -> Option<String> {
    request
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn replay_change_summary(request: &AuthorizeRequest) -> String {
    [
        request.project.as_deref(),
        request.subject.as_deref(),
        request.provider.as_deref(),
        request.model.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn app_run_totals_from_rows(runs: &[AppRunRow]) -> AppRunTotals {
    let mut totals = AppRunTotals {
        runs: runs.len() as u64,
        ..AppRunTotals::default()
    };
    for run in runs {
        match run.decision.as_str() {
            "allow" => totals.allow += 1,
            "warn" => totals.warn += 1,
            "deny" => totals.deny += 1,
            "ask" => totals.ask += 1,
            _ => {}
        }
        totals.limit_hits += run.limit_hits;
        totals.spend_usd += run.cost_usd.unwrap_or(0.0);
        totals.tokens += run.actual_tokens.or(run.estimated_tokens).unwrap_or(0);
    }
    totals
}

fn app_run_totals_from_report(report: crate::ledger::RunTotalsReport) -> AppRunTotals {
    AppRunTotals {
        runs: report.runs,
        allow: report.allow,
        warn: report.warn,
        deny: report.deny,
        ask: report.ask,
        limit_hits: report.limit_hits,
        spend_usd: report.spend_usd,
        tokens: report.tokens,
    }
}

fn app_decision_label(kind: &str) -> String {
    if kind.ends_with(".allow") {
        "allow"
    } else if kind.ends_with(".warn") {
        "warn"
    } else if kind.ends_with(".deny") {
        "deny"
    } else if kind.ends_with(".ask") {
        "ask"
    } else {
        kind
    }
    .to_owned()
}

async fn list_simulations(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let simulations = list_simulation_artifacts(&state.simulation_dir)?
        .into_iter()
        .map(simulation_surface_summary)
        .collect::<Vec<_>>();
    Ok(Json(serde_json::to_value(simulations)?))
}

async fn simulation_report(
    State(state): State<AppState>,
    AxumPath(simulation_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let artifact = load_simulation_artifact(&state.simulation_dir, &simulation_id)?;
    Ok(Json(serde_json::to_value(artifact.report)?))
}

async fn simulation_dashboard_html(
    State(state): State<AppState>,
    AxumPath(simulation_id): AxumPath<String>,
) -> Result<Html<String>, NoetError> {
    let artifact = load_simulation_artifact(&state.simulation_dir, &simulation_id)?;
    if !artifact.dashboard_path.exists() {
        return Err(NoetError::NotFound(format!(
            "simulation dashboard for {} not found",
            artifact.id
        )));
    }
    Ok(Html(std::fs::read_to_string(&artifact.dashboard_path)?))
}

async fn simulation_strategy_usage(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<SimulationStrategyPath>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let strategy = load_simulation_strategy(
        &state.simulation_dir,
        &path.simulation_id,
        &path.strategy_id,
    )?;
    Ok(Json(read_json_file(&strategy.usage_report_path)?))
}

async fn simulation_strategy_decisions(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<SimulationStrategyPath>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let strategy = load_simulation_strategy(
        &state.simulation_dir,
        &path.simulation_id,
        &path.strategy_id,
    )?;
    Ok(Json(read_json_file(&strategy.decisions_report_path)?))
}

async fn simulation_strategy_dashboard_html(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<SimulationStrategyPath>,
) -> Result<Html<String>, NoetError> {
    let strategy = load_simulation_strategy(
        &state.simulation_dir,
        &path.simulation_id,
        &path.strategy_id,
    )?;
    if !strategy.dashboard_path.exists() {
        return Err(NoetError::NotFound(format!(
            "strategy dashboard for {} not found in simulation {}",
            path.strategy_id, path.simulation_id
        )));
    }
    Ok(Html(std::fs::read_to_string(&strategy.dashboard_path)?))
}

async fn simulations_index_html(State(state): State<AppState>) -> Result<Html<String>, NoetError> {
    let simulations = list_simulation_artifacts(&state.simulation_dir)?;
    Ok(Html(render_simulations_index(&simulations)))
}

async fn report_updates_stream(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.report_updates.subscribe()).filter_map(|message| {
        let update = match message {
            Ok(update) => update,
            Err(_) => return None,
        };
        let data = serde_json::to_string(&update).ok()?;
        Some(Ok(Event::default().event("report-update").data(data)))
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn noether_app_html() -> Html<&'static str> {
    Html(noether_app::app_shell())
}

fn list_simulation_artifacts(simulation_dir: &Path) -> Result<Vec<SimulationArtifact>, NoetError> {
    if !simulation_dir.exists() {
        return Ok(Vec::new());
    }

    let mut artifacts = Vec::new();
    for entry in std::fs::read_dir(simulation_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        let report_path = entry.path().join("simulation-report.json");
        let dashboard_path = entry.path().join("simulation-dashboard.html");
        if !report_path.exists() || !dashboard_path.exists() {
            continue;
        }
        let report = read_simulation_report(&report_path)?;
        artifacts.push(SimulationArtifact {
            id,
            report,
            dashboard_path,
        });
    }

    artifacts.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(artifacts)
}

fn load_simulation_artifact(
    simulation_dir: &Path,
    simulation_id: &str,
) -> Result<SimulationArtifact, NoetError> {
    let simulation_id = normalized_surface_id(simulation_id, "simulation")?;
    let simulation_path = simulation_dir.join(simulation_id);
    let report_path = simulation_path.join("simulation-report.json");
    let dashboard_path = simulation_path.join("simulation-dashboard.html");
    if !report_path.exists() {
        return Err(NoetError::NotFound(format!(
            "simulation artifact {simulation_id} not found"
        )));
    }
    Ok(SimulationArtifact {
        id: simulation_id.to_owned(),
        report: read_simulation_report(&report_path)?,
        dashboard_path,
    })
}

fn load_simulation_strategy(
    simulation_dir: &Path,
    simulation_id: &str,
    strategy_id: &str,
) -> Result<LoadedSimulationStrategy, NoetError> {
    let strategy_id = normalized_surface_id(strategy_id, "strategy")?;
    let artifact = load_simulation_artifact(simulation_dir, simulation_id)?;
    let simulation_root = simulation_dir.join(&artifact.id);
    let report = artifact
        .report
        .strategies
        .into_iter()
        .find(|strategy| strategy.id == strategy_id)
        .ok_or_else(|| {
            NoetError::NotFound(format!(
                "strategy artifact {strategy_id} not found in simulation {simulation_id}"
            ))
        })?;
    let strategy_dir =
        simulation_root
            .join("strategies")
            .join(crate::simulation::encode_path_component(
                &report.id,
                "simulation",
            ));
    Ok(LoadedSimulationStrategy {
        usage_report_path: strategy_dir.join("usage-report.json"),
        decisions_report_path: strategy_dir.join("decisions-report.json"),
        dashboard_path: strategy_dir.join("noether-dashboard.html"),
    })
}

fn normalized_surface_id<'a>(value: &'a str, kind: &str) -> Result<&'a str, NoetError> {
    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) if !value.trim().is_empty() => Ok(value),
        _ => Err(NoetError::NotFound(format!("invalid {kind} id {value}"))),
    }
}

fn read_simulation_report(path: &Path) -> Result<SimulationComparisonReport, NoetError> {
    Ok(serde_json::from_slice(&read_file_bytes(path)?)?)
}

fn read_json_file(path: &Path) -> Result<serde_json::Value, NoetError> {
    Ok(serde_json::from_slice(&read_file_bytes(path)?)?)
}

fn read_file_bytes(path: &Path) -> Result<Vec<u8>, NoetError> {
    std::fs::read(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            NoetError::NotFound(format!("artifact {} not found", path.display()))
        } else {
            error.into()
        }
    })
}

fn simulation_surface_summary(artifact: SimulationArtifact) -> SimulationSurfaceSummary {
    let simulation_id = percent_encode_path_component(&artifact.id);
    let report_url = format!("/v1/simulations/{simulation_id}");
    let dashboard_url = format!("{report_url}/dashboard");
    let strategies = artifact
        .report
        .strategies
        .iter()
        .map(|strategy| SimulationStrategySurfaceSummary {
            id: strategy.id.clone(),
            usage_url: format!(
                "/v1/simulations/{simulation_id}/strategies/{}/usage",
                percent_encode_path_component(&strategy.id)
            ),
            decisions_url: format!(
                "/v1/simulations/{simulation_id}/strategies/{}/decisions",
                percent_encode_path_component(&strategy.id)
            ),
            dashboard_url: format!(
                "/v1/simulations/{simulation_id}/strategies/{}/dashboard",
                percent_encode_path_component(&strategy.id)
            ),
        })
        .collect();
    SimulationSurfaceSummary {
        id: artifact.id,
        name: artifact.report.name,
        seed: artifact.report.seed,
        horizon_days: artifact.report.horizon_days,
        total_requests: artifact.report.total_requests,
        strategy_count: artifact.report.strategies.len(),
        report_url,
        dashboard_url,
        strategies,
    }
}

fn render_simulations_index(simulations: &[SimulationArtifact]) -> String {
    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether simulation surfaces</title>");
    html.push_str(
        "<style>
        :root { color-scheme: dark; --bg:#0f172a; --panel:#111c33; --muted:#94a3b8; --text:#e5edf7; --line:#263449; --blue:#38bdf8; }
        * { box-sizing:border-box; }
        body { margin:0; font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; background:radial-gradient(circle at top left,#172554,#0f172a 42%); color:var(--text); }
        main { max-width:1120px; margin:0 auto; padding:32px 20px 48px; }
        h1 { margin:0 0 8px; font-size:34px; letter-spacing:-0.04em; }
        h2 { margin:0 0 10px; font-size:22px; }
        p, li { color:var(--muted); }
        .sub { margin:0 0 22px; max-width:760px; }
        .stack { display:grid; gap:16px; }
        .card { background:rgba(17,28,51,.88); border:1px solid var(--line); border-radius:18px; padding:20px; box-shadow:0 18px 55px rgba(0,0,0,.22); }
        .meta { display:flex; gap:10px; flex-wrap:wrap; margin:10px 0 16px; }
        .pill { display:inline-flex; align-items:center; border-radius:999px; padding:4px 9px; background:#1e293b; border:1px solid var(--line); font-size:13px; color:var(--text); }
        .links, .strategy-list { display:grid; gap:8px; }
        .split { display:grid; gap:16px; grid-template-columns:1.15fr .85fr; }
        .empty { color:var(--muted); padding:18px; border:1px dashed var(--line); border-radius:14px; }
        a { color:var(--blue); text-decoration:none; }
        a:hover { text-decoration:underline; }
        code { color:var(--blue); }
        @media (max-width:860px) { .split { grid-template-columns:1fr; } h1 { font-size:28px; } }
        </style>",
    );
    html.push_str("</head><body><main>");
    html.push_str("<h1>Noether simulation surfaces</h1>");
    html.push_str("<p class=\"sub\">Artifact-backed simulation review for outputs generated by <code>noet simulate</code>. These routes serve the checked report and dashboard files under the configured simulation directory without inventing a separate server-owned registry.</p>");

    if simulations.is_empty() {
        html.push_str("<div class=\"empty\">No simulation artifacts are available yet. Run <code>noet simulate &lt;file&gt;</code> first, then refresh this page.</div>");
        html.push_str("</main></body></html>");
        return html;
    }

    html.push_str("<section class=\"stack\">");
    for artifact in simulations {
        let title = artifact.report.name.as_deref().unwrap_or(&artifact.id);
        let summary = simulation_surface_summary(artifact.clone());
        let _ = std::fmt::Write::write_fmt(
            &mut html,
            format_args!(
                "<article class=\"card\"><h2>{}</h2><div class=\"meta\"><span class=\"pill\">seed {}</span><span class=\"pill\">{} simulated day(s)</span><span class=\"pill\">{} request(s)</span><span class=\"pill\">{} strategy variant(s)</span></div>",
                escape_html(title),
                artifact.report.seed,
                artifact.report.horizon_days,
                artifact.report.total_requests,
                artifact.report.strategies.len()
            ),
        );
        html.push_str("<div class=\"split\">");
        html.push_str("<div class=\"links\"><strong>Simulation comparison surface</strong>");
        let _ = std::fmt::Write::write_fmt(
            &mut html,
            format_args!(
                "<a href=\"{}\">Comparison dashboard</a><a href=\"{}\">Simulation report JSON</a>",
                escape_html(&summary.dashboard_url),
                escape_html(&summary.report_url)
            ),
        );
        html.push_str(
            "</div><div class=\"strategy-list\"><strong>Per-strategy artifact surfaces</strong>",
        );
        for strategy in summary.strategies {
            let _ = std::fmt::Write::write_fmt(
                &mut html,
                format_args!(
                    "<div><span class=\"pill\">{}</span> <a href=\"{}\">dashboard</a> · <a href=\"{}\">usage</a> · <a href=\"{}\">decisions</a></div>",
                    escape_html(&strategy.id),
                    escape_html(&strategy.dashboard_url),
                    escape_html(&strategy.usage_url),
                    escape_html(&strategy.decisions_url)
                ),
            );
        }
        html.push_str("</div></div></article>");
    }
    html.push_str("</section></main></body></html>");
    html
}

fn percent_encode_path_component(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                let _ = std::fmt::Write::write_fmt(&mut encoded, format_args!("%{byte:02X}"));
            }
        }
    }
    encoded
}

async fn noether_app_js() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/javascript; charset=utf-8")],
        noether_app::app_js(),
    )
}

async fn noether_app_css() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/css; charset=utf-8")],
        noether_app::app_css(),
    )
}

async fn noether_app_logo() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        noether_app::logo_svg(),
    )
}

async fn noether_app_favicon() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        noether_app::favicon_svg(),
    )
}

async fn openapi_json() -> Result<impl IntoResponse, NoetError> {
    openapi::openapi_json_response()
}

async fn api_docs() -> impl IntoResponse {
    openapi::api_docs_html()
}

async fn deprecated_dashboard_surface() -> impl IntoResponse {
    (
        StatusCode::GONE,
        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        "The old Noether dashboard has been removed. Use /policy, /runs, or /replay.",
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&#39;")
}

fn publish_report_update(state: &AppState, kind: &'static str, trace_id: Option<String>) {
    let _ = state.report_updates.send(ReportUpdate { kind, trace_id });
}

fn request_trace_id(request: &AuthorizeRequest) -> Option<String> {
    request
        .metadata
        .get("trace_id")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

fn finalize_trace_id(payload: &FinalizeReservation) -> Option<String> {
    payload
        .metadata
        .get("trace_id")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

fn policy_previous_path(state: &AppState) -> PathBuf {
    state
        .policy_proposal_path
        .parent()
        .unwrap_or_else(|| Path::new(".noet"))
        .join("policy.previous.yaml")
}

fn policy_audit_path(state: &AppState) -> PathBuf {
    state
        .policy_proposal_path
        .parent()
        .unwrap_or_else(|| Path::new(".noet"))
        .join("policy.audit.jsonl")
}

async fn write_previous_policy_snapshot(state: &AppState, source: &str) -> Result<(), NoetError> {
    let previous_path = policy_previous_path(state);
    if let Some(parent) = previous_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(previous_path, source.as_bytes()).await?;
    Ok(())
}

async fn append_policy_audit(
    state: &AppState,
    action: &str,
    reason: &str,
) -> Result<(), NoetError> {
    let audit_path = policy_audit_path(state);
    if let Some(parent) = audit_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let entry = serde_json::json!({
        "occurred_at": chrono::Utc::now(),
        "action": action,
        "reason": reason,
        "policy_proposal_path": state.policy_proposal_path,
    });
    let mut line = serde_json::to_string(&entry)?;
    line.push('\n');
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    let mut file = options.open(audit_path).await?;
    use tokio::io::AsyncWriteExt;
    file.write_all(line.as_bytes()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
    use axum::routing::any;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio::sync::{Mutex as TokioMutex, Notify};
    use tokio_stream::StreamExt;
    use tower::ServiceExt;

    use crate::contract::{
        AuthorizeRequest, BudgetRule, DecisionOutcome, DecisionSeverity, FinalizeOutcome,
        FinalizeReservation, PolicyAction, PolicyCondition, PolicyRule, RuleMatch, TraceEvent,
        UsageObservation,
    };
    use crate::fixture::{CapturedBody, ResponseSource, list_fixture_paths, read_fixture};
    use crate::policy::PolicyFile;
    use crate::proxy::{ProxyRoute, ProxyRoutes};
    use crate::redaction::REDACTED;

    use super::*;

    fn test_state(policy: Option<PolicyFile>) -> AppState {
        state_with_dir(
            PathBuf::from(".noet/test-fixtures"),
            policy,
            DecisionMode::DryRun,
        )
    }

    fn state_with_dir(
        fixture_dir: PathBuf,
        policy: Option<PolicyFile>,
        decision_mode: DecisionMode,
    ) -> AppState {
        AppState::new(fixture_dir, None, policy, decision_mode)
    }

    fn state_with_routes(fixture_dir: PathBuf, routes: ProxyRoutes) -> AppState {
        let mut state = AppState::new(fixture_dir, None, None, DecisionMode::DryRun);
        state.routes = routes.routes;
        state
    }

    fn state_with_policy_routes(
        fixture_dir: PathBuf,
        policy: PolicyFile,
        decision_mode: DecisionMode,
        routes: ProxyRoutes,
    ) -> AppState {
        let mut state = AppState::new(fixture_dir, None, Some(policy), decision_mode);
        state.routes = routes.routes;
        state
    }

    fn proxy_routes(upstream_base_url: url::Url) -> ProxyRoutes {
        ProxyRoutes {
            routes: vec![ProxyRoute {
                id: "openai-wrapper".to_owned(),
                path_prefix: Some("/providers/openai".to_owned()),
                header_name: None,
                header_value: None,
                upstream_base_url,
            }],
        }
    }

    fn report_request(
        trace_id: &str,
        request_id: &str,
        model: &str,
        estimated_cost_usd: f64,
    ) -> AuthorizeRequest {
        let mut metadata = BTreeMap::new();
        metadata.insert("trace_id".to_owned(), json!(trace_id));
        metadata.insert("request_id".to_owned(), json!(request_id));
        AuthorizeRequest {
            budget_id: None,
            entities: vec!["project:noether".to_owned()],
            subject: Some("user:local".to_owned()),
            project: Some("noether".to_owned()),
            provider: Some("openai".to_owned()),
            model: Some(model.to_owned()),
            estimated_tokens: Some((estimated_cost_usd * 1_000_000.0) as u64),
            estimated_cost_usd: Some(estimated_cost_usd),
            metadata,
        }
    }

    fn finalize_payload(
        trace_id: &str,
        model: &str,
        actual_cost_usd: f64,
        input_tokens: u64,
        output_tokens: u64,
    ) -> FinalizeReservation {
        let mut metadata = BTreeMap::new();
        metadata.insert("trace_id".to_owned(), json!(trace_id));
        FinalizeReservation {
            reservation_id: None,
            outcome: crate::contract::FinalizeOutcome::Success,
            usage: Some(crate::contract::UsageObservation {
                provider: Some("openai".to_owned()),
                model: Some(model.to_owned()),
                input_tokens: Some(input_tokens),
                output_tokens: Some(output_tokens),
                total_tokens: Some(input_tokens + output_tokens),
                cost_usd: Some(actual_cost_usd),
                latency_ms: None,
                stop_reason: None,
            }),
            actual_cost_usd: Some(actual_cost_usd),
            metadata,
        }
    }

    async fn seed_reporting_data(state: &AppState) {
        let mut ledger = state.ledger.lock().await;

        let alpha = ledger
            .try_authorize(
                None,
                &report_request("trace-alpha", "req-alpha", "gpt-4.1", 1.25),
            )
            .expect("authorize alpha");
        let alpha_reservation = alpha.reservation.expect("alpha reservation");
        ledger
            .finalize(
                &alpha_reservation.id,
                &finalize_payload("trace-alpha", "gpt-4.1", 1.25, 1_000, 250),
            )
            .expect("finalize alpha");
        ledger
            .record_event(TraceEvent {
                id: Some("evt-alpha-tool".to_owned()),
                trace_id: Some("trace-alpha".to_owned()),
                occurred_at: None,
                kind: "tool.observed".to_owned(),
                payload: json!({"name":"bash","success":true}),
            })
            .expect("record alpha tool");

        let beta = ledger
            .try_authorize(
                None,
                &report_request("trace-beta", "req-beta", "gpt-4.1-mini", 0.75),
            )
            .expect("authorize beta");
        let beta_reservation = beta.reservation.expect("beta reservation");
        ledger
            .finalize(
                &beta_reservation.id,
                &finalize_payload("trace-beta", "gpt-4.1-mini", 0.75, 400, 120),
            )
            .expect("finalize beta");
        ledger
            .record_event(TraceEvent {
                id: Some("evt-beta-agent".to_owned()),
                trace_id: Some("trace-beta".to_owned()),
                occurred_at: None,
                kind: "pi.agent_context".to_owned(),
                payload: json!({"skill":"research"}),
            })
            .expect("record beta context");
        ledger
            .record_event(TraceEvent {
                id: Some("evt-beta-tool".to_owned()),
                trace_id: Some("trace-beta".to_owned()),
                occurred_at: None,
                kind: "tool.observed".to_owned(),
                payload: json!({"name":"rg","success":true}),
            })
            .expect("record beta tool");
    }

    fn checked_in_simulation(path: &str) -> crate::simulation::SimulationFile {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let content = std::fs::read_to_string(manifest_dir.join(path))
            .expect("checked-in simulation example is readable");
        serde_yaml::from_str(&content).expect("checked-in simulation example parses")
    }

    fn seed_simulation_artifacts(
        simulation_dir: &Path,
    ) -> crate::simulation::SimulationComparisonReport {
        std::fs::create_dir_all(simulation_dir).expect("simulation dir");
        let out_dir = simulation_dir.join("runaway-pressure");
        let simulation = checked_in_simulation("examples/simulations/runaway-pressure.noet.yaml");
        let report =
            crate::simulation::compare_strategies(&simulation, &out_dir).expect("simulation run");
        std::fs::write(
            out_dir.join("simulation-report.json"),
            serde_json::to_vec_pretty(&report).expect("simulation report json"),
        )
        .expect("write simulation report");
        std::fs::write(
            out_dir.join("simulation-dashboard.html"),
            "<!doctype html><html><body><h1>Simulation comparison dashboard</h1><p>Budget limits changed the spend story.</p></body></html>",
        )
        .expect("write simulation dashboard");
        for strategy in &report.strategies {
            let strategy_dir = out_dir.join(&strategy.db_path);
            let strategy_dir = strategy_dir.parent().expect("strategy dir");
            std::fs::write(
                strategy_dir.join("noether-dashboard.html"),
                format!(
                    "<!doctype html><html><body><h1>Strategy dashboard</h1><p>{}</p></body></html>",
                    strategy.id
                ),
            )
            .expect("write strategy dashboard");
        }
        report
    }

    fn strict_policy() -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "tiny".to_owned(),
                priority: 0,
                models: Default::default(),
                limits: crate::contract::BudgetLimitPolicy {
                    request_cost: None,
                    context_tokens: None,
                    spend: vec![crate::contract::SpendWindowLimit {
                        id: Some("budget-cap".to_owned()),
                        by: crate::contract::SpendWindowBy::Global,
                        window: "60s".to_owned(),
                        mode: Some(crate::contract::SpendWindowMode::Tumbling),
                        anchor: Some(crate::contract::WindowAnchorPolicy {
                            kind: crate::contract::WindowAnchorKind::FirstSeen,
                        }),
                        max_usd: 0.01,
                        warn_at_fractions: vec![0.8],
                        action: crate::contract::PolicyAction::Block,
                    }],
                    tool_calls: None,
                    agent_steps: None,
                    retries: None,
                },
                allocation: None,
                rule_match: RuleMatch::default(),
            }],
            policies: Vec::new(),
        }
    }

    fn require_project_policy() -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: Vec::new(),
            policies: vec![PolicyRule {
                id: "require-project".to_owned(),
                action: PolicyAction::Block,
                reason: "project is required".to_owned(),
                when: PolicyCondition {
                    missing: Some("project".to_owned()),
                    rule_match: RuleMatch::default(),
                },
            }],
        }
    }

    fn warn_project_policy() -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "personal-local".to_owned(),
                priority: 0,
                models: crate::contract::BudgetModelPolicy {
                    allow: vec!["openai:gpt-4.1".to_owned()],
                },
                limits: crate::contract::BudgetLimitPolicy {
                    request_cost: None,
                    context_tokens: Some(crate::contract::ContextTokenLimit {
                        max_tokens: 1_000,
                        action: crate::contract::PolicyAction::Warn,
                    }),
                    spend: vec![crate::contract::SpendWindowLimit {
                        id: Some("daily-cap".to_owned()),
                        by: crate::contract::SpendWindowBy::Global,
                        window: "1d".to_owned(),
                        mode: Some(crate::contract::SpendWindowMode::Tumbling),
                        anchor: Some(crate::contract::WindowAnchorPolicy {
                            kind: crate::contract::WindowAnchorKind::FirstSeen,
                        }),
                        max_usd: 1000.0,
                        warn_at_fractions: vec![1.0],
                        action: crate::contract::PolicyAction::Warn,
                    }],
                    tool_calls: None,
                    agent_steps: None,
                    retries: None,
                },
                allocation: None,
                rule_match: RuleMatch::default(),
            }],
            policies: vec![PolicyRule {
                id: "require-project".to_owned(),
                action: PolicyAction::Warn,
                reason: "project should be present for attribution".to_owned(),
                when: PolicyCondition {
                    missing: Some("project".to_owned()),
                    rule_match: RuleMatch::default(),
                },
            }],
        }
    }

    fn model_locked_policy() -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "personal-local".to_owned(),
                priority: 0,
                models: crate::contract::BudgetModelPolicy {
                    allow: vec!["openai-codex:gpt-4.1".to_owned()],
                },
                limits: crate::contract::BudgetLimitPolicy {
                    request_cost: None,
                    context_tokens: None,
                    spend: vec![crate::contract::SpendWindowLimit {
                        id: Some("daily-cap".to_owned()),
                        by: crate::contract::SpendWindowBy::Global,
                        window: "1d".to_owned(),
                        mode: Some(crate::contract::SpendWindowMode::Tumbling),
                        anchor: Some(crate::contract::WindowAnchorPolicy {
                            kind: crate::contract::WindowAnchorKind::FirstSeen,
                        }),
                        max_usd: 1000.0,
                        warn_at_fractions: vec![0.8],
                        action: crate::contract::PolicyAction::Block,
                    }],
                    tool_calls: None,
                    agent_steps: None,
                    retries: None,
                },
                allocation: None,
                rule_match: RuleMatch::default(),
            }],
            policies: Vec::new(),
        }
    }

    #[derive(Clone, Debug)]
    struct ObservedUpstreamRequest {
        method: String,
        path_and_query: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    struct TestUpstream {
        base_url: url::Url,
        observed: Arc<TokioMutex<Vec<ObservedUpstreamRequest>>>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestUpstream {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    async fn start_upstream(
        status: StatusCode,
        response_headers: Vec<(&'static str, &'static str)>,
        response_body: &'static [u8],
    ) -> TestUpstream {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let addr = listener.local_addr().expect("upstream local addr");
        let observed = Arc::new(TokioMutex::new(Vec::new()));
        let handler_observed = Arc::clone(&observed);
        let response_bytes = response_body.to_vec();
        let app = Router::new().fallback(any(
            move |method: Method, uri: Uri, headers: HeaderMap, body: axum::body::Bytes| {
                let handler_observed = Arc::clone(&handler_observed);
                let response_headers = response_headers.clone();
                let response_bytes = response_bytes.clone();
                async move {
                    handler_observed.lock().await.push(ObservedUpstreamRequest {
                        method: method.to_string(),
                        path_and_query: uri
                            .path_and_query()
                            .map(|path| path.as_str().to_owned())
                            .unwrap_or_else(|| uri.path().to_owned()),
                        headers: headers
                            .iter()
                            .map(|(name, value)| {
                                (
                                    name.as_str().to_ascii_lowercase(),
                                    value.to_str().unwrap_or("<non-utf8>").to_owned(),
                                )
                            })
                            .collect(),
                        body: body.to_vec(),
                    });
                    let mut response = axum::response::Response::new(Body::from(response_bytes));
                    *response.status_mut() = status;
                    for (name, value) in response_headers {
                        response.headers_mut().insert(
                            axum::http::HeaderName::from_static(name),
                            axum::http::HeaderValue::from_static(value),
                        );
                    }
                    response
                }
            },
        ));
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("upstream server");
        });

        TestUpstream {
            base_url: url::Url::parse(&format!("http://{addr}/")).expect("upstream url"),
            observed,
            handle,
        }
    }

    async fn start_streaming_upstream(
        first_chunk: &'static [u8],
        second_chunk: &'static [u8],
    ) -> (TestUpstream, Arc<Notify>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let addr = listener.local_addr().expect("upstream local addr");
        let observed = Arc::new(TokioMutex::new(Vec::new()));
        let handler_observed = Arc::clone(&observed);
        let release_completion = Arc::new(Notify::new());
        let handler_release_completion = Arc::clone(&release_completion);
        let app = Router::new().fallback(any(
            move |method: Method, uri: Uri, headers: HeaderMap, body: axum::body::Bytes| {
                let handler_observed = Arc::clone(&handler_observed);
                let handler_release_completion = Arc::clone(&handler_release_completion);
                async move {
                    handler_observed.lock().await.push(ObservedUpstreamRequest {
                        method: method.to_string(),
                        path_and_query: uri
                            .path_and_query()
                            .map(|path| path.as_str().to_owned())
                            .unwrap_or_else(|| uri.path().to_owned()),
                        headers: headers
                            .iter()
                            .map(|(name, value)| {
                                (
                                    name.as_str().to_ascii_lowercase(),
                                    value.to_str().unwrap_or("<non-utf8>").to_owned(),
                                )
                            })
                            .collect(),
                        body: body.to_vec(),
                    });

                    let (sender, receiver) =
                        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(1);
                    tokio::spawn(async move {
                        sender
                            .send(Ok(axum::body::Bytes::from_static(first_chunk)))
                            .await
                            .expect("send first upstream chunk");
                        handler_release_completion.notified().await;
                        sender
                            .send(Ok(axum::body::Bytes::from_static(second_chunk)))
                            .await
                            .expect("send second upstream chunk");
                    });

                    let mut response = axum::response::Response::new(Body::from_stream(
                        tokio_stream::wrappers::ReceiverStream::new(receiver),
                    ));
                    *response.status_mut() = StatusCode::OK;
                    response.headers_mut().insert(
                        axum::http::header::CONTENT_TYPE,
                        axum::http::HeaderValue::from_static("text/event-stream"),
                    );
                    response.headers_mut().insert(
                        axum::http::header::CACHE_CONTROL,
                        axum::http::HeaderValue::from_static("no-cache"),
                    );
                    response
                }
            },
        ));
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("upstream server");
        });

        (
            TestUpstream {
                base_url: url::Url::parse(&format!("http://{addr}/")).expect("upstream url"),
                observed,
                handle,
            },
            release_completion,
        )
    }

    #[tokio::test]
    async fn transparent_route_preserves_request_and_response_without_translation() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture_dir = tempdir.path().join("fixtures");
        let upstream = start_upstream(
            StatusCode::ACCEPTED,
            vec![
                ("x-upstream-reply", "preserved"),
                ("set-cookie", "session=secret"),
            ],
            br#"{"ok":true}"#,
        )
        .await;
        let app = build_router(state_with_routes(
            fixture_dir.clone(),
            ProxyRoutes {
                routes: vec![ProxyRoute {
                    id: "openai-wrapper".to_owned(),
                    path_prefix: Some("/providers/openai".to_owned()),
                    header_name: None,
                    header_value: None,
                    upstream_base_url: upstream.base_url.clone(),
                }],
            },
        ));
        let request_body = r#"{"model":"gpt-test", "api_key":"sk-body", "messages":[]}"#;

        let response = app
            .oneshot(
                Request::put("/providers/openai/v1/chat/completions?stream=false")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer sk-test")
                    .header("x-account-id", "acct_123")
                    .header("x-noet-provider", "openai")
                    .body(Body::from(request_body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            response
                .headers()
                .get("x-upstream-reply")
                .and_then(|value| value.to_str().ok()),
            Some("preserved")
        );
        assert_eq!(
            response
                .headers()
                .get("set-cookie")
                .and_then(|value| value.to_str().ok()),
            Some("session=secret")
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&body[..], br#"{"ok":true}"#);

        let observed = upstream.observed.lock().await;
        assert_eq!(observed.len(), 1);
        let request = &observed[0];
        assert_eq!(request.method, "PUT");
        assert_eq!(request.path_and_query, "/v1/chat/completions?stream=false");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer sk-test")
        );
        assert_eq!(
            request.headers.get("x-account-id").map(String::as_str),
            Some("acct_123")
        );
        assert_eq!(
            request.headers.get("x-noet-provider").map(String::as_str),
            Some("openai")
        );
        assert_eq!(
            std::str::from_utf8(&request.body).expect("utf8 body"),
            request_body
        );
        drop(observed);

        let paths = list_fixture_paths(&fixture_dir)
            .await
            .expect("fixture paths");
        let fixture = read_fixture(&paths[0]).await.expect("fixture");
        assert_eq!(fixture.response.source, ResponseSource::Upstream);
        assert_eq!(
            fixture
                .request
                .headers
                .get("authorization")
                .map(String::as_str),
            Some(REDACTED)
        );
        assert_eq!(
            fixture
                .response
                .headers
                .get("set-cookie")
                .map(String::as_str),
            Some(REDACTED)
        );
        match fixture.request.body {
            CapturedBody::Json { value } => assert_eq!(value["api_key"], REDACTED),
            CapturedBody::Text { .. } | CapturedBody::Binary { .. } | CapturedBody::Empty => {
                panic!("expected captured JSON")
            }
        }
    }

    #[tokio::test]
    async fn transparent_route_dry_run_deny_forwards_and_records_decision() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture_dir = tempdir.path().join("fixtures");
        let upstream = start_upstream(StatusCode::OK, Vec::new(), b"allowed by dry run").await;
        let app = build_router(state_with_policy_routes(
            fixture_dir.clone(),
            require_project_policy(),
            DecisionMode::DryRun,
            proxy_routes(upstream.base_url.clone()),
        ));

        let response = app
            .oneshot(
                Request::post("/providers/openai/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"model":"gpt-test"}).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(upstream.observed.lock().await.len(), 1);

        let paths = list_fixture_paths(&fixture_dir)
            .await
            .expect("fixture paths");
        let fixture = read_fixture(&paths[0]).await.expect("fixture");
        assert_eq!(fixture.response.source, ResponseSource::Upstream);
        let decision = fixture.decision.expect("decision metadata");
        assert_eq!(decision.mode, DecisionMode::DryRun);
        assert_eq!(
            decision.decision.outcome,
            crate::contract::DecisionOutcome::Deny
        );
    }

    #[tokio::test]
    async fn transparent_route_enforce_deny_does_not_call_upstream() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture_dir = tempdir.path().join("fixtures");
        let upstream = start_upstream(StatusCode::OK, Vec::new(), b"should not be called").await;
        let app = build_router(state_with_policy_routes(
            fixture_dir.clone(),
            require_project_policy(),
            DecisionMode::Enforce,
            proxy_routes(upstream.base_url.clone()),
        ));

        let response = app
            .oneshot(
                Request::post("/providers/openai/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"model":"gpt-test"}).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(upstream.observed.lock().await.is_empty());

        let paths = list_fixture_paths(&fixture_dir)
            .await
            .expect("fixture paths");
        let fixture = read_fixture(&paths[0]).await.expect("fixture");
        assert_eq!(fixture.response.source, ResponseSource::DecisionDenied);
    }

    #[tokio::test]
    async fn transparent_route_streams_first_chunk_before_upstream_completes() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture_dir = tempdir.path().join("fixtures");
        let (upstream, release_completion) =
            start_streaming_upstream(b"data: first\n\n", b"data: second\n\n").await;
        let app = build_router(state_with_policy_routes(
            fixture_dir.clone(),
            require_project_policy(),
            DecisionMode::DryRun,
            proxy_routes(upstream.base_url.clone()),
        ));

        let response = app
            .oneshot(
                Request::post("/providers/openai/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"model":"gpt-test"}).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let mut body = response.into_body().into_data_stream();
        let first = tokio::time::timeout(Duration::from_millis(500), body.next())
            .await
            .expect("first chunk before upstream completion")
            .expect("first chunk present")
            .expect("first chunk ok");
        assert_eq!(&first[..], b"data: first\n\n");
        let fixture_paths_before_completion = if fixture_dir.exists() {
            list_fixture_paths(&fixture_dir)
                .await
                .expect("fixture paths before completion")
        } else {
            Vec::new()
        };
        assert!(fixture_paths_before_completion.is_empty());

        release_completion.notify_one();
        let second = tokio::time::timeout(Duration::from_millis(500), body.next())
            .await
            .expect("second chunk after release")
            .expect("second chunk present")
            .expect("second chunk ok");
        assert_eq!(&second[..], b"data: second\n\n");
        assert!(body.next().await.is_none());

        let paths = tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                let paths = list_fixture_paths(&fixture_dir)
                    .await
                    .expect("fixture paths after completion");
                if !paths.is_empty() {
                    break paths;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fixture written after stream completion");
        let fixture = read_fixture(&paths[0]).await.expect("fixture");
        assert_eq!(fixture.response.source, ResponseSource::Upstream);
        match fixture.response.body {
            CapturedBody::Binary { bytes } => assert_eq!(bytes, 27),
            CapturedBody::Json { .. } | CapturedBody::Text { .. } | CapturedBody::Empty => {
                panic!("expected streaming fixture body summary")
            }
        }
        assert_eq!(fixture.response.chunks.len(), 2);
        assert_eq!(
            fixture.response.chunks[0].text.as_deref(),
            Some("data: first\n\n")
        );
        assert_eq!(
            fixture.response.chunks[1].text.as_deref(),
            Some("data: second\n\n")
        );
        assert!(fixture.response.error.is_none());
        let decision = fixture.decision.expect("decision metadata");
        assert_eq!(decision.mode, DecisionMode::DryRun);
        assert_eq!(
            decision.decision.outcome,
            crate::contract::DecisionOutcome::Deny
        );
    }

    #[tokio::test]
    async fn authorize_endpoint_creates_reservation_for_allowed_request() {
        let app = build_router(test_state(Some(strict_policy())));
        let response = app
            .oneshot(
                Request::post("/v1/authorize")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"project":"noether","estimated_cost_usd":0.001}).to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["outcome"], "allow");
        assert!(value["reservation"]["id"].is_string());
    }

    #[tokio::test]
    async fn finalize_endpoint_is_idempotent() {
        let app = build_router(test_state(Some(strict_policy())));
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/authorize")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"project":"noether","estimated_cost_usd":0.001}).to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("authorize response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("authorize body");
        let decision: Value = serde_json::from_slice(&body).expect("authorize json");
        let reservation_id = decision["reservation"]["id"]
            .as_str()
            .expect("reservation id");

        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::post(format!("/v1/reservations/{reservation_id}/finalize"))
                        .header("content-type", "application/json")
                        .body(Body::from(json!({"actual_cost_usd":0.001}).to_string()))
                        .expect("request"),
                )
                .await
                .expect("finalize response");
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn finalize_endpoint_rejects_invalid_accounting() {
        let app = build_router(test_state(Some(strict_policy())));
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/authorize")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"project":"noether","estimated_cost_usd":0.001}).to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("authorize response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("authorize body");
        let decision: Value = serde_json::from_slice(&body).expect("authorize json");
        let reservation_id = decision["reservation"]["id"]
            .as_str()
            .expect("reservation id");

        let response = app
            .oneshot(
                Request::post(format!("/v1/reservations/{reservation_id}/finalize"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "outcome":"success",
                            "actual_cost_usd": -0.001
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("finalize response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn events_endpoint_accepts_trace_events() {
        let app = build_router(test_state(None));
        let response = app
            .oneshot(
                Request::post("/v1/events")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"kind":"request.completed","payload":{"ok":true}}).to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn reporting_endpoints_match_shared_reporting_domain() {
        let state = test_state(None);
        seed_reporting_data(&state).await;

        let expected_usage = {
            let ledger = state.ledger.lock().await;
            serde_json::to_value(reporting::usage_report(&ledger).expect("usage report"))
                .expect("usage json")
        };
        let expected_decisions = {
            let ledger = state.ledger.lock().await;
            serde_json::to_value(reporting::decisions_report(&ledger).expect("decisions report"))
                .expect("decisions json")
        };
        let expected_trace = {
            let ledger = state.ledger.lock().await;
            serde_json::to_value(reporting::trace_report(&ledger, "trace-beta").expect("trace"))
                .expect("trace json")
        };
        let expected_observations = {
            let ledger = state.ledger.lock().await;
            serde_json::to_value(
                reporting::observations_report(&ledger, Some("tool"), Some("trace-beta"))
                    .expect("observations"),
            )
            .expect("observations json")
        };
        let app = build_router(state);

        let usage_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/reports/usage")
                    .body(Body::empty())
                    .expect("usage request"),
            )
            .await
            .expect("usage response");
        assert_eq!(usage_response.status(), StatusCode::OK);
        let usage_body = to_bytes(usage_response.into_body(), usize::MAX)
            .await
            .expect("usage body");
        let usage_json: Value = serde_json::from_slice(&usage_body).expect("usage value");
        assert_eq!(usage_json, expected_usage);

        let decisions_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/reports/decisions")
                    .body(Body::empty())
                    .expect("decisions request"),
            )
            .await
            .expect("decisions response");
        assert_eq!(decisions_response.status(), StatusCode::OK);
        let decisions_body = to_bytes(decisions_response.into_body(), usize::MAX)
            .await
            .expect("decisions body");
        let decisions_json: Value =
            serde_json::from_slice(&decisions_body).expect("decisions value");
        assert_eq!(decisions_json, expected_decisions);

        let trace_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/reports/traces/trace-beta")
                    .body(Body::empty())
                    .expect("trace request"),
            )
            .await
            .expect("trace response");
        assert_eq!(trace_response.status(), StatusCode::OK);
        let trace_body = to_bytes(trace_response.into_body(), usize::MAX)
            .await
            .expect("trace body");
        let trace_json: Value = serde_json::from_slice(&trace_body).expect("trace value");
        assert_eq!(trace_json, expected_trace);

        let observations_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/reports/observations?kind=tool&trace=trace-beta")
                    .body(Body::empty())
                    .expect("observations request"),
            )
            .await
            .expect("observations response");
        assert_eq!(observations_response.status(), StatusCode::OK);
        let observations_body = to_bytes(observations_response.into_body(), usize::MAX)
            .await
            .expect("observations body");
        let observations_json: Value =
            serde_json::from_slice(&observations_body).expect("observations value");
        assert_eq!(observations_json, expected_observations);
    }

    #[tokio::test]
    async fn simulation_surfaces_have_coherent_empty_states() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = test_state(None);
        state.simulation_dir = tempdir.path().join("simulations");
        let app = build_router(state);

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/simulations")
                    .body(Body::empty())
                    .expect("simulation list request"),
            )
            .await
            .expect("simulation list response");
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = to_bytes(list_response.into_body(), usize::MAX)
            .await
            .expect("simulation list body");
        let list_json: Value = serde_json::from_slice(&list_body).expect("simulation list json");
        assert_eq!(list_json, json!([]));

        let html_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/simulations")
                    .body(Body::empty())
                    .expect("simulation html request"),
            )
            .await
            .expect("simulation html response");
        assert_eq!(html_response.status(), StatusCode::OK);
        let html_body = to_bytes(html_response.into_body(), usize::MAX)
            .await
            .expect("simulation html body");
        let html = String::from_utf8(html_body.to_vec()).expect("simulation html");
        assert!(html.contains("Noether simulation surfaces"));
        assert!(html.contains("No simulation artifacts are available yet"));

        let missing_response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/simulations/missing")
                    .body(Body::empty())
                    .expect("missing simulation request"),
            )
            .await
            .expect("missing simulation response");
        assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn simulation_routes_serve_generated_artifacts_and_strategy_surfaces() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = test_state(None);
        state.simulation_dir = tempdir.path().join("simulations");
        let report = seed_simulation_artifacts(&state.simulation_dir);
        let simulation_root = state.simulation_dir.join("runaway-pressure");
        let expected_report: Value = serde_json::from_slice(
            &std::fs::read(simulation_root.join("simulation-report.json"))
                .expect("simulation report artifact"),
        )
        .expect("expected simulation report");
        let first_strategy = report
            .strategies
            .first()
            .expect("simulation strategy")
            .clone();
        let expected_usage: Value = serde_json::from_slice(
            &std::fs::read(simulation_root.join(&first_strategy.usage_report_path))
                .expect("strategy usage report"),
        )
        .expect("expected usage json");
        let expected_decisions: Value = serde_json::from_slice(
            &std::fs::read(simulation_root.join(&first_strategy.decisions_report_path))
                .expect("strategy decisions report"),
        )
        .expect("expected decisions json");

        let app = build_router(state);

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/simulations")
                    .body(Body::empty())
                    .expect("simulation list request"),
            )
            .await
            .expect("simulation list response");
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = to_bytes(list_response.into_body(), usize::MAX)
            .await
            .expect("simulation list body");
        let list_json: Value = serde_json::from_slice(&list_body).expect("simulation list json");
        assert_eq!(list_json.as_array().map(Vec::len), Some(1));
        assert_eq!(list_json[0]["id"], "runaway-pressure");
        assert_eq!(
            list_json[0]["dashboard_url"],
            "/v1/simulations/runaway-pressure/dashboard"
        );
        assert!(
            list_json[0]["strategies"][0]["dashboard_url"]
                .as_str()
                .expect("strategy dashboard url")
                .contains("%20")
        );

        let report_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/simulations/runaway-pressure")
                    .body(Body::empty())
                    .expect("simulation report request"),
            )
            .await
            .expect("simulation report response");
        assert_eq!(report_response.status(), StatusCode::OK);
        let report_body = to_bytes(report_response.into_body(), usize::MAX)
            .await
            .expect("simulation report body");
        let report_json: Value = serde_json::from_slice(&report_body).expect("simulation report");
        assert_eq!(report_json, expected_report);

        let dashboard_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/simulations/runaway-pressure/dashboard")
                    .body(Body::empty())
                    .expect("simulation dashboard request"),
            )
            .await
            .expect("simulation dashboard response");
        assert_eq!(dashboard_response.status(), StatusCode::OK);
        let dashboard_body = to_bytes(dashboard_response.into_body(), usize::MAX)
            .await
            .expect("simulation dashboard body");
        let dashboard_html =
            String::from_utf8(dashboard_body.to_vec()).expect("simulation dashboard html");
        assert!(dashboard_html.contains("Simulation comparison dashboard"));
        assert!(dashboard_html.contains("Budget limits changed the spend story."));

        let encoded_strategy_id = percent_encode_path_component(&first_strategy.id);
        let usage_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/simulations/runaway-pressure/strategies/{encoded_strategy_id}/usage"
                    ))
                    .body(Body::empty())
                    .expect("strategy usage request"),
            )
            .await
            .expect("strategy usage response");
        assert_eq!(usage_response.status(), StatusCode::OK);
        let usage_body = to_bytes(usage_response.into_body(), usize::MAX)
            .await
            .expect("strategy usage body");
        let usage_json: Value = serde_json::from_slice(&usage_body).expect("strategy usage json");
        assert_eq!(usage_json, expected_usage);

        let decisions_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/simulations/runaway-pressure/strategies/{encoded_strategy_id}/decisions"
                    ))
                    .body(Body::empty())
                    .expect("strategy decisions request"),
            )
            .await
            .expect("strategy decisions response");
        assert_eq!(decisions_response.status(), StatusCode::OK);
        let decisions_body = to_bytes(decisions_response.into_body(), usize::MAX)
            .await
            .expect("strategy decisions body");
        let decisions_json: Value =
            serde_json::from_slice(&decisions_body).expect("strategy decisions json");
        assert_eq!(decisions_json, expected_decisions);

        let strategy_dashboard_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/simulations/runaway-pressure/strategies/{encoded_strategy_id}/dashboard"
                    ))
                    .body(Body::empty())
                    .expect("strategy dashboard request"),
            )
            .await
            .expect("strategy dashboard response");
        assert_eq!(strategy_dashboard_response.status(), StatusCode::OK);
        let strategy_dashboard_body = to_bytes(strategy_dashboard_response.into_body(), usize::MAX)
            .await
            .expect("strategy dashboard body");
        let strategy_dashboard_html =
            String::from_utf8(strategy_dashboard_body.to_vec()).expect("strategy dashboard html");
        assert!(strategy_dashboard_html.contains("Strategy dashboard"));
        assert!(strategy_dashboard_html.contains(&first_strategy.id));

        let index_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/simulations")
                    .body(Body::empty())
                    .expect("simulation index request"),
            )
            .await
            .expect("simulation index response");
        assert_eq!(index_response.status(), StatusCode::OK);
        let index_body = to_bytes(index_response.into_body(), usize::MAX)
            .await
            .expect("simulation index body");
        let index_html = String::from_utf8(index_body.to_vec()).expect("simulation index html");
        assert!(index_html.contains("Noether simulation surfaces"));
        assert!(index_html.contains("Simulation comparison surface"));
        assert!(index_html.contains(&first_strategy.id));

        let missing_strategy_response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/simulations/runaway-pressure/strategies/missing/dashboard")
                    .body(Body::empty())
                    .expect("missing strategy request"),
            )
            .await
            .expect("missing strategy response");
        assert_eq!(missing_strategy_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn simulation_strategy_routes_ignore_tampered_report_paths() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = test_state(None);
        state.simulation_dir = tempdir.path().join("simulations");
        let report = seed_simulation_artifacts(&state.simulation_dir);
        let simulation_root = state.simulation_dir.join("runaway-pressure");
        let first_strategy = report
            .strategies
            .first()
            .expect("simulation strategy")
            .clone();
        let expected_usage: Value = serde_json::from_slice(
            &std::fs::read(simulation_root.join(&first_strategy.usage_report_path))
                .expect("strategy usage report"),
        )
        .expect("expected usage json");
        let expected_decisions: Value = serde_json::from_slice(
            &std::fs::read(simulation_root.join(&first_strategy.decisions_report_path))
                .expect("strategy decisions report"),
        )
        .expect("expected decisions json");

        let escaped_dir = tempdir.path().join("escaped");
        std::fs::create_dir_all(&escaped_dir).expect("escaped dir");
        std::fs::write(
            escaped_dir.join("usage-report.json"),
            serde_json::to_vec(&json!([{ "escaped": true }])).expect("escaped usage json"),
        )
        .expect("write escaped usage");
        std::fs::write(
            escaped_dir.join("decisions-report.json"),
            serde_json::to_vec(&json!([{ "escaped": true }])).expect("escaped decisions json"),
        )
        .expect("write escaped decisions");
        std::fs::write(
            escaped_dir.join("noether-dashboard.html"),
            "<!doctype html><html><body>escaped dashboard</body></html>",
        )
        .expect("write escaped dashboard");

        let report_path = simulation_root.join("simulation-report.json");
        let mut tampered_report: crate::simulation::SimulationComparisonReport =
            serde_json::from_slice(&std::fs::read(&report_path).expect("simulation report"))
                .expect("parse simulation report");
        let strategy = tampered_report
            .strategies
            .first_mut()
            .expect("tampered strategy");
        strategy.usage_report_path = escaped_dir.join("usage-report.json");
        strategy.decisions_report_path = escaped_dir.join("decisions-report.json");
        strategy.db_path = escaped_dir.join("simulation.sqlite");
        std::fs::write(
            &report_path,
            serde_json::to_vec_pretty(&tampered_report).expect("tampered report json"),
        )
        .expect("write tampered report");

        let app = build_router(state);
        let encoded_strategy_id = percent_encode_path_component(&first_strategy.id);

        let usage_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/simulations/runaway-pressure/strategies/{encoded_strategy_id}/usage"
                    ))
                    .body(Body::empty())
                    .expect("strategy usage request"),
            )
            .await
            .expect("strategy usage response");
        assert_eq!(usage_response.status(), StatusCode::OK);
        let usage_body = to_bytes(usage_response.into_body(), usize::MAX)
            .await
            .expect("strategy usage body");
        let usage_json: Value = serde_json::from_slice(&usage_body).expect("strategy usage json");
        assert_eq!(usage_json, expected_usage);

        let decisions_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/simulations/runaway-pressure/strategies/{encoded_strategy_id}/decisions"
                    ))
                    .body(Body::empty())
                    .expect("strategy decisions request"),
            )
            .await
            .expect("strategy decisions response");
        assert_eq!(decisions_response.status(), StatusCode::OK);
        let decisions_body = to_bytes(decisions_response.into_body(), usize::MAX)
            .await
            .expect("strategy decisions body");
        let decisions_json: Value =
            serde_json::from_slice(&decisions_body).expect("strategy decisions json");
        assert_eq!(decisions_json, expected_decisions);

        let strategy_dashboard_response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/simulations/runaway-pressure/strategies/{encoded_strategy_id}/dashboard"
                    ))
                    .body(Body::empty())
                    .expect("strategy dashboard request"),
            )
            .await
            .expect("strategy dashboard response");
        assert_eq!(strategy_dashboard_response.status(), StatusCode::OK);
        let strategy_dashboard_body = to_bytes(strategy_dashboard_response.into_body(), usize::MAX)
            .await
            .expect("strategy dashboard body");
        let strategy_dashboard_html =
            String::from_utf8(strategy_dashboard_body.to_vec()).expect("strategy dashboard html");
        assert!(strategy_dashboard_html.contains("Strategy dashboard"));
        assert!(strategy_dashboard_html.contains(&first_strategy.id));
        assert!(!strategy_dashboard_html.contains("escaped dashboard"));
    }

    #[tokio::test]
    async fn noether_app_serves_policy_runs_replay_shell() {
        let app = build_router(test_state(None));

        for route in ["/", "/policy", "/runs", "/replay"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(route)
                        .body(Body::empty())
                        .expect("app shell request"),
                )
                .await
                .expect("app shell response");
            assert_eq!(response.status(), StatusCode::OK, "route {route}");
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("app shell body");
            let html = String::from_utf8(body.to_vec()).expect("app shell html");
            for marker in [
                "noether",
                "What&apos;s allowed here.",
                "What actually happened.",
                "What would change.",
                "/app/app.js",
                "/app/app.css",
                "ask Noether...",
            ] {
                assert!(
                    html.contains(marker),
                    "route {route} missing app marker: {marker}"
                );
            }
        }
    }

    #[tokio::test]
    async fn noether_app_assets_are_new_product_shell_assets() {
        let app = build_router(test_state(None));
        let js_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/app/app.js")
                    .body(Body::empty())
                    .expect("js request"),
            )
            .await
            .expect("js response");
        assert_eq!(js_response.status(), StatusCode::OK);
        let js_body = to_bytes(js_response.into_body(), usize::MAX)
            .await
            .expect("js body");
        let js = String::from_utf8(js_body.to_vec()).expect("app js");
        assert!(js.contains("modeFromPath"));
        assert!(js.contains("policy"));
        assert!(js.contains("runs"));
        assert!(js.contains("replay"));
        assert!(!js.contains("/v1/dashboard"));
        assert!(!js.contains("<html"));

        let css_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/app/app.css")
                    .body(Body::empty())
                    .expect("css request"),
            )
            .await
            .expect("css response");
        assert_eq!(css_response.status(), StatusCode::OK);
        let css_body = to_bytes(css_response.into_body(), usize::MAX)
            .await
            .expect("css body");
        let css = String::from_utf8(css_body.to_vec()).expect("app css");
        assert!(css.contains(".policy-grid"));
        assert!(css.contains(".runs-table"));
        assert!(css.contains(".scenarios"));
        assert!(css.contains(".editor-source"));
        assert!(!css.contains(".dashboard-shell"));

        let logo_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/app/logo.svg")
                    .body(Body::empty())
                    .expect("logo request"),
            )
            .await
            .expect("logo response");
        assert_eq!(logo_response.status(), StatusCode::OK);
        let logo_body = to_bytes(logo_response.into_body(), usize::MAX)
            .await
            .expect("logo body");
        let logo_svg = String::from_utf8(logo_body.to_vec()).expect("logo svg");
        assert!(logo_svg.contains("<svg"));
    }

    #[tokio::test]
    async fn health_exposes_sidecar_readiness() {
        let app = build_router(test_state(Some(model_locked_policy())));

        let health_response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("health request"),
            )
            .await
            .expect("health response");
        assert_eq!(health_response.status(), StatusCode::OK);
        let health_body = to_bytes(health_response.into_body(), usize::MAX)
            .await
            .expect("health body");
        let health_json: serde_json::Value =
            serde_json::from_slice(&health_body).expect("health json");
        assert_eq!(health_json["status"], "ok");
        assert_eq!(health_json["policy_loaded"], true);
        assert_eq!(health_json["decision_mode"], "dry_run");
        assert_eq!(health_json["ledger_backend"], "in_memory");
    }

    #[tokio::test]
    async fn serves_openapi_json_and_api_docs() {
        let app = build_router(test_state(Some(model_locked_policy())));

        let openapi_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .expect("openapi request"),
            )
            .await
            .expect("openapi response");
        assert_eq!(openapi_response.status(), StatusCode::OK);
        assert_eq!(
            openapi_response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json; charset=utf-8")
        );
        let openapi_body = to_bytes(openapi_response.into_body(), usize::MAX)
            .await
            .expect("openapi body");
        let spec: serde_json::Value = serde_json::from_slice(&openapi_body).expect("openapi json");
        assert_eq!(spec["info"]["title"], "Noether Sidecar API");
        assert!(
            spec["info"]["description"]
                .as_str()
                .expect("description")
                .contains("Noether does not call model providers")
        );
        assert!(spec["paths"]["/v1/authorize"]["post"].is_object());
        assert!(spec["paths"]["/v1/reservations/{id}/finalize"]["post"].is_object());
        assert!(spec["paths"]["/v1/events"]["post"].is_object());
        assert!(spec["paths"]["/health"]["get"].is_object());

        for path in ["/docs", "/api/docs"] {
            let docs_response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("docs request"),
                )
                .await
                .expect("docs response");
            assert_eq!(docs_response.status(), StatusCode::OK);
            let docs_body = to_bytes(docs_response.into_body(), usize::MAX)
                .await
                .expect("docs body");
            let html = String::from_utf8(docs_body.to_vec()).expect("docs html");
            assert!(html.contains("Noether Sidecar API"));
            assert!(html.contains("/openapi.json"));
            assert!(html.contains("Noether does not call model providers"));
        }
    }

    #[tokio::test]
    async fn noether_app_policy_source_is_clean_user_yaml() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let policy_path = tempdir.path().join("policy.yaml");
        std::fs::write(
            &policy_path,
            r#"
version: 0
routing:
  mode: explicit_then_fallback
  specificity: [project, user, team, group, org, global]
budgets:
  - id: personal-local
    priority: 0
    match:
      subject: null
      user: null
      project: null
      team: null
      group: null
      org: null
      workflow: null
      surface: null
      provider: null
      model: null
    limits:
      spend:
        - id: daily-cap
          window: 1d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 100
          action: block
"#,
        )
        .expect("write policy");
        let policy = crate::policy::load_policy(&policy_path)
            .await
            .expect("load policy");
        let state = AppState::with_reloadable_policy(
            tempdir.path().join("fixtures"),
            None,
            policy_path,
            policy,
            DecisionMode::DryRun,
        );
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/app/policy")
                    .body(Body::empty())
                    .expect("policy request"),
            )
            .await
            .expect("policy response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("policy body");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("policy json");
        let source = payload["source"].as_str().expect("source");

        assert!(source.contains("fallback_order:"));
        assert!(!source.contains("specificity:"));
        assert!(!source.contains("match:"));
        assert!(!source.contains("null"));
        assert!(!source.contains("priority: 0"));
    }

    #[tokio::test]
    async fn noether_app_discard_policy_proposal_removes_saved_draft() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(
            tempdir.path().join("fixtures"),
            Some(model_locked_policy()),
            DecisionMode::DryRun,
        );
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        std::fs::write(
            &state.policy_proposal_path,
            serde_yaml::to_string(&strict_policy()).expect("strict policy yaml"),
        )
        .expect("write proposal");

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/app/policy/proposal")
                    .body(Body::empty())
                    .expect("discard request"),
            )
            .await
            .expect("discard response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("discard body");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("discard json");
        assert!(payload["proposal"].is_null());
        assert!(!tempdir.path().join("policy.proposed.yaml").exists());
    }

    #[tokio::test]
    async fn noether_app_enforce_requires_confirmation_and_writes_rollback_snapshot() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let policy_path = tempdir.path().join("policy.yaml");
        let active_source = serde_yaml::to_string(&model_locked_policy()).expect("active yaml");
        std::fs::write(&policy_path, &active_source).expect("write active policy");
        let policy = crate::policy::load_policy(&policy_path)
            .await
            .expect("load active policy");
        let mut state = AppState::with_reloadable_policy(
            tempdir.path().join("fixtures"),
            None,
            policy_path.clone(),
            policy,
            DecisionMode::DryRun,
        );
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        std::fs::write(
            &state.policy_proposal_path,
            serde_yaml::to_string(&strict_policy()).expect("proposal yaml"),
        )
        .expect("write proposal");
        let app = build_router(state);

        let rejected = app
            .clone()
            .oneshot(
                Request::post("/v1/app/policy/enforce")
                    .body(Body::empty())
                    .expect("unconfirmed enforce"),
            )
            .await
            .expect("unconfirmed response");
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

        let enforced = app
            .clone()
            .oneshot(
                Request::post("/v1/app/policy/enforce")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"confirm_replay":true}).to_string()))
                    .expect("confirmed enforce"),
            )
            .await
            .expect("confirmed response");
        assert_eq!(enforced.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read_to_string(tempdir.path().join("policy.previous.yaml"))
                .expect("previous policy"),
            active_source
        );
        assert!(tempdir.path().join("policy.audit.jsonl").exists());

        let rolled_back = app
            .oneshot(
                Request::post("/v1/app/policy/rollback")
                    .body(Body::empty())
                    .expect("rollback request"),
            )
            .await
            .expect("rollback response");
        assert_eq!(rolled_back.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read_to_string(policy_path).expect("rolled back policy"),
            active_source
        );
    }

    #[tokio::test]
    async fn noether_app_replay_reevaluates_historical_requests_against_draft_policy() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(tempdir.path().join("fixtures"), None, DecisionMode::DryRun);
        state.ledger = Arc::new(Mutex::new(
            BudgetLedger::open_sqlite(&tempdir.path().join("noether.sqlite"))
                .expect("sqlite ledger"),
        ));
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        std::fs::write(
            &state.policy_proposal_path,
            serde_yaml::to_string(&strict_policy()).expect("strict policy yaml"),
        )
        .expect("write proposal");

        {
            let mut ledger = state.ledger.lock().await;
            let mut request = report_request("trace-replay", "req-replay", "gpt-4.1", 1.25);
            request
                .metadata
                .insert("agent_run_id".to_owned(), json!("run-replay"));
            let decision = ledger.try_authorize(None, &request).expect("authorize");
            let reservation = decision.reservation.expect("reservation");
            ledger
                .finalize(
                    &reservation.id,
                    &finalize_payload("trace-replay", "gpt-4.1", 1.25, 1_000, 250),
                )
                .expect("finalize");
        }

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/app/replay")
                    .body(Body::empty())
                    .expect("replay request"),
            )
            .await
            .expect("replay response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("replay body");
        let replay: serde_json::Value =
            serde_json::from_slice(&body).expect("replay response json");

        assert_eq!(replay["baseline"]["allow"], 1);
        assert_eq!(replay["proposal"]["proposed"]["deny"], 1);
        assert_eq!(
            replay["proposal"]["changed_runs"][0]["run_id"],
            "run-replay"
        );
        assert_eq!(replay["proposal"]["changed_runs"][0]["from"], "allow");
        assert_eq!(replay["proposal"]["changed_runs"][0]["to"], "deny");
        assert_eq!(
            replay["proposal"]["recommendations"][0]["action"],
            "review_changed_runs"
        );
        assert_eq!(replay["proposal"]["mode"], "draft_impact");
        assert_eq!(replay["proposal"]["can_enforce"], true);
        assert_eq!(replay["proposal"]["spend_delta_usd"], -1.25);
    }

    #[test]
    fn noether_app_replay_caps_changed_run_examples() {
        let proposal_source =
            serde_yaml::to_string(&require_project_policy()).expect("policy yaml");
        let proposal = AppPolicyProposal {
            path: "policy.proposed.yaml".to_owned(),
            source: proposal_source,
        };
        let historical_requests = (0..150)
            .map(|index| {
                let mut request = report_request(
                    &format!("trace-cap-{index}"),
                    &format!("request-cap-{index}"),
                    "gpt-4.1",
                    0.01,
                );
                request.project = None;
                request.entities = Vec::new();
                crate::ledger::HistoricalAuthorizeRequest {
                    occurred_at: chrono::Utc::now(),
                    decision_id: format!("decision-cap-{index}"),
                    baseline_outcome: DecisionOutcome::Allow,
                    request,
                }
            })
            .collect::<Vec<_>>();

        let replay = app_replay_proposal(
            "",
            &proposal,
            &historical_requests,
            &std::collections::BTreeMap::new(),
            &[],
            ReplayScopeOptions {
                mode: "preview".to_owned(),
                request_cap: Some(APP_REPLAY_PREVIEW_REQUEST_CAP),
                total_requests_in_window: 150,
                full_replay_available: true,
                changed_runs_cap: APP_REPLAY_CHANGED_RUNS_CAP,
                window_seeded: false,
            },
        )
        .expect("replay proposal");

        assert_eq!(replay.changed_runs.len(), APP_REPLAY_CHANGED_RUNS_CAP);
        assert_eq!(replay.scope.changed_runs_total, 150);
        assert_eq!(
            replay.scope.changed_runs_returned,
            APP_REPLAY_CHANGED_RUNS_CAP
        );
        assert_eq!(replay.scope.mode, "preview");
        assert!(replay.scope.full_replay_available);
    }

    #[tokio::test]
    async fn noether_app_full_replay_job_completes_full_month_replay() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(tempdir.path().join("fixtures"), None, DecisionMode::DryRun);
        state.ledger = Arc::new(Mutex::new(
            BudgetLedger::open_sqlite(&tempdir.path().join("noether.sqlite"))
                .expect("sqlite ledger"),
        ));
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        std::fs::write(
            &state.policy_proposal_path,
            serde_yaml::to_string(&require_project_policy()).expect("policy yaml"),
        )
        .expect("write proposal");
        {
            let mut ledger = state.ledger.lock().await;
            let mut request = report_request("trace-job", "request-job", "gpt-4.1", 0.01);
            request.project = None;
            request.entities = Vec::new();
            ledger.try_authorize(None, &request).expect("authorize");
        }

        let app = build_router(state);
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/app/replay/jobs")
                    .body(Body::empty())
                    .expect("job request"),
            )
            .await
            .expect("job response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("job body");
        let started: serde_json::Value = serde_json::from_slice(&body).expect("job json");
        let job_id = started["id"].as_str().expect("job id").to_owned();

        let mut completed = None;
        for _ in 0..20 {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/v1/app/replay/jobs/{job_id}"))
                        .body(Body::empty())
                        .expect("job status request"),
                )
                .await
                .expect("job status response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("job status body");
            let status: serde_json::Value = serde_json::from_slice(&body).expect("job status json");
            if status["status"] == "completed" {
                completed = Some(status);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let completed = completed.expect("job completed");
        assert_eq!(completed["result"]["history_window_days"], 30);
        assert_eq!(
            completed["result"]["proposal"]["scope"]["mode"],
            "full_month"
        );
        assert_eq!(
            completed["result"]["proposal"]["scope"]["request_cap"],
            serde_json::Value::Null
        );
        assert_eq!(
            completed["result"]["proposal"]["changed_runs"][0]["to"],
            "deny"
        );
    }

    #[tokio::test]
    async fn noether_app_replay_keeps_unchanged_history_empty() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(tempdir.path().join("fixtures"), None, DecisionMode::DryRun);
        state.ledger = Arc::new(Mutex::new(
            BudgetLedger::open_sqlite(&tempdir.path().join("noether.sqlite"))
                .expect("sqlite ledger"),
        ));
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        std::fs::write(
            &state.policy_proposal_path,
            serde_yaml::to_string(&PolicyFile {
                version: 0,
                routing: Default::default(),
                budgets: Vec::new(),
                policies: Vec::new(),
            })
            .expect("policy yaml"),
        )
        .expect("write proposal");

        {
            let mut ledger = state.ledger.lock().await;
            let mut request = report_request("trace-same", "req-same", "gpt-4.1", 0.5);
            request
                .metadata
                .insert("agent_run_id".to_owned(), json!("run-same"));
            ledger.try_authorize(None, &request).expect("authorize");
        }

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/app/replay")
                    .body(Body::empty())
                    .expect("replay request"),
            )
            .await
            .expect("replay response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("replay body");
        let replay: serde_json::Value =
            serde_json::from_slice(&body).expect("replay response json");

        assert_eq!(replay["proposal"]["proposed"]["allow"], 1);
        assert_eq!(
            replay["proposal"]["changed_runs"].as_array().unwrap().len(),
            0
        );
        assert_eq!(replay["proposal"]["mode"], "draft_impact");
        assert_eq!(replay["proposal"]["can_enforce"], true);
        assert_eq!(
            replay["proposal"]["recommendations"][0]["action"],
            "review_policy_diff"
        );
        assert_eq!(replay["proposal"]["spend_delta_usd"], 0.0);
    }

    #[test]
    fn noether_app_replay_marks_identical_draft_as_backtest() {
        let source = serde_yaml::to_string(&PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: Vec::new(),
            policies: Vec::new(),
        })
        .expect("policy yaml");
        let proposal = AppPolicyProposal {
            path: "policy.proposed.yaml".to_owned(),
            source: source.clone(),
        };

        let replay = app_replay_proposal(
            &source,
            &proposal,
            &[],
            &std::collections::BTreeMap::new(),
            &[],
            ReplayScopeOptions {
                mode: "preview".to_owned(),
                request_cap: Some(APP_REPLAY_PREVIEW_REQUEST_CAP),
                total_requests_in_window: 0,
                full_replay_available: false,
                changed_runs_cap: APP_REPLAY_CHANGED_RUNS_CAP,
                window_seeded: false,
            },
        )
        .expect("replay proposal");

        assert_eq!(replay.mode, "current_policy_backtest");
        assert!(!replay.can_enforce);
        assert_eq!(replay.changed_lines, 0);
    }

    #[tokio::test]
    async fn noether_app_replay_reports_static_history_window() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(tempdir.path().join("fixtures"), None, DecisionMode::DryRun);
        state.ledger = Arc::new(Mutex::new(
            BudgetLedger::open_sqlite(&tempdir.path().join("noether.sqlite"))
                .expect("sqlite ledger"),
        ));

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/app/replay")
                    .body(Body::empty())
                    .expect("replay request"),
            )
            .await
            .expect("replay response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("replay body");
        let replay: serde_json::Value =
            serde_json::from_slice(&body).expect("replay response json");

        assert_eq!(replay["history_window_days"], 30);
        assert!(replay["history_window_start"].as_str().is_some());
        assert!(replay["history_window_end"].as_str().is_some());
    }

    #[test]
    fn noether_app_replay_evaluates_spend_windows_at_historical_time() {
        let policy = strict_policy();
        let first_at = chrono::Utc::now() - chrono::Duration::minutes(5);
        let second_at = first_at + chrono::Duration::minutes(2);
        let historical_requests = vec![
            crate::ledger::HistoricalAuthorizeRequest {
                occurred_at: first_at,
                decision_id: "decision-1".to_owned(),
                baseline_outcome: DecisionOutcome::Allow,
                request: report_request("trace-1", "request-1", "gpt-4.1", 0.007),
            },
            crate::ledger::HistoricalAuthorizeRequest {
                occurred_at: second_at,
                decision_id: "decision-2".to_owned(),
                baseline_outcome: DecisionOutcome::Allow,
                request: report_request("trace-2", "request-2", "gpt-4.1", 0.007),
            },
        ];

        let (totals, changed_runs, spend_delta, _) = replay_historical_requests(
            &policy,
            &historical_requests,
            &std::collections::BTreeMap::new(),
            &[],
        )
        .expect("replay");

        assert_eq!(totals.allow, 2);
        assert_eq!(totals.deny, 0);
        assert!(changed_runs.is_empty());
        assert_eq!(spend_delta, 0.0);
    }

    #[test]
    fn noether_app_replay_preview_seeds_prior_window_spend() {
        let policy = strict_policy();
        let occurred_at = chrono::Utc::now();
        let historical_requests = vec![crate::ledger::HistoricalAuthorizeRequest {
            occurred_at,
            decision_id: "decision-seeded".to_owned(),
            baseline_outcome: DecisionOutcome::Allow,
            request: report_request("trace-seeded", "request-seeded", "gpt-4.1", 0.007),
        }];
        let seed = ReplaySpendSeed {
            rule_id: "tiny".to_owned(),
            limit_id: "budget-cap".to_owned(),
            scope_key: "global".to_owned(),
            amount_usd: 0.007,
            mode: SpendWindowMode::Tumbling,
            seeded_at: occurred_at - chrono::Duration::seconds(1),
            window_started_at: occurred_at - chrono::Duration::seconds(10),
        };

        let (totals, changed_runs, _, _) = replay_historical_requests(
            &policy,
            &historical_requests,
            &std::collections::BTreeMap::new(),
            &[seed],
        )
        .expect("replay");

        assert_eq!(totals.deny, 1);
        assert_eq!(changed_runs[0].to_decision, "deny");
        assert_eq!(
            changed_runs[0].rule.as_deref(),
            Some("tiny.spend_window.budget-cap")
        );
    }

    #[test]
    fn noether_app_replay_matches_live_warning_policy_semantics() {
        let policy = warn_project_policy();
        let mut live_ledger = BudgetLedger::default();
        let mut request = report_request("trace-warn", "request-warn", "gpt-4.1", 0.25);
        request.project = None;
        request.entities = Vec::new();
        request.estimated_tokens = Some(1_200);
        request.provider = Some("openai".to_owned());
        let live = live_ledger
            .try_authorize_replay_at(Some(&policy), &request, chrono::Utc::now())
            .expect("live-style replay authorize");
        let historical_requests = vec![crate::ledger::HistoricalAuthorizeRequest {
            occurred_at: live.created_at,
            decision_id: "decision-warn".to_owned(),
            baseline_outcome: DecisionOutcome::Allow,
            request,
        }];

        let (totals, changed_runs, spend_delta, _) = replay_historical_requests(
            &policy,
            &historical_requests,
            &std::collections::BTreeMap::new(),
            &[],
        )
        .expect("replay");

        assert_eq!(live.outcome, DecisionOutcome::Warn);
        assert!(live.explanations.iter().any(|explanation| {
            explanation.rule_id == "require-project"
                && explanation.severity == DecisionSeverity::Warn
        }));
        assert!(live.explanations.iter().any(|explanation| {
            explanation.rule_id == "personal-local.context_tokens"
                && explanation.severity == DecisionSeverity::Warn
        }));
        assert_eq!(totals.warn, 1);
        assert_eq!(changed_runs[0].to_decision, "warn");
        assert_eq!(changed_runs[0].rule.as_deref(), Some("require-project"));
        assert_eq!(spend_delta, 0.0);
    }

    #[tokio::test]
    async fn noether_app_run_detail_returns_agent_run_for_changed_run_links() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(tempdir.path().join("fixtures"), None, DecisionMode::DryRun);
        state.ledger = Arc::new(Mutex::new(
            BudgetLedger::open_sqlite(&tempdir.path().join("noether.sqlite"))
                .expect("sqlite ledger"),
        ));
        {
            let mut ledger = state.ledger.lock().await;
            let mut request = report_request("trace-detail", "req-detail", "gpt-4.1", 0.75);
            request
                .metadata
                .insert("agent_run_id".to_owned(), json!("run-detail"));
            ledger.try_authorize(None, &request).expect("authorize");
            ledger
                .record_event(TraceEvent {
                    id: Some("evt-detail-tool".to_owned()),
                    trace_id: Some("trace-detail".to_owned()),
                    occurred_at: None,
                    kind: "tool.observed".to_owned(),
                    payload: json!({
                        "agent_run_id": "run-detail",
                        "name": "bash",
                        "success": true
                    }),
                })
                .expect("record event");
        }

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/app/runs/run-detail")
                    .body(Body::empty())
                    .expect("run detail request"),
            )
            .await
            .expect("run detail response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("run detail body");
        let run: serde_json::Value = serde_json::from_slice(&body).expect("run detail json");
        assert_eq!(run["id"], "run-detail");
        assert_eq!(run["agent_run_id"], "run-detail");
        assert_eq!(run["trace_id"], "trace-detail");
        assert!(
            run["timeline"]
                .as_array()
                .expect("timeline array")
                .iter()
                .any(|item| item["kind"] == "tool.observed")
        );
    }

    #[test]
    fn noether_app_policy_suggestions_include_reason_and_model_evidence() {
        let suggestions = app_policy_suggestions(&[AppRuleStat {
            rule: "personal-local".to_owned(),
            allow: 0,
            warn: 0,
            deny: 15,
            ask: 0,
            limit_hits: 0,
            top_reason: Some("provider/model is not allowed by budget".to_owned()),
            top_model: Some("openai-codex/gpt-5.5".to_owned()),
        }]);

        assert_eq!(suggestions[0].action, "open_runs_filtered_to_rule");
        assert!(suggestions[0].title.contains("personal-local blocked 15"));
        assert!(suggestions[0].body.contains("models.allow"));
        assert!(
            suggestions[0]
                .evidence
                .iter()
                .any(|line| line.contains("Reason: provider/model"))
        );
        assert!(
            suggestions[0]
                .evidence
                .iter()
                .any(|line| line.contains("Top model: openai-codex/gpt-5.5"))
        );
    }

    #[tokio::test]
    async fn noether_app_apply_blocked_model_suggestion_saves_concrete_draft() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(
            tempdir.path().join("fixtures"),
            Some(model_locked_policy()),
            DecisionMode::DryRun,
        );
        state.ledger = Arc::new(Mutex::new(
            BudgetLedger::open_sqlite(&tempdir.path().join("noether.sqlite"))
                .expect("sqlite ledger"),
        ));
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        {
            let mut ledger = state.ledger.lock().await;
            let mut request = report_request("trace-model", "req-model", "gpt-5.5", 0.5);
            request.provider = Some("openai-codex".to_owned());
            ledger
                .try_authorize(state.active_policy().await.as_deref(), &request)
                .expect("authorize denied model");
        }

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/app/policy/suggestions/personal-local-denies/apply")
                    .method("POST")
                    .body(Body::empty())
                    .expect("apply request"),
            )
            .await
            .expect("apply response");
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("apply body");
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("apply json");

        assert!(
            payload["policy"]["proposal"]["source"]
                .as_str()
                .expect("proposal source")
                .contains("openai-codex:gpt-5.5")
        );
    }

    #[tokio::test]
    async fn old_dashboard_surface_is_gone() {
        let app = build_router(test_state(None));

        for route in [
            "/dashboard",
            "/dashboard/app.js",
            "/v1/dashboard/overview?window=all&lens=project",
            "/v1/dashboard/strategy-lab?simulation=runaway-pressure",
            "/v1/reports/dashboard-data?trace=trace-beta",
            "/v1/reports/dashboard?trace=trace-beta",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(route)
                        .body(Body::empty())
                        .expect("removed dashboard request"),
                )
                .await
                .expect("removed dashboard response");
            assert_eq!(response.status(), StatusCode::GONE, "route {route}");
        }
    }

    #[tokio::test]
    async fn report_update_stream_emits_after_authorize_finalize_and_event() {
        let app = build_router(test_state(None));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/reports/updates")
                    .body(Body::empty())
                    .expect("updates request"),
            )
            .await
            .expect("updates response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let mut body = response.into_body().into_data_stream();

        let authorize_response = app
            .clone()
            .oneshot(
                Request::post("/v1/authorize")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&report_request(
                            "trace-live",
                            "req-live",
                            "gpt-4.1",
                            1.25,
                        ))
                        .expect("authorize json"),
                    ))
                    .expect("authorize request"),
            )
            .await
            .expect("authorize response");
        assert_eq!(authorize_response.status(), StatusCode::OK);
        let authorize_body = to_bytes(authorize_response.into_body(), usize::MAX)
            .await
            .expect("authorize body");
        let authorize_json: Value =
            serde_json::from_slice(&authorize_body).expect("authorize value");
        let reservation_id = authorize_json["reservation"]["id"]
            .as_str()
            .expect("reservation id")
            .to_owned();

        let authorize_chunk = tokio::time::timeout(Duration::from_millis(500), body.next())
            .await
            .expect("authorize stream event")
            .expect("authorize chunk present")
            .expect("authorize chunk ok");
        let authorize_text = String::from_utf8(authorize_chunk.to_vec()).expect("authorize text");
        assert!(authorize_text.contains("\"kind\":\"authorize\""));
        assert!(authorize_text.contains("trace-live"));

        let finalize_response = app
            .clone()
            .oneshot(
                Request::post(format!("/v1/reservations/{reservation_id}/finalize"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&finalize_payload(
                            "trace-live",
                            "gpt-4.1",
                            1.25,
                            1_000,
                            250,
                        ))
                        .expect("finalize json"),
                    ))
                    .expect("finalize request"),
            )
            .await
            .expect("finalize response");
        assert_eq!(finalize_response.status(), StatusCode::OK);

        let finalize_chunk = tokio::time::timeout(Duration::from_millis(500), body.next())
            .await
            .expect("finalize stream event")
            .expect("finalize chunk present")
            .expect("finalize chunk ok");
        let finalize_text = String::from_utf8(finalize_chunk.to_vec()).expect("finalize text");
        assert!(finalize_text.contains("\"kind\":\"finalize\""));
        assert!(finalize_text.contains("trace-live"));

        let event_response = app
            .oneshot(
                Request::post("/v1/events")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "trace_id":"trace-live",
                            "kind":"tool.observed",
                            "payload":{"name":"bash","success":true}
                        })
                        .to_string(),
                    ))
                    .expect("event request"),
            )
            .await
            .expect("event response");
        assert_eq!(event_response.status(), StatusCode::ACCEPTED);

        let event_chunk = tokio::time::timeout(Duration::from_millis(500), body.next())
            .await
            .expect("event stream event")
            .expect("event chunk present")
            .expect("event chunk ok");
        let event_text = String::from_utf8(event_chunk.to_vec()).expect("event text");
        assert!(event_text.contains("\"kind\":\"event\""));
        assert!(event_text.contains("trace-live"));
    }

    #[tokio::test]
    async fn sqlite_ledger_persists_decision_reservation_usage_and_events() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("noether.sqlite");
        let ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        let mut state = test_state(Some(strict_policy()));
        state.ledger = Arc::new(Mutex::new(ledger));
        let app = build_router(state);

        let authorize_response = app
            .clone()
            .oneshot(
                Request::post("/v1/authorize")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "project":"noether",
                            "provider":"openai",
                            "model":"gpt-test",
                            "estimated_cost_usd":0.001,
                            "metadata":{"trace_id":"trace-sqlite","request_id":"request-1"}
                        })
                        .to_string(),
                    ))
                    .expect("authorize request"),
            )
            .await
            .expect("authorize response");
        assert_eq!(authorize_response.status(), StatusCode::OK);
        let body = to_bytes(authorize_response.into_body(), usize::MAX)
            .await
            .expect("authorize body");
        let decision: Value = serde_json::from_slice(&body).expect("authorize json");
        let reservation_id = decision["reservation"]["id"]
            .as_str()
            .expect("reservation id");

        let finalize_response = app
            .clone()
            .oneshot(
                Request::post(format!("/v1/reservations/{reservation_id}/finalize"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "actual_cost_usd":0.0008,
                            "usage":{
                                "provider":"openai",
                                "model":"gpt-test",
                                "input_tokens":10,
                                "output_tokens":20,
                                "total_tokens":30,
                                "cost_usd":0.0008,
                                "stop_reason":"stop"
                            }
                        })
                        .to_string(),
                    ))
                    .expect("finalize request"),
            )
            .await
            .expect("finalize response");
        assert_eq!(finalize_response.status(), StatusCode::OK);

        let event_response = app
            .oneshot(
                Request::post("/v1/events")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "trace_id":"trace-sqlite",
                            "kind":"tool.observed",
                            "payload":{"source":"test","name":"shell","duration_ms":5,"success":true}
                        })
                        .to_string(),
                    ))
                    .expect("event request"),
            )
            .await
            .expect("event response");
        assert_eq!(event_response.status(), StatusCode::ACCEPTED);

        let report = BudgetLedger::open_sqlite(&db_path)
            .expect("reopen sqlite")
            .usage_report()
            .expect("usage report");
        assert_eq!(report.total_cost_usd, 0.0008);
        assert_eq!(report.rows[0].project.as_deref(), Some("noether"));
        assert_eq!(report.rows[0].total_tokens, 30);

        let trace = BudgetLedger::open_sqlite(&db_path)
            .expect("reopen sqlite")
            .trace_report("trace-sqlite")
            .expect("trace report");
        assert!(trace.items.iter().any(|item| item.kind == "decision.allow"));
        assert!(
            trace
                .items
                .iter()
                .any(|item| item.kind == "usage.finalized")
        );
        assert!(trace.items.iter().any(|item| item.kind == "tool.observed"));
    }

    #[tokio::test]
    async fn capture_dry_run_records_deny_decision_without_blocking() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture_dir = tempdir.path().join("fixtures");
        let app = build_router(state_with_dir(
            fixture_dir.clone(),
            Some(require_project_policy()),
            DecisionMode::DryRun,
        ));

        let response = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"model":"noether-test"}).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let paths = list_fixture_paths(&fixture_dir)
            .await
            .expect("fixture paths");
        let fixture = read_fixture(&paths[0]).await.expect("fixture");
        let decision = fixture.decision.expect("decision metadata");
        assert_eq!(fixture.response.source, ResponseSource::Mock);
        assert_eq!(decision.mode, DecisionMode::DryRun);
        assert_eq!(
            decision.decision.outcome,
            crate::contract::DecisionOutcome::Deny
        );
    }

    #[tokio::test]
    async fn capture_enforce_blocks_deny_decision_before_mock_or_upstream() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture_dir = tempdir.path().join("fixtures");
        let app = build_router(state_with_dir(
            fixture_dir.clone(),
            Some(require_project_policy()),
            DecisionMode::Enforce,
        ));

        let response = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"model":"noether-test"}).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let paths = list_fixture_paths(&fixture_dir)
            .await
            .expect("fixture paths");
        let fixture = read_fixture(&paths[0]).await.expect("fixture");
        assert_eq!(fixture.response.source, ResponseSource::DecisionDenied);
    }

    #[tokio::test]
    async fn capture_uses_selected_ledger_backend_for_policy_decisions() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture_dir = tempdir.path().join("fixtures");
        let upstream = start_upstream(
            StatusCode::OK,
            vec![("content-type", "application/json")],
            br#"{"ok":true}"#,
        )
        .await;
        let mut state = AppState::new(
            fixture_dir,
            Some(upstream.base_url.clone()),
            Some(model_locked_policy()),
            DecisionMode::Enforce,
        );
        let db_path = tempdir.path().join("capture.sqlite");
        state.ledger = Arc::new(TokioMutex::new(
            BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger"),
        ));
        state.ledger_backend = LedgerBackend::SQLite {
            path: db_path.clone(),
        };
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "other-provider:other-model",
                            "project": "noether"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let decisions = BudgetLedger::open_sqlite(&db_path)
            .expect("reopen sqlite")
            .decisions_report()
            .expect("decisions");
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].kind, "decision.deny");
    }

    #[tokio::test]
    #[ignore = "requires NOET_TEST_POSTGRES_URL and an isolated PostgreSQL database"]
    async fn postgres_backend_persists_auth_finalize_event_and_reports() {
        let database_url = std::env::var("NOET_TEST_POSTGRES_URL").expect("NOET_TEST_POSTGRES_URL");
        let schema = format!("noether_server_test_{}", uuid::Uuid::new_v4().simple());
        let (admin, admin_connection) =
            tokio_postgres::connect(&database_url, tokio_postgres::NoTls)
                .await
                .expect("postgres admin connection");
        tokio::spawn(async move {
            let _ = admin_connection.await;
        });
        admin
            .batch_execute(&format!(
                r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE; CREATE SCHEMA "{schema}";"#
            ))
            .await
            .expect("create test schema");
        let separator = if database_url.contains('?') { '&' } else { '?' };
        let scoped_url = format!("{database_url}{separator}options=-csearch_path%3D{schema}");

        let postgres_ledger = AsyncPostgresLedger::connect(&scoped_url)
            .await
            .expect("postgres backend");
        let mut state = AppState::new(
            PathBuf::from(".noet/test-fixtures"),
            None,
            Some(model_locked_policy()),
            DecisionMode::Enforce,
        );
        state.ledger_backend = LedgerBackend::Postgres {
            database_url: scoped_url,
            ledger: postgres_ledger,
        };
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "trace_id".to_owned(),
            Value::String("trace-postgres-backend".to_owned()),
        );
        metadata.insert(
            "request_id".to_owned(),
            Value::String("request-postgres-backend".to_owned()),
        );
        let request = AuthorizeRequest {
            budget_id: None,
            entities: vec!["project:noether".to_owned()],
            subject: Some("user:test".to_owned()),
            project: Some("noether".to_owned()),
            provider: Some("openai-codex".to_owned()),
            model: Some("gpt-4.1".to_owned()),
            estimated_tokens: Some(100),
            estimated_cost_usd: Some(0.25),
            metadata: metadata.clone(),
        };

        let decision = state
            .authorize_request(state.active_policy().await, request)
            .await
            .expect("authorize");
        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        let reservation_id = decision.reservation.expect("reservation").id;

        state
            .finalize_reservation(
                reservation_id,
                FinalizeReservation {
                    reservation_id: None,
                    outcome: FinalizeOutcome::Success,
                    usage: Some(UsageObservation {
                        provider: Some("openai-codex".to_owned()),
                        model: Some("gpt-4.1".to_owned()),
                        input_tokens: Some(10),
                        output_tokens: Some(5),
                        total_tokens: Some(15),
                        cost_usd: Some(0.20),
                        latency_ms: Some(123),
                        stop_reason: Some("stop".to_owned()),
                    }),
                    actual_cost_usd: Some(0.20),
                    metadata,
                },
            )
            .await
            .expect("finalize");
        state
            .record_trace_event(TraceEvent {
                id: None,
                trace_id: Some("trace-postgres-backend".to_owned()),
                occurred_at: None,
                kind: "tool.observed".to_owned(),
                payload: json!({"name": "shell", "success": true}),
            })
            .await
            .expect("record event");

        assert_eq!(state.ledger_backend_name(), "postgres");
        let usage = state
            .read_ledger(|ledger| ledger.usage_report())
            .await
            .expect("usage report");
        assert_eq!(usage.total_cost_usd, 0.20);
        let trace = state
            .read_ledger(|ledger| ledger.trace_report("trace-postgres-backend"))
            .await
            .expect("trace report");
        assert!(trace.items.iter().any(|item| item.kind == "decision.allow"));
        assert!(
            trace
                .items
                .iter()
                .any(|item| item.kind == "usage.finalized")
        );
        assert!(trace.items.iter().any(|item| item.kind == "tool.observed"));

        admin
            .batch_execute(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE;"#))
            .await
            .expect("drop test schema");
    }

    #[tokio::test]
    #[ignore = "requires NOET_TEST_POSTGRES_URL and an isolated PostgreSQL database"]
    async fn postgres_backend_async_finalize_persists_after_response() {
        let database_url = std::env::var("NOET_TEST_POSTGRES_URL").expect("NOET_TEST_POSTGRES_URL");
        let schema = format!("noether_server_test_{}", uuid::Uuid::new_v4().simple());
        let (admin, admin_connection) =
            tokio_postgres::connect(&database_url, tokio_postgres::NoTls)
                .await
                .expect("postgres admin connection");
        tokio::spawn(async move {
            let _ = admin_connection.await;
        });
        admin
            .batch_execute(&format!(
                r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE; CREATE SCHEMA "{schema}";"#
            ))
            .await
            .expect("create test schema");
        let separator = if database_url.contains('?') { '&' } else { '?' };
        let scoped_url = format!("{database_url}{separator}options=-csearch_path%3D{schema}");
        let postgres_ledger = AsyncPostgresLedger::connect_with_options(
            &scoped_url,
            AsyncPostgresLedgerOptions {
                async_finalize: true,
                ..AsyncPostgresLedgerOptions::default()
            },
        )
        .await
        .expect("postgres backend");
        let mut state = AppState::new(
            PathBuf::from(".noet/test-fixtures"),
            None,
            Some(model_locked_policy()),
            DecisionMode::Enforce,
        );
        state.ledger_backend = LedgerBackend::Postgres {
            database_url: scoped_url,
            ledger: postgres_ledger,
        };
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "trace_id".to_owned(),
            Value::String("trace-postgres-async-finalize".to_owned()),
        );
        let decision = state
            .authorize_request(
                state.active_policy().await,
                AuthorizeRequest {
                    budget_id: None,
                    entities: vec!["project:noether".to_owned()],
                    subject: Some("user:test".to_owned()),
                    project: Some("noether".to_owned()),
                    provider: Some("openai-codex".to_owned()),
                    model: Some("gpt-4.1".to_owned()),
                    estimated_tokens: Some(100),
                    estimated_cost_usd: Some(0.25),
                    metadata: metadata.clone(),
                },
            )
            .await
            .expect("authorize");
        state
            .finalize_reservation(
                decision.reservation.expect("reservation").id,
                FinalizeReservation {
                    reservation_id: None,
                    outcome: FinalizeOutcome::Success,
                    usage: Some(UsageObservation {
                        provider: Some("openai-codex".to_owned()),
                        model: Some("gpt-4.1".to_owned()),
                        input_tokens: Some(10),
                        output_tokens: Some(5),
                        total_tokens: Some(15),
                        cost_usd: Some(0.20),
                        latency_ms: Some(123),
                        stop_reason: Some("stop".to_owned()),
                    }),
                    actual_cost_usd: Some(0.20),
                    metadata,
                },
            )
            .await
            .expect("finalize");

        let mut total_cost = 0.0;
        for _ in 0..50 {
            total_cost = state
                .read_ledger(|ledger| ledger.usage_report())
                .await
                .expect("usage report")
                .total_cost_usd;
            if total_cost == 0.20 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(total_cost, 0.20);

        admin
            .batch_execute(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE;"#))
            .await
            .expect("drop test schema");
    }

    #[tokio::test]
    async fn reloadable_policy_applies_updated_policy_on_next_request() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let policy_path = tempdir.path().join("policy.yaml");
        std::fs::write(
            &policy_path,
            r#"
version: 0
routing:
  mode: explicit_then_fallback
  fallback_order: [project, user, team, group, org, global]
budgets:
  - id: personal-local
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
policies: []
"#,
        )
        .expect("write initial policy");
        let policy = crate::policy::load_policy(&policy_path)
            .await
            .expect("load initial policy");
        let state = AppState::with_reloadable_policy(
            PathBuf::from(".noether/test-fixtures"),
            None,
            policy_path.clone(),
            policy,
            DecisionMode::Enforce,
        );

        let allowed_policy = state.active_policy().await;
        let allowed = state
            .ledger
            .lock()
            .await
            .try_authorize(
                allowed_policy.as_deref(),
                &report_request("trace-reload-allow", "req-allow", "gpt-4.1", 0.01),
            )
            .expect("authorize before reload");
        assert_eq!(allowed.outcome, crate::contract::DecisionOutcome::Allow);

        std::fs::write(
            &policy_path,
            r#"
version: 0
routing:
  mode: explicit_then_fallback
  fallback_order: [project, user, team, group, org, global]
budgets:
  - id: personal-local
    match:
      project: noether
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 10
          action: block
      context_tokens:
        max_tokens: 1
        action: block
policies: []
"#,
        )
        .expect("write updated policy");

        let denied_policy = state.active_policy().await;
        let denied = state
            .ledger
            .lock()
            .await
            .try_authorize(
                denied_policy.as_deref(),
                &report_request("trace-reload-deny", "req-deny", "gpt-4.1", 0.01),
            )
            .expect("authorize after reload");
        assert_eq!(denied.outcome, crate::contract::DecisionOutcome::Deny);
    }

    #[tokio::test]
    async fn reloadable_policy_keeps_last_good_policy_after_invalid_edit() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let policy_path = tempdir.path().join("policy.yaml");
        std::fs::write(
            &policy_path,
            r#"
version: 0
routing:
  mode: explicit_then_fallback
  fallback_order: [project, user, team, group, org, global]
budgets:
  - id: personal-local
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
policies: []
"#,
        )
        .expect("write initial policy");
        let policy = crate::policy::load_policy(&policy_path)
            .await
            .expect("load initial policy");
        let state = AppState::with_reloadable_policy(
            PathBuf::from(".noether/test-fixtures"),
            None,
            policy_path.clone(),
            policy,
            DecisionMode::Enforce,
        );

        std::fs::write(&policy_path, "version: nope\n").expect("write invalid policy");

        let preserved_policy = state.active_policy().await;
        let decision = state
            .ledger
            .lock()
            .await
            .try_authorize(
                preserved_policy.as_deref(),
                &report_request("trace-reload-stale", "req-stale", "gpt-4.1", 0.01),
            )
            .expect("authorize with preserved policy");
        assert_eq!(decision.outcome, crate::contract::DecisionOutcome::Allow);
    }
}
