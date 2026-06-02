use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Extension, Path as AxumPath, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderValue, Request, StatusCode};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use uuid::Uuid;

mod app_runs;
mod policy_routes;
mod replay;
mod reports;
mod simulations;
mod static_routes;
use crate::capture::capture;
use crate::config::NoetherConfig;
use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, DecisionMode, DecisionOutcome, FinalizeReservation,
    Reservation, TraceEvent,
};
use crate::error::NoetError;
use crate::ledger::{AsyncPostgresLedger, AsyncPostgresLedgerOptions, BudgetLedger};
pub use crate::ledger_backend::LedgerBackend;
use crate::policy::PolicyFile;
use crate::policy_workbench::AppPolicyResponse;
use crate::proxy::ProxyRoute;
use crate::replay_workbench::AppReplayJob;
use crate::request_identity::{
    NOETHER_API_KEY_HEADER, RequestContext, add_request_context_metadata,
    add_request_context_to_event, apply_request_context_to_authorize_request,
    insert_request_id_header, is_noether_bearer_authorization, normalize_actor_header,
    normalize_api_key, request_context_from_headers, request_has_noether_api_key,
    request_id_from_headers,
};
use app_runs::{app_run_detail, app_runs};
use policy_routes::{
    app_policy, apply_app_policy_suggestion, discard_app_policy_proposal,
    enforce_app_policy_proposal, rollback_app_policy, update_app_policy_proposal,
};
use replay::{app_replay, app_replay_job, start_app_replay_job};
use reports::{
    report_approval_audit, report_decisions, report_observations, report_trace, report_usage,
};
use simulations::{
    list_simulations, simulation_dashboard_html, simulation_report,
    simulation_strategy_dashboard_html, simulation_strategy_decisions, simulation_strategy_usage,
    simulations_index_html,
};
use static_routes::{
    api_docs, deprecated_dashboard_surface, noether_app_css, noether_app_favicon, noether_app_html,
    noether_app_js, noether_app_logo, openapi_json, report_updates_stream,
};

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
    api_key: Option<Arc<str>>,
    actor_header: Option<Arc<str>>,
    pub ledger: Arc<Mutex<BudgetLedger>>,
    pub ledger_backend: LedgerBackend,
    pub report_updates: broadcast::Sender<ReportUpdate>,
    replay_jobs: Arc<Mutex<BTreeMap<String, AppReplayJob>>>,
    replay_snapshots_path: PathBuf,
    metrics: AppMetrics,
}

#[derive(Clone, Debug, Default)]
struct AppMetrics {
    requests_total: Arc<AtomicU64>,
    unauthorized_total: Arc<AtomicU64>,
    responses_4xx_total: Arc<AtomicU64>,
    responses_5xx_total: Arc<AtomicU64>,
    decisions_allow_total: Arc<AtomicU64>,
    decisions_warn_total: Arc<AtomicU64>,
    decisions_deny_total: Arc<AtomicU64>,
    errors_total: Arc<AtomicU64>,
}

#[allow(dead_code)]
fn assert_app_state_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AppState>();
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    decision_mode: DecisionMode,
    policy_loaded: bool,
    upstream_configured: bool,
    route_count: usize,
    ledger_backend: &'static str,
    auth_configured: bool,
    request_body_limit_bytes: usize,
    replay_jobs: usize,
    replay_job_capacity: usize,
    postgres_async_finalize_failures: Option<u64>,
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

const APP_REPLAY_HISTORY_WINDOW_DAYS: i64 = 30;
const APP_REPLAY_CHANGED_RUNS_CAP: usize = 100;
const APP_REPLAY_MAX_JOBS: usize = 8;
const APP_REPLAY_JOB_RETENTION_MINUTES: i64 = 30;
const DEFAULT_APP_REQUEST_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const UPSTREAM_CONNECT_TIMEOUT_SECONDS: u64 = 10;

fn app_request_body_limit_bytes() -> usize {
    parse_app_request_body_limit_bytes(
        std::env::var("NOET_REQUEST_BODY_LIMIT_BYTES")
            .ok()
            .as_deref(),
    )
}

