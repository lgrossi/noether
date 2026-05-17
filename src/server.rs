use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::fs;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::capture::capture;
use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, DecisionMode, FinalizeReservation, Reservation, TraceEvent,
};
use crate::error::NoetError;
use crate::ledger::BudgetLedger;
use crate::live_dashboard;
use crate::policy::PolicyFile;
use crate::proxy::ProxyRoute;
use crate::reporting;

#[derive(Clone)]
pub struct AppState {
    pub fixture_dir: PathBuf,
    pub upstream: Option<url::Url>,
    pub routes: Vec<ProxyRoute>,
    pub client: reqwest::Client,
    pub policy: Option<Arc<PolicyFile>>,
    pub decision_mode: DecisionMode,
    pub ledger: Arc<Mutex<BudgetLedger>>,
    pub report_updates: broadcast::Sender<ReportUpdate>,
}

impl AppState {
    pub fn new(
        fixture_dir: PathBuf,
        upstream: Option<url::Url>,
        policy: Option<PolicyFile>,
        decision_mode: DecisionMode,
    ) -> Self {
        let (report_updates, _) = broadcast::channel(64);
        Self {
            fixture_dir,
            upstream,
            routes: Vec::new(),
            client: reqwest::Client::new(),
            policy: policy.map(Arc::new),
            decision_mode,
            ledger: Arc::new(Mutex::new(BudgetLedger::default())),
            report_updates,
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
    pub db_path: PathBuf,
    pub upstream: Option<url::Url>,
    pub routes: Vec<ProxyRoute>,
    pub policy: Option<PolicyFile>,
    pub decision_mode: DecisionMode,
}

pub async fn serve(config: ServeConfig) -> Result<(), NoetError> {
    fs::create_dir_all(&config.fixture_dir).await?;
    if let Some(parent) = config.db_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let bind = config.bind;
    let ledger = BudgetLedger::open_sqlite(&config.db_path)?;
    let mut state = AppState::new(
        config.fixture_dir,
        config.upstream,
        config.policy,
        config.decision_mode,
    );
    state.routes = config.routes;
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
        .route("/v1/reports/dashboard-data", get(report_dashboard_data))
        .route("/v1/reports/dashboard", get(report_dashboard_html))
        .route("/v1/reports/updates", get(report_updates_stream))
        .route("/dashboard", get(live_dashboard_html))
        .route("/dashboard/app.js", get(live_dashboard_js))
        .route("/dashboard/app.css", get(live_dashboard_css))
        .route("/v1/chat/completions", any(capture))
        .route("/v1/messages", any(capture))
        .route("/v1/responses", any(capture))
        .route("/health", any(health))
        .fallback(any(capture))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn authorize(
    State(state): State<AppState>,
    Json(request): Json<AuthorizeRequest>,
) -> Result<Json<AuthorizeDecision>, NoetError> {
    let decision = state
        .ledger
        .lock()
        .await
        .try_authorize(state.policy.as_deref(), &request)?;
    publish_report_update(&state, "authorize", request_trace_id(&request));
    Ok(Json(decision))
}

async fn finalize_reservation(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(payload): Json<FinalizeReservation>,
) -> Result<Json<Reservation>, NoetError> {
    let reservation = state.ledger.lock().await.finalize(&id, &payload)?;
    publish_report_update(&state, "finalize", finalize_trace_id(&payload));
    Ok(Json(reservation))
}

async fn record_event(
    State(state): State<AppState>,
    Json(event): Json<TraceEvent>,
) -> Result<impl IntoResponse, NoetError> {
    let trace_id = event.trace_id.clone();
    state.ledger.lock().await.record_event(event)?;
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

async fn report_usage(State(state): State<AppState>) -> Result<Json<serde_json::Value>, NoetError> {
    let ledger = state.ledger.lock().await;
    Ok(Json(serde_json::to_value(reporting::usage_report(
        &ledger,
    )?)?))
}

async fn report_decisions(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let ledger = state.ledger.lock().await;
    Ok(Json(serde_json::to_value(reporting::decisions_report(
        &ledger,
    )?)?))
}

async fn report_trace(
    State(state): State<AppState>,
    AxumPath(trace_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let ledger = state.ledger.lock().await;
    Ok(Json(serde_json::to_value(reporting::trace_report(
        &ledger, &trace_id,
    )?)?))
}

async fn report_observations(
    State(state): State<AppState>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let ledger = state.ledger.lock().await;
    Ok(Json(serde_json::to_value(reporting::observations_report(
        &ledger,
        query.kind.as_deref(),
        query.trace.as_deref(),
    )?)?))
}

async fn report_dashboard_data(
    State(state): State<AppState>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<serde_json::Value>, NoetError> {
    let ledger = state.ledger.lock().await;
    Ok(Json(serde_json::to_value(reporting::dashboard_report(
        &ledger,
        query.trace.as_deref(),
    )?)?))
}

async fn report_dashboard_html(
    State(state): State<AppState>,
    Query(query): Query<ReportQuery>,
) -> Result<Html<String>, NoetError> {
    let ledger = state.ledger.lock().await;
    let report = reporting::dashboard_report(&ledger, query.trace.as_deref())?;
    Ok(Html(render_dashboard_artifact(&report)))
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

async fn live_dashboard_html(Query(query): Query<ReportQuery>) -> Result<Html<String>, NoetError> {
    Ok(Html(live_dashboard::dashboard_shell(
        query.trace.as_deref(),
    )))
}

async fn live_dashboard_js() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/javascript; charset=utf-8")],
        live_dashboard::dashboard_js(),
    )
}

async fn live_dashboard_css() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/css; charset=utf-8")],
        live_dashboard::dashboard_css(),
    )
}

fn render_dashboard_artifact(report: &reporting::DashboardReportData) -> String {
    let featured_trace = report.featured_trace_id.as_deref().unwrap_or("latest");
    let decision_total = report.summary.decisions.total();
    let tool_count = report.summary.activity.tools;
    let agent_count = report.summary.activity.agent;
    let context_count = report.summary.activity.skill_context;
    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Noether reporting dashboard</title>");
    html.push_str(
        "<style>
        :root { color-scheme: dark; --bg:#0f172a; --panel:#111c33; --muted:#94a3b8; --text:#e5edf7; --line:#263449; --blue:#38bdf8; }
        * { box-sizing: border-box; }
        body { margin:0; font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; background:radial-gradient(circle at top left,#172554,#0f172a 42%); color:var(--text); }
        main { max-width:1100px; margin:0 auto; padding:32px 20px 48px; }
        h1,h2 { margin:0; letter-spacing:-0.03em; }
        h1 { font-size:34px; margin-bottom:6px; }
        h2 { font-size:22px; margin-bottom:12px; }
        .sub,.muted { color:var(--muted); }
        .hero,.panel { background:rgba(17,28,51,.88); border:1px solid var(--line); border-radius:18px; box-shadow:0 18px 55px rgba(0,0,0,.22); padding:20px; margin-top:16px; }
        .hero { margin-top:0; }
        .grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(220px,1fr)); margin-top:16px; }
        .card { background:rgba(15,23,42,.55); border:1px solid rgba(148,163,184,.14); border-radius:16px; padding:16px; }
        .label { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.08em; }
        .value { font-size:28px; font-weight:800; margin-top:6px; }
        ul { margin:12px 0 0 18px; padding:0; }
        li { margin:6px 0; }
        .entry { border-top:1px solid var(--line); padding:12px 0; }
        .entry:first-child { border-top:0; padding-top:0; }
        .pill { display:inline-block; padding:4px 9px; border-radius:999px; background:#1e293b; border:1px solid var(--line); font-size:12px; margin-right:8px; }
        code { color:var(--blue); }
        </style>",
    );
    html.push_str("</head><body><main>");
    html.push_str("<h1>Noether reporting dashboard</h1>");
    html.push_str("<div class=\"sub\">HTTP-served reporting artifact backed by the same ledger read model as CLI export.</div>");
    html.push_str("<section class=\"hero\">");
    html.push_str("<div class=\"label\">Featured trace</div>");
    html.push_str(&format!(
        "<div class=\"value\"><code>{}</code></div>",
        escape_html(featured_trace)
    ));
    html.push_str("<p class=\"muted\">This artifact summarizes the latest reporting state without depending on the live dashboard UI.</p>");
    html.push_str("</section>");

    html.push_str("<section class=\"grid\">");
    metric_card_html(
        &mut html,
        "Finalized spend",
        &format_money(report.usage.total_cost_usd),
        "finalized cost in the ledger",
    );
    metric_card_html(
        &mut html,
        "Decisions",
        &decision_total.to_string(),
        "authorize outcomes captured in the report set",
    );
    metric_card_html(
        &mut html,
        "Tokens",
        &report.summary.usage.total_tokens.to_string(),
        "finalized tokens across the selected report set",
    );
    metric_card_html(
        &mut html,
        "Evidence",
        &format!("{tool_count} tools · {agent_count} agent · {context_count} context"),
        "observed activity attached to the selected trace",
    );
    html.push_str("</section>");

    html.push_str("<section class=\"panel\"><h2>Policy decisions</h2>");
    if report.decisions.is_empty() {
        html.push_str("<p class=\"muted\">No authorization decisions have been recorded yet.</p>");
    } else {
        for item in report.decisions.iter().take(8) {
            html.push_str("<div class=\"entry\">");
            html.push_str(&format!(
                "<div><span class=\"pill\">{}</span><strong>{}</strong></div>",
                escape_html(&item.kind),
                escape_html(&item.summary)
            ));
            html.push_str("</div>");
        }
    }
    html.push_str("</section>");

    html.push_str("<section class=\"panel\"><h2>Available traces</h2>");
    if report.available_traces.is_empty() {
        html.push_str("<p class=\"muted\">No trace-backed decisions are available yet.</p>");
    } else {
        html.push_str("<ul>");
        for trace in &report.available_traces {
            html.push_str(&format!(
                "<li><code>{}</code> · {} · {}</li>",
                escape_html(&trace.trace_id),
                escape_html(trace.latest_decision_kind.as_deref().unwrap_or("decision")),
                escape_html(&trace.latest_decision_summary)
            ));
        }
        html.push_str("</ul>");
    }
    html.push_str("</section>");

    html.push_str("<section class=\"panel\"><h2>Recent observations</h2>");
    if report.observations.is_empty() {
        html.push_str("<p class=\"muted\">No observations matched the selected trace yet.</p>");
    } else {
        for item in report.observations.iter().take(8) {
            html.push_str(&format!(
                "<div class=\"entry\"><span class=\"pill\">{}</span>{}</div>",
                escape_html(&item.kind),
                escape_html(&item.summary)
            ));
        }
    }
    html.push_str("</section>");

    html.push_str("</main></body></html>");
    html
}

fn metric_card_html(html: &mut String, label: &str, value: &str, hint: &str) {
    html.push_str("<article class=\"card\">");
    html.push_str(&format!(
        "<div class=\"label\">{}</div><div class=\"value\">{}</div><div class=\"muted\">{}</div>",
        escape_html(label),
        escape_html(value),
        escape_html(hint)
    ));
    html.push_str("</article>");
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

    use crate::contract::{BudgetRule, PolicyCondition, PolicyEffect, PolicyRule, RuleMatch};
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

    fn strict_policy() -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "tiny".to_owned(),
                limit_usd: 0.01,
                priority: 0,
                warn_at_fraction: 0.8,
                window_seconds: 60,
                eligible: Default::default(),
                models: Default::default(),
                guards: Default::default(),
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
                effect: PolicyEffect::Deny,
                reason: "project is required".to_owned(),
                when: PolicyCondition {
                    missing: Some("project".to_owned()),
                    rule_match: RuleMatch::default(),
                },
            }],
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
        let expected_dashboard = {
            let ledger = state.ledger.lock().await;
            serde_json::to_value(
                reporting::dashboard_report(&ledger, Some("trace-beta")).expect("dashboard data"),
            )
            .expect("dashboard json")
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

        let dashboard_response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/reports/dashboard-data?trace=trace-beta")
                    .body(Body::empty())
                    .expect("dashboard data request"),
            )
            .await
            .expect("dashboard data response");
        assert_eq!(dashboard_response.status(), StatusCode::OK);
        let dashboard_body = to_bytes(dashboard_response.into_body(), usize::MAX)
            .await
            .expect("dashboard data body");
        let dashboard_json: Value =
            serde_json::from_slice(&dashboard_body).expect("dashboard value");
        assert_eq!(dashboard_json, expected_dashboard);
    }

    #[tokio::test]
    async fn dashboard_html_endpoint_renders_report_artifact_markers() {
        let state = test_state(None);
        seed_reporting_data(&state).await;
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/reports/dashboard?trace=trace-beta")
                    .body(Body::empty())
                    .expect("dashboard request"),
            )
            .await
            .expect("dashboard response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("dashboard body");
        let html = String::from_utf8(body.to_vec()).expect("dashboard html");
        for marker in [
            "Noether reporting dashboard",
            "Featured trace",
            "trace-beta",
            "Policy decisions",
            "Available traces",
            "Recent observations",
        ] {
            assert!(html.contains(marker), "missing dashboard marker: {marker}");
        }
    }

    #[tokio::test]
    async fn live_dashboard_shell_serves_bootstrapped_trace_picker() {
        let app = build_router(test_state(None));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/dashboard?trace=trace-beta")
                    .body(Body::empty())
                    .expect("dashboard shell request"),
            )
            .await
            .expect("dashboard shell response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("dashboard shell body");
        let html = String::from_utf8(body.to_vec()).expect("dashboard shell html");
        for marker in [
            "Noether live dashboard",
            "dashboard-trace-select",
            "/dashboard/app.js",
            "/dashboard/app.css",
            "trace-beta",
        ] {
            assert!(html.contains(marker), "missing live shell marker: {marker}");
        }
    }

    #[tokio::test]
    async fn live_dashboard_assets_reference_reporting_api() {
        let app = build_router(test_state(None));

        let js_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/app.js")
                    .body(Body::empty())
                    .expect("js request"),
            )
            .await
            .expect("js response");
        assert_eq!(js_response.status(), StatusCode::OK);
        let js_body = to_bytes(js_response.into_body(), usize::MAX)
            .await
            .expect("js body");
        let js = String::from_utf8(js_body.to_vec()).expect("dashboard js");
        assert!(js.contains("/v1/reports/dashboard-data"));
        assert!(!js.contains("<html"));

        let css_response = app
            .oneshot(
                Request::builder()
                    .uri("/dashboard/app.css")
                    .body(Body::empty())
                    .expect("css request"),
            )
            .await
            .expect("css response");
        assert_eq!(css_response.status(), StatusCode::OK);
        let css_body = to_bytes(css_response.into_body(), usize::MAX)
            .await
            .expect("css body");
        let css = String::from_utf8(css_body.to_vec()).expect("dashboard css");
        assert!(css.contains(".overview"));
        assert!(css.contains(".panel-block"));
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
}