fn parse_app_request_body_limit_bytes(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_APP_REQUEST_BODY_LIMIT_BYTES)
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
    ParseError(String),
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
                    PolicySourceSnapshot::ReadError(_) | PolicySourceSnapshot::ParseError(_) => {
                        serde_yaml::to_string(policy.as_ref()).ok()?
                    }
                };
                Some((Some(reloadable.source_path.clone()), source, policy))
            }
        }
    }

    async fn reload_error(&self) -> Option<String> {
        let state = self.state.lock().await;
        match &*state {
            PolicyRuntimeState::Static(_) => None,
            PolicyRuntimeState::Reloadable(reloadable) => match &reloadable.last_observed_source {
                PolicySourceSnapshot::ReadError(error)
                | PolicySourceSnapshot::ParseError(error) => Some(error.clone()),
                PolicySourceSnapshot::Bytes(_) => None,
            },
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
                        self.last_observed_source =
                            PolicySourceSnapshot::ParseError(error.to_string());
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
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(UPSTREAM_CONNECT_TIMEOUT_SECONDS))
                .build()
                .expect("valid upstream HTTP client"),
            policy,
            decision_mode,
            api_key: None,
            actor_header: None,
            ledger: Arc::new(Mutex::new(BudgetLedger::default())),
            ledger_backend: LedgerBackend::in_memory(),
            report_updates,
            replay_jobs: Arc::new(Mutex::new(BTreeMap::new())),
            replay_snapshots_path: PathBuf::from(".noet/replay-snapshots.json"),
            metrics: AppMetrics::default(),
        }
    }

    #[cfg(test)]
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
        self.ledger_backend.postgres_async_finalize_failures()
    }

    pub fn record_decision_metric(&self, decision: &AuthorizeDecision) {
        record_decision_metrics(&self.metrics, decision);
    }

    pub(crate) fn should_strip_proxy_request_header(
        &self,
        name: &axum::http::HeaderName,
        value: &HeaderValue,
    ) -> bool {
        if name.as_str().eq_ignore_ascii_case(NOETHER_API_KEY_HEADER) {
            return true;
        }
        if self
            .actor_header
            .as_deref()
            .is_some_and(|actor_header| name.as_str().eq_ignore_ascii_case(actor_header))
        {
            return true;
        }
        if name == AUTHORIZATION
            && let Some(api_key) = self.api_key.as_deref()
            && is_noether_bearer_authorization(value, api_key)
        {
            return true;
        }
        false
    }

    pub async fn authorize_request(
        &self,
        policy: Option<Arc<PolicyFile>>,
        request: AuthorizeRequest,
    ) -> Result<AuthorizeDecision, NoetError> {
        self.ledger_backend
            .authorize_request(Arc::clone(&self.ledger), policy, request)
            .await
    }

    pub async fn finalize_reservation(
        &self,
        reservation_id: String,
        payload: FinalizeReservation,
    ) -> Result<Reservation, NoetError> {
        self.ledger_backend
            .finalize_reservation(Arc::clone(&self.ledger), reservation_id, payload)
            .await
    }

    pub async fn record_trace_event(&self, event: TraceEvent) -> Result<(), NoetError> {
        self.ledger_backend
            .record_trace_event(Arc::clone(&self.ledger), event)
            .await
    }

    async fn active_policy_source(&self) -> Option<(Option<PathBuf>, String, Arc<PolicyFile>)> {
        self.policy.source().await
    }

    async fn policy_reload_error(&self) -> Option<String> {
        self.policy.reload_error().await
    }

    async fn update_policy_source(
        &self,
        source: String,
    ) -> Result<(Option<PathBuf>, PolicyFile), NoetError> {
        self.policy.update_source(source).await
    }

    async fn read_ledger<T: Send + 'static>(
        &self,
        read: impl FnOnce(&BudgetLedger) -> Result<T, NoetError> + Send + 'static,
    ) -> Result<T, NoetError> {
        self.ledger_backend
            .read_ledger(Arc::clone(&self.ledger), read)
            .await
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
    pub noether_config: NoetherConfig,
    pub decision_mode: DecisionMode,
    pub api_key: Option<String>,
    pub actor_header: Option<String>,
    pub on_bound: Option<Box<dyn FnOnce() -> Result<(), NoetError> + Send>>,
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
    if let Some(postgres_ledger) = postgres_ledger.as_ref() {
        postgres_ledger
            .set_advisory_config(config.noether_config.advisory.clone())
            .await;
    }
    let mut ledger = if config.database_url.is_some() {
        BudgetLedger::default()
    } else {
        BudgetLedger::open_sqlite(&config.db_path)?
    };
    ledger.set_advisory_config(config.noether_config.advisory.clone());
    let policy_proposal_path = config
        .simulation_dir
        .parent()
        .unwrap_or_else(|| Path::new(".noet"))
        .join("policy.proposed.yaml");
    let replay_snapshots_path = config
        .simulation_dir
        .parent()
        .unwrap_or_else(|| Path::new(".noet"))
        .join("replay-snapshots.json");
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
    state.replay_snapshots_path = replay_snapshots_path;
    state.routes = config.routes;
    state.api_key = normalize_api_key(config.api_key).map(Arc::from);
    state.actor_header = normalize_actor_header(config.actor_header).map(Arc::from);
    if state.api_key.is_none() && bind_requires_auth_warning(bind) {
        warn!(
            bind = %bind,
            "NOET_API_KEY is not set while noet is bound to a non-loopback address; rely on an external security boundary or configure bearer auth"
        );
    }
    if let Some(database_url) = config.database_url {
        if let Some(postgres_ledger) = postgres_ledger {
            state.ledger_backend = LedgerBackend::postgres(database_url, postgres_ledger);
        }
    } else {
        state.ledger_backend = LedgerBackend::sqlite(config.db_path.clone());
    }
    state.ledger = Arc::new(Mutex::new(ledger));
    let app = build_router(state);

    info!(bind = %bind, "starting noet capture server");
    let listener = TcpListener::bind(bind).await?;
    if let Some(on_bound) = config.on_bound {
        on_bound()?;
    }
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn build_router(state: AppState) -> Router {
    let request_body_limit = app_request_body_limit_bytes();
    Router::new()
        .route("/v1/authorize", post(authorize))
        .route("/v1/reservations/{id}/finalize", post(finalize_reservation))
        .route("/v1/events", post(record_event))
        .route("/v1/reports/usage", get(report_usage))
        .route("/v1/reports/decisions", get(report_decisions))
        .route("/v1/reports/traces/{trace_id}", get(report_trace))
        .route("/v1/reports/observations", get(report_observations))
        .route("/v1/reports/approval-audit", get(report_approval_audit))
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
        .route("/favicon.ico", get(noether_app_favicon))
        .route("/openapi.json", get(openapi_json))
        .route("/docs", get(api_docs))
        .route("/api/docs", get(api_docs))
        .route("/dashboard", any(deprecated_dashboard_surface))
        .route("/dashboard/{*path}", any(deprecated_dashboard_surface))
        .route("/v1/chat/completions", any(capture))
        .route("/v1/messages", any(capture))
        .route("/v1/responses", any(capture))
        .route("/health", any(health))
        .route("/metrics", get(metrics))
        .fallback(any(capture))
        .layer(from_fn_with_state(
            state.clone(),
            request_context_middleware,
        ))
        .layer(DefaultBodyLimit::max(request_body_limit))
        .layer(RequestBodyLimitLayer::new(request_body_limit))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let replay_jobs = state.replay_jobs.lock().await.len();
    Json(HealthResponse {
        status: "ok",
        decision_mode: state.decision_mode,
        policy_loaded: state.active_policy().await.is_some(),
        upstream_configured: state.upstream.is_some(),
        route_count: state.routes.len(),
        ledger_backend: state.ledger_backend_name(),
        auth_configured: state.api_key.is_some(),
        request_body_limit_bytes: app_request_body_limit_bytes(),
        replay_jobs,
        replay_job_capacity: APP_REPLAY_MAX_JOBS,
        postgres_async_finalize_failures: state.postgres_async_finalize_failures(),
    })
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let replay_jobs = state.replay_jobs.lock().await.len();
    let metrics = &state.metrics;
    let body = format!(
        concat!(
            "noet_requests_total {}\n",
            "noet_unauthorized_requests_total {}\n",
            "noet_responses_4xx_total {}\n",
            "noet_responses_5xx_total {}\n",
            "noet_decisions_allow_total {}\n",
            "noet_decisions_warn_total {}\n",
            "noet_decisions_deny_total {}\n",
            "noet_errors_total {}\n",
            "noet_replay_jobs {}\n",
            "noet_replay_job_capacity {}\n"
        ),
        metrics.requests_total.load(Ordering::Relaxed),
        metrics.unauthorized_total.load(Ordering::Relaxed),
        metrics.responses_4xx_total.load(Ordering::Relaxed),
        metrics.responses_5xx_total.load(Ordering::Relaxed),
        metrics.decisions_allow_total.load(Ordering::Relaxed),
        metrics.decisions_warn_total.load(Ordering::Relaxed),
        metrics.decisions_deny_total.load(Ordering::Relaxed),
        metrics.errors_total.load(Ordering::Relaxed),
        replay_jobs,
        APP_REPLAY_MAX_JOBS
    );
    ([(CONTENT_TYPE, "text/plain; charset=utf-8")], body)
}

async fn request_context_middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let request_id = request_id_from_headers(request.headers())
        .unwrap_or_else(|| format!("req-{}", Uuid::new_v4()));
    state.metrics.requests_total.fetch_add(1, Ordering::Relaxed);

    if let Some(api_key) = state.api_key.as_deref() {
        let authorized = request_has_noether_api_key(request.headers(), api_key);
        if !authorized {
            state
                .metrics
                .unauthorized_total
                .fetch_add(1, Ordering::Relaxed);
            state
                .metrics
                .responses_4xx_total
                .fetch_add(1, Ordering::Relaxed);
            let mut response = (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "missing or invalid Noether API key",
                    "message": format!("Send Authorization: Bearer <NOET_API_KEY> for Noether API calls, or {NOETHER_API_KEY_HEADER}: <NOET_API_KEY> when preserving a provider Authorization header on proxy traffic."),
                })),
            )
                .into_response();
            insert_request_id_header(&mut response, &request_id);
            return response;
        }
    }

    let context = match request_context_from_headers(
        request.headers(),
        &request_id,
        state.api_key.is_some(),
        state.actor_header.as_deref(),
    ) {
        Ok(context) => context,
        Err(error_response) => {
            state
                .metrics
                .unauthorized_total
                .fetch_add(1, Ordering::Relaxed);
            state
                .metrics
                .responses_4xx_total
                .fetch_add(1, Ordering::Relaxed);
            let mut response = error_response.into_response();
            insert_request_id_header(&mut response, &request_id);
            return response;
        }
    };
    request.extensions_mut().insert(context);
    let mut response = next.run(request).await;
    insert_request_id_header(&mut response, &request_id);
    let status = response.status();
    if status.is_client_error() {
        state
            .metrics
            .responses_4xx_total
            .fetch_add(1, Ordering::Relaxed);
    } else if status.is_server_error() {
        state
            .metrics
            .responses_5xx_total
            .fetch_add(1, Ordering::Relaxed);
        state.metrics.errors_total.fetch_add(1, Ordering::Relaxed);
    }
    response
}

fn bind_requires_auth_warning(bind: SocketAddr) -> bool {
    !bind.ip().is_loopback()
}

fn record_decision_metrics(metrics: &AppMetrics, decision: &AuthorizeDecision) {
    match decision.outcome {
        DecisionOutcome::Allow => metrics
            .decisions_allow_total
            .fetch_add(1, Ordering::Relaxed),
        DecisionOutcome::Warn => metrics.decisions_warn_total.fetch_add(1, Ordering::Relaxed),
        DecisionOutcome::Deny => metrics.decisions_deny_total.fetch_add(1, Ordering::Relaxed),
    };
}

async fn authorize(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Json(mut request): Json<AuthorizeRequest>,
) -> Result<Json<AuthorizeDecision>, NoetError> {
    apply_request_context_to_authorize_request(&mut request, &context);
    let policy = state.active_policy().await;
    let decision = state
        .authorize_request(policy.clone(), request.clone())
        .await?;
    record_decision_metrics(&state.metrics, &decision);
    info!(
        request_id = %context.request_id,
        actor = %context.actor,
        outcome = ?decision.outcome,
        action = ?decision.action,
        decision_id = %decision.decision_id,
        "noet decision"
    );
    publish_report_update(&state, "authorize", request_trace_id(&request));
    Ok(Json(decision))
}

async fn finalize_reservation(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    AxumPath(id): AxumPath<String>,
    Json(mut payload): Json<FinalizeReservation>,
) -> Result<Json<Reservation>, NoetError> {
    add_request_context_metadata(&mut payload.metadata, &context);
    let trace_id = finalize_trace_id(&payload);
    let reservation = state.finalize_reservation(id, payload).await?;
    publish_report_update(&state, "finalize", trace_id);
    Ok(Json(reservation))
}

async fn record_event(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Json(mut event): Json<TraceEvent>,
) -> Result<impl IntoResponse, NoetError> {
    add_request_context_to_event(&mut event, &context);
    let trace_id = event.trace_id.clone();
    state.record_trace_event(event).await?;
    publish_report_update(&state, "event", trace_id);
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true })),
    ))
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
        FinalizeReservation, PolicyAction, PolicyCondition, PolicyRule, RuleMatch, SpendWindowMode,
        TraceEvent, UsageObservation,
    };
    use crate::fixture::{CapturedBody, ResponseSource, list_fixture_paths, read_fixture};
    use crate::ledger::ReplaySpendSeed;
    use crate::policy::PolicyFile;
    use crate::policy_workbench::{AppPolicyProposal, AppRuleStat, app_policy_suggestions};
    use crate::proxy::{ProxyRoute, ProxyRoutes};
    use crate::redaction::REDACTED;
    use crate::replay_workbench::{
        AppRunUsage, ReplayScopeOptions, app_replay_proposal, replay_baseline_totals,
        replay_historical_requests,
    };
    use crate::reporting;

    use super::simulations::percent_encode_path_component;
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

    async fn wait_for_replay_job(app: &axum::Router, job_id: &str) -> serde_json::Value {
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
            if status["status"] != "running" {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("replay job did not complete");
    }

    #[test]
    fn request_body_limit_defaults_to_large_llm_payload_cap_and_accepts_env_override() {
        assert_eq!(
            parse_app_request_body_limit_bytes(None),
            DEFAULT_APP_REQUEST_BODY_LIMIT_BYTES
        );
        assert_eq!(
            parse_app_request_body_limit_bytes(Some("2097152")),
            2_097_152
        );
        assert_eq!(
            parse_app_request_body_limit_bytes(Some("0")),
            DEFAULT_APP_REQUEST_BODY_LIMIT_BYTES
        );
        assert_eq!(
            parse_app_request_body_limit_bytes(Some("not-a-number")),
            DEFAULT_APP_REQUEST_BODY_LIMIT_BYTES
        );
    }

    #[tokio::test]
    async fn api_key_auth_rejects_missing_bearer_and_preserves_request_id() {
        let mut state = test_state(None);
        state.api_key = Some(Arc::<str>::from("secret-token"));
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::get("/health")
                    .header("x-noet-request-id", "req-auth")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get("x-noet-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("req-auth")
        );
    }

    #[tokio::test]
    async fn api_key_auth_accepts_bearer_and_health_reports_auth() {
        let mut state = test_state(None);
        state.api_key = Some(Arc::<str>::from("secret-token"));
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::get("/health")
                    .header("authorization", "Bearer secret-token")
                    .header("x-noet-request-id", "req-health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-noet-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("req-health")
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let payload: Value = serde_json::from_slice(&body).expect("health json");
        assert_eq!(payload["auth_configured"], true);
        assert_eq!(payload["replay_job_capacity"], APP_REPLAY_MAX_JOBS);
    }

    #[tokio::test]
    async fn configured_actor_header_is_required_with_clear_error() {
        let mut state = test_state(None);
        state.api_key = Some(Arc::<str>::from("secret-token"));
        state.actor_header = Some(Arc::<str>::from("x-goog-authenticated-user-email"));
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::get("/health")
                    .header("authorization", "Bearer secret-token")
                    .header("x-noet-request-id", "req-missing-actor")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get("x-noet-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("req-missing-actor")
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let payload: Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(payload["error"], "missing trusted actor header");
        assert_eq!(payload["actor_header"], "x-goog-authenticated-user-email");
        assert!(
            payload["message"]
                .as_str()
                .unwrap()
                .contains("strip client-supplied")
        );
    }

    #[tokio::test]
    async fn authorize_adds_actor_and_request_id_metadata() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = test_state(None);
        state.api_key = Some(Arc::<str>::from("secret-token"));
        let db_path = tempdir.path().join("actor.sqlite");
        state.ledger = Arc::new(TokioMutex::new(
            BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger"),
        ));
        state.ledger_backend = LedgerBackend::sqlite(db_path);
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                Request::post("/v1/authorize")
                    .header("authorization", "Bearer secret-token")
                    .header("x-noet-request-id", "req-actor")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "project": "noether" }).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let historical = state
            .read_ledger(|ledger| ledger.historical_authorize_requests())
            .await
            .expect("historical requests");
        assert_eq!(historical.len(), 1);
        assert_eq!(historical[0].request.metadata["request_id"], "req-actor");
        assert_eq!(historical[0].request.metadata["actor"]["id"], "api_key");
        assert_eq!(historical[0].request.metadata["actor"]["source"], "bearer");
    }

    #[tokio::test]
    async fn trusted_actor_header_overrides_client_claimed_user_identity() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = test_state(None);
        state.api_key = Some(Arc::<str>::from("secret-token"));
        state.actor_header = Some(Arc::<str>::from("x-goog-authenticated-user-email"));
        let db_path = tempdir.path().join("iap-actor.sqlite");
        state.ledger = Arc::new(TokioMutex::new(
            BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger"),
        ));
        state.ledger_backend = LedgerBackend::sqlite(db_path);
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                Request::post("/v1/authorize")
                    .header("authorization", "Bearer secret-token")
                    .header(
                        "x-goog-authenticated-user-email",
                        "accounts.google.com:alice@example.com",
                    )
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "subject": "user:eve",
                            "entities": ["user:eve", "project:noether"]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let historical = state
            .read_ledger(|ledger| ledger.historical_authorize_requests())
            .await
            .expect("historical requests");
        assert_eq!(
            historical[0].request.subject.as_deref(),
            Some("user:alice@example.com")
        );
        assert_eq!(
            historical[0].request.entities,
            vec![
                "project:noether".to_owned(),
                "user:alice@example.com".to_owned()
            ]
        );
        assert_eq!(
            historical[0].request.metadata["actor"]["id"],
            "accounts.google.com:alice@example.com"
        );
        assert_eq!(
            historical[0].request.metadata["actor"]["source"],
            "trusted_header"
        );
        assert_eq!(
            historical[0].request.metadata["client_claimed_subject"],
            "user:eve"
        );
        assert_eq!(
            historical[0].request.metadata["client_claimed_user_entities"],
            json!(["user:eve"])
        );
    }

    #[tokio::test]
    async fn metrics_counts_requests_decisions_and_replay_capacity() {
        let app = build_router(test_state(None));

        let authorize_response = app
            .clone()
            .oneshot(
                Request::post("/v1/authorize")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "project": "noether" }).to_string()))
                    .expect("request"),
            )
            .await
            .expect("authorize response");
        assert_eq!(authorize_response.status(), StatusCode::OK);

        let metrics_response = app
            .oneshot(
                Request::get("/metrics")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("metrics response");
        assert_eq!(metrics_response.status(), StatusCode::OK);
        let body = to_bytes(metrics_response.into_body(), usize::MAX)
            .await
            .expect("metrics body");
        let metrics = std::str::from_utf8(&body).expect("metrics utf8");
        assert!(metrics.contains("noet_requests_total 2"));
        assert!(metrics.contains("noet_decisions_allow_total 1"));
        assert!(metrics.contains(&format!("noet_replay_job_capacity {APP_REPLAY_MAX_JOBS}")));
    }

    #[tokio::test]
    async fn favicon_ico_does_not_enter_capture_fallback() {
        let app = build_router(test_state(None));

        let favicon_response = app
            .clone()
            .oneshot(
                Request::get("/favicon.ico")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("favicon response");
        assert_eq!(favicon_response.status(), StatusCode::OK);

        let metrics_response = app
            .oneshot(
                Request::get("/metrics")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("metrics response");
        let body = to_bytes(metrics_response.into_body(), usize::MAX)
            .await
            .expect("metrics body");
        let metrics = std::str::from_utf8(&body).expect("metrics utf8");
        assert!(metrics.contains("noet_requests_total 2"));
        assert!(metrics.contains("noet_responses_4xx_total 0"));
        assert!(metrics.contains("noet_decisions_deny_total 0"));
    }

    #[tokio::test]
    async fn record_event_actor_is_exposed_in_approval_audit() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = test_state(None);
        state.api_key = Some(Arc::<str>::from("secret-token"));
        let db_path = tempdir.path().join("approval-actor.sqlite");
        state.ledger = Arc::new(TokioMutex::new(
            BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger"),
        ));
        state.ledger_backend = LedgerBackend::sqlite(db_path);
        let app = build_router(state);

        let event = json!({
            "trace_id": "trace-approval-actor",
            "kind": "approval.self.approved",
            "payload": {
                "subject": "user:alice",
                "actor": {"id": "user:spoofed", "source": "client"},
                "rule_id": "restricted-tool",
                "decision_reason": "approval requested"
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/events")
                    .header("authorization", "Bearer secret-token")
                    .header("x-noet-request-id", "req-approval")
                    .header("content-type", "application/json")
                    .body(Body::from(event.to_string()))
                    .expect("request"),
            )
            .await
            .expect("event response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let audit_response = app
            .oneshot(
                Request::get("/v1/reports/approval-audit")
                    .header("authorization", "Bearer secret-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("audit response");
        assert_eq!(audit_response.status(), StatusCode::OK);
        let body = to_bytes(audit_response.into_body(), usize::MAX)
            .await
            .expect("audit body");
        let payload: Value = serde_json::from_slice(&body).expect("audit json");
        assert_eq!(payload["items"][0]["actor"], "api_key");
        assert_eq!(payload["items"][0]["request_id"], "req-approval");
    }

    #[tokio::test]
    async fn trusted_actor_header_overwrites_client_claimed_event_actor() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = test_state(None);
        state.api_key = Some(Arc::<str>::from("secret-token"));
        state.actor_header = Some(Arc::<str>::from("x-goog-authenticated-user-email"));
        let db_path = tempdir.path().join("approval-trusted-actor.sqlite");
        state.ledger = Arc::new(TokioMutex::new(
            BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger"),
        ));
        state.ledger_backend = LedgerBackend::sqlite(db_path);
        let app = build_router(state);

        let event = json!({
            "trace_id": "trace-approval-trusted-actor",
            "kind": "approval.self.approved",
            "payload": {
                "subject": "user:eve",
                "actor": {"id": "user:eve", "source": "client"},
                "rule_id": "restricted-tool",
                "decision_reason": "approval requested"
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/events")
                    .header("authorization", "Bearer secret-token")
                    .header(
                        "x-goog-authenticated-user-email",
                        "accounts.google.com:alice@example.com",
                    )
                    .header("x-noet-request-id", "req-trusted-approval")
                    .header("content-type", "application/json")
                    .body(Body::from(event.to_string()))
                    .expect("request"),
            )
            .await
            .expect("event response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let audit_response = app
            .oneshot(
                Request::get("/v1/reports/approval-audit")
                    .header("authorization", "Bearer secret-token")
                    .header(
                        "x-goog-authenticated-user-email",
                        "accounts.google.com:alice@example.com",
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("audit response");
        assert_eq!(audit_response.status(), StatusCode::OK);
        let body = to_bytes(audit_response.into_body(), usize::MAX)
            .await
            .expect("audit body");
        let payload: Value = serde_json::from_slice(&body).expect("audit json");
        assert_eq!(
            payload["items"][0]["actor"],
            "accounts.google.com:alice@example.com"
        );
        assert_eq!(payload["items"][0]["request_id"], "req-trusted-approval");
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
                        warning_cadence: None,
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
                        warning_cadence: None,
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
                        warning_cadence: None,
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
                        warning_cadence: None,
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
        let mut state = state_with_routes(
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
        );
        state.api_key = Some(Arc::<str>::from("noether-secret"));
        state.actor_header = Some(Arc::<str>::from("x-goog-authenticated-user-email"));
        let app = build_router(state);
        let request_body = r#"{"model":"gpt-test", "api_key":"sk-body", "messages":[]}"#;

        let response = app
            .oneshot(
                Request::put("/providers/openai/v1/chat/completions?stream=false")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer sk-test")
                    .header("x-noet-api-key", "noether-secret")
                    .header(
                        "x-goog-authenticated-user-email",
                        "accounts.google.com:alice@example.com",
                    )
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
        assert_eq!(request.headers.get("x-noet-api-key"), None);
        assert_eq!(request.headers.get("x-goog-authenticated-user-email"), None);
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
    async fn transparent_route_strips_noether_bearer_authorization_before_upstream() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture_dir = tempdir.path().join("fixtures");
        let upstream = start_upstream(StatusCode::OK, vec![], br#"{"ok":true}"#).await;
        let mut state = state_with_routes(
            fixture_dir,
            ProxyRoutes {
                routes: vec![ProxyRoute {
                    id: "openai-wrapper".to_owned(),
                    path_prefix: Some("/providers/openai".to_owned()),
                    header_name: None,
                    header_value: None,
                    upstream_base_url: upstream.base_url.clone(),
                }],
            },
        );
        state.api_key = Some(Arc::<str>::from("noether-secret"));
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::post("/providers/openai/v1/responses")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer noether-secret")
                    .body(Body::from(r#"{"model":"gpt-test"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let observed = upstream.observed.lock().await;
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].headers.get("authorization"), None);
    }

    #[tokio::test]
    async fn capture_accepts_body_above_axum_default_limit() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let app = build_router(state_with_dir(
            tempdir.path().join("fixtures"),
            None,
            DecisionMode::DryRun,
        ));
        let body = vec![b'a'; 3 * 1024 * 1024];

        let response = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "text/plain")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
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
        let state = test_state(None);
        let app = build_router(state.clone());
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
    async fn events_endpoint_preserves_non_object_payloads_when_adding_context() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("events.sqlite");
        let mut state = test_state(None);
        state.ledger = Arc::new(TokioMutex::new(
            BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger"),
        ));
        state.ledger_backend = LedgerBackend::sqlite(db_path);
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                Request::post("/v1/events")
                    .header("content-type", "application/json")
                    .header("x-noet-request-id", "req-array-payload")
                    .body(Body::from(
                        json!({
                            "trace_id":"trace-array-payload",
                            "kind":"custom.array_payload",
                            "payload":["kept", 1]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let trace = state
            .read_ledger(|ledger| ledger.trace_report("trace-array-payload"))
            .await
            .expect("trace report");
        assert_eq!(trace.items.len(), 1);
        assert!(
            trace.items[0]
                .summary
                .contains("keys=actor,original_payload,request_id")
        );
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
    async fn noether_app_replay_job_reevaluates_historical_requests_against_draft_policy() {
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
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/app/replay/jobs")
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
        let replay = wait_for_replay_job(&app, &job_id).await;

        assert_eq!(replay["result"]["baseline"]["allow"], 1);
        assert_eq!(replay["result"]["proposal"]["proposed"]["deny"], 1);
        assert_eq!(
            replay["result"]["proposal"]["changed_runs"][0]["run_id"],
            "run-replay"
        );
        assert_eq!(
            replay["result"]["proposal"]["changed_runs"][0]["from"],
            "allow"
        );
        assert_eq!(
            replay["result"]["proposal"]["changed_runs"][0]["to"],
            "deny"
        );
        assert_eq!(
            replay["result"]["proposal"]["recommendations"][0]["action"],
            "review_changed_runs"
        );
        assert_eq!(replay["result"]["proposal"]["mode"], "draft_impact");
        assert_eq!(replay["result"]["proposal"]["can_enforce"], true);
        assert_eq!(replay["result"]["proposal"]["spend_delta_usd"], -1.25);
        assert_eq!(replay["result"]["proposal"]["scope"]["mode"], "full_month");
        assert_eq!(
            replay["result"]["proposal"]["scope"]["request_cap"],
            serde_json::Value::Null
        );
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
                mode: "full_month".to_owned(),
                request_cap: None,
                total_requests_in_window: 150,
                full_replay_available: false,
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
        assert_eq!(replay.scope.mode, "full_month");
        assert!(!replay.scope.full_replay_available);
    }

    #[test]
    fn noether_app_replay_counts_proposed_outcomes_per_authorization() {
        let policy = PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: Vec::new(),
            policies: Vec::new(),
        };
        let occurred_at = chrono::Utc::now();
        let historical_requests = (0..3)
            .map(|index| {
                let mut request = report_request(
                    "trace-shared-run",
                    &format!("request-shared-run-{index}"),
                    "gpt-4.1",
                    0.01,
                );
                request
                    .metadata
                    .insert("agent_run_id".to_owned(), json!("run-shared"));
                crate::ledger::HistoricalAuthorizeRequest {
                    occurred_at: occurred_at + chrono::Duration::seconds(index),
                    decision_id: format!("decision-shared-run-{index}"),
                    baseline_outcome: DecisionOutcome::Allow,
                    request,
                }
            })
            .collect::<Vec<_>>();

        let (totals, changed_runs, _, changed_runs_total) = replay_historical_requests(
            &policy,
            &historical_requests,
            &std::collections::BTreeMap::new(),
            &[],
        )
        .expect("replay");

        assert_eq!(totals.runs, 1);
        assert_eq!(totals.allow, 3);
        assert_eq!(totals.warn, 0);
        assert_eq!(
            totals.allow + totals.warn + totals.deny + totals.ask,
            historical_requests.len() as u64
        );
        assert_eq!(totals.spend_usd, 0.03);
        assert!(changed_runs.is_empty());
        assert_eq!(changed_runs_total, 0);
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
        state.replay_snapshots_path = tempdir.path().join("replay-snapshots.json");
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
        assert_eq!(completed["snapshot"]["scope"]["mode"], "full_month");
        assert_eq!(completed["snapshot"]["policy_stale"], false);
        assert!(tempdir.path().join("replay-snapshots.json").exists());

        let response = app
            .oneshot(
                Request::get("/v1/app/replay")
                    .body(Body::empty())
                    .expect("replay request"),
            )
            .await
            .expect("replay response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("replay body");
        let replay: serde_json::Value = serde_json::from_slice(&body).expect("replay json");
        assert_eq!(replay["snapshots"][0]["id"], completed["id"]);
    }

    #[tokio::test]
    async fn noether_app_replay_hides_running_job_for_previous_draft() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(tempdir.path().join("fixtures"), None, DecisionMode::DryRun);
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        std::fs::write(
            &state.policy_proposal_path,
            serde_yaml::to_string(&require_project_policy()).expect("policy yaml"),
        )
        .expect("write proposal");
        state.replay_jobs.lock().await.insert(
            "stale-job".to_owned(),
            AppReplayJob {
                status: "running".to_owned(),
                history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
                created_at: chrono::Utc::now(),
                completed_at: None,
                active_policy_hash: "old-active-policy".to_owned(),
                proposed_policy_hash: "old-proposed-policy".to_owned(),
                error: None,
                result: None,
                snapshot: None,
            },
        );

        let app = build_router(state);
        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/app/replay")
                    .body(Body::empty())
                    .expect("replay request"),
            )
            .await
            .expect("replay response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("replay body");
        let replay: serde_json::Value = serde_json::from_slice(&body).expect("replay json");
        assert_eq!(replay["current_job"], serde_json::Value::Null);

        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/app/replay/jobs/stale-job")
                    .body(Body::empty())
                    .expect("job status request"),
            )
            .await
            .expect("job status response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .oneshot(
                Request::post("/v1/app/replay/jobs")
                    .body(Body::empty())
                    .expect("start job request"),
            )
            .await
            .expect("start job response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn noether_app_full_replay_job_seeds_prior_spend_windows() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(tempdir.path().join("fixtures"), None, DecisionMode::DryRun);
        state.ledger = Arc::new(Mutex::new(
            BudgetLedger::open_sqlite(&tempdir.path().join("noether.sqlite"))
                .expect("sqlite ledger"),
        ));
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        state.replay_snapshots_path = tempdir.path().join("replay-snapshots.json");
        let mut policy = strict_policy();
        policy.budgets[0].limits.spend[0].window = "31d".to_owned();
        std::fs::write(
            &state.policy_proposal_path,
            serde_yaml::to_string(&policy).expect("policy yaml"),
        )
        .expect("write proposal");
        {
            let mut ledger = state.ledger.lock().await;
            let now = chrono::Utc::now();
            ledger
                .try_authorize_at(
                    Some(&policy),
                    &report_request("trace-prior-spend", "request-prior-spend", "gpt-4.1", 0.007),
                    now - chrono::Duration::days(30) - chrono::Duration::seconds(5),
                )
                .expect("prior authorization");
            ledger
                .try_authorize_at(
                    None,
                    &report_request(
                        "trace-window-replay",
                        "request-window-replay",
                        "gpt-4.1",
                        0.007,
                    ),
                    now,
                )
                .expect("historical authorization");
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
        let completed = wait_for_replay_job(&app, &job_id).await;

        assert_eq!(completed["status"], "completed");
        let proposal = &completed["result"]["proposal"];
        assert_eq!(proposal["scope"]["window_seeded"], true);
        assert_eq!(proposal["proposed"]["deny"], 1);
        assert_eq!(proposal["changed_runs"][0]["to"], "deny");
    }

    #[tokio::test]
    async fn noether_app_full_replay_expires_tumbling_seed_before_later_replay_request() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(tempdir.path().join("fixtures"), None, DecisionMode::DryRun);
        state.ledger = Arc::new(Mutex::new(
            BudgetLedger::open_sqlite(&tempdir.path().join("noether.sqlite"))
                .expect("sqlite ledger"),
        ));
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        state.replay_snapshots_path = tempdir.path().join("replay-snapshots.json");
        let mut policy = strict_policy();
        policy.budgets[0].limits.spend[0].window = "10m".to_owned();
        std::fs::write(
            &state.policy_proposal_path,
            serde_yaml::to_string(&policy).expect("policy yaml"),
        )
        .expect("write proposal");
        {
            let mut ledger = state.ledger.lock().await;
            let replay_window_start =
                chrono::Utc::now() - chrono::Duration::days(APP_REPLAY_HISTORY_WINDOW_DAYS);
            ledger
                .try_authorize_at(
                    Some(&policy),
                    &report_request(
                        "trace-expiring-seed",
                        "request-expiring-seed",
                        "gpt-4.1",
                        0.007,
                    ),
                    replay_window_start - chrono::Duration::minutes(9),
                )
                .expect("prior authorization");
            ledger
                .try_authorize_at(
                    None,
                    &report_request(
                        "trace-after-seed-expired",
                        "request-after-seed-expired",
                        "gpt-4.1",
                        0.007,
                    ),
                    replay_window_start + chrono::Duration::minutes(2),
                )
                .expect("historical authorization");
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
        let completed = wait_for_replay_job(&app, &job_id).await;

        assert_eq!(completed["status"], "completed");
        let proposal = &completed["result"]["proposal"];
        assert_eq!(proposal["scope"]["window_seeded"], true);
        assert_eq!(proposal["proposed"]["allow"], 1);
        assert_eq!(proposal["proposed"]["deny"], 0);
        assert_eq!(
            proposal["changed_runs"]
                .as_array()
                .expect("changed runs")
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn noether_app_replay_does_not_reuse_job_result_after_ledger_changes() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut state = state_with_dir(tempdir.path().join("fixtures"), None, DecisionMode::DryRun);
        state.ledger = Arc::new(Mutex::new(
            BudgetLedger::open_sqlite(&tempdir.path().join("noether.sqlite"))
                .expect("sqlite ledger"),
        ));
        state.policy_proposal_path = tempdir.path().join("policy.proposed.yaml");
        state.replay_snapshots_path = tempdir.path().join("replay-snapshots.json");
        std::fs::write(
            &state.policy_proposal_path,
            serde_yaml::to_string(&require_project_policy()).expect("policy yaml"),
        )
        .expect("write proposal");
        {
            let mut ledger = state.ledger.lock().await;
            let mut request =
                report_request("trace-stale-job", "request-stale-job", "gpt-4.1", 0.01);
            request.project = None;
            request.entities = Vec::new();
            ledger.try_authorize(None, &request).expect("authorize");
        }

        let app = build_router(state.clone());
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
        let completed = wait_for_replay_job(&app, &job_id).await;
        assert_eq!(completed["status"], "completed");

        {
            let mut ledger = state.ledger.lock().await;
            let request = report_request(
                "trace-after-replay",
                "request-after-replay",
                "gpt-4.1",
                0.01,
            );
            ledger.try_authorize(None, &request).expect("authorize");
        }

        let response = app
            .oneshot(
                Request::get("/v1/app/replay")
                    .body(Body::empty())
                    .expect("replay request"),
            )
            .await
            .expect("replay response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("replay body");
        let replay: serde_json::Value = serde_json::from_slice(&body).expect("replay json");
        assert_eq!(replay["proposal"], serde_json::Value::Null);
        assert_eq!(replay["current_job"]["status"], "completed");
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
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/app/replay/jobs")
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
        let replay = wait_for_replay_job(&app, &job_id).await;

        assert_eq!(replay["result"]["proposal"]["proposed"]["allow"], 1);
        assert_eq!(
            replay["result"]["proposal"]["changed_runs"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(replay["result"]["proposal"]["mode"], "draft_impact");
        assert_eq!(replay["result"]["proposal"]["can_enforce"], true);
        assert_eq!(
            replay["result"]["proposal"]["recommendations"][0]["action"],
            "review_policy_diff"
        );
        assert_eq!(replay["result"]["proposal"]["spend_delta_usd"], 0.0);
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
                mode: "full_month".to_owned(),
                request_cap: None,
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
    fn noether_app_replay_baseline_uses_estimate_when_agent_run_usage_is_missing() {
        let mut request = report_request("trace-estimate", "request-estimate", "gpt-4.1", 0.25);
        request
            .metadata
            .insert("agent_run_id".to_owned(), json!("run-estimate"));
        request.estimated_tokens = Some(250);
        let totals = replay_baseline_totals(
            &[crate::ledger::HistoricalAuthorizeRequest {
                occurred_at: chrono::Utc::now(),
                decision_id: "decision-estimate".to_owned(),
                baseline_outcome: DecisionOutcome::Allow,
                request,
            }],
            &std::collections::BTreeMap::<String, AppRunUsage>::new(),
        );

        assert_eq!(totals.runs, 1);
        assert_eq!(totals.spend_usd, 0.25);
        assert_eq!(totals.tokens, 250);
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
        state.ledger_backend = LedgerBackend::sqlite(db_path.clone());
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
        state.ledger_backend = LedgerBackend::postgres(scoped_url, postgres_ledger);
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
        state.ledger_backend = LedgerBackend::postgres(scoped_url, postgres_ledger);
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

    #[tokio::test]
    async fn noether_app_policy_reports_reload_error_after_invalid_edit() {
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

        assert_eq!(payload["status"], "reload_error");
        assert!(
            payload["reload_error"]
                .as_str()
                .expect("reload error")
                .contains("invalid type")
        );
    }
}
