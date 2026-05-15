use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{any, post};
use axum::{Json, Router};
use tokio::fs;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::capture::capture;
use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, DecisionMode, FinalizeReservation, Reservation, TraceEvent,
};
use crate::error::NoetError;
use crate::ledger::BudgetLedger;
use crate::policy::PolicyFile;
use crate::proxy::ProxyRoute;

#[derive(Clone)]
pub struct AppState {
    pub fixture_dir: PathBuf,
    pub upstream: Option<url::Url>,
    pub routes: Vec<ProxyRoute>,
    pub client: reqwest::Client,
    pub policy: Option<Arc<PolicyFile>>,
    pub decision_mode: DecisionMode,
    pub ledger: Arc<Mutex<BudgetLedger>>,
}

impl AppState {
    pub fn new(
        fixture_dir: PathBuf,
        upstream: Option<url::Url>,
        policy: Option<PolicyFile>,
        decision_mode: DecisionMode,
    ) -> Self {
        Self {
            fixture_dir,
            upstream,
            routes: Vec::new(),
            client: reqwest::Client::new(),
            policy: policy.map(Arc::new),
            decision_mode,
            ledger: Arc::new(Mutex::new(BudgetLedger::default())),
        }
    }
}

pub struct ServeConfig {
    pub bind: SocketAddr,
    pub fixture_dir: PathBuf,
    pub upstream: Option<url::Url>,
    pub routes: Vec<ProxyRoute>,
    pub policy: Option<PolicyFile>,
    pub decision_mode: DecisionMode,
}

pub async fn serve(config: ServeConfig) -> Result<(), NoetError> {
    fs::create_dir_all(&config.fixture_dir).await?;
    let bind = config.bind;
    let mut state = AppState::new(
        config.fixture_dir,
        config.upstream,
        config.policy,
        config.decision_mode,
    );
    state.routes = config.routes;
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
) -> Json<AuthorizeDecision> {
    let decision = state
        .ledger
        .lock()
        .await
        .authorize(state.policy.as_deref(), &request);
    Json(decision)
}

async fn finalize_reservation(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(payload): Json<FinalizeReservation>,
) -> Result<Json<Reservation>, NoetError> {
    let reservation = state.ledger.lock().await.finalize(&id, &payload)?;
    Ok(Json(reservation))
}

async fn record_event(
    State(state): State<AppState>,
    Json(event): Json<TraceEvent>,
) -> impl IntoResponse {
    state.ledger.lock().await.record_event(event);
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true })),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
    use axum::routing::any;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokioMutex;
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

    fn strict_policy() -> PolicyFile {
        PolicyFile {
            version: 0,
            budgets: vec![BudgetRule {
                id: "tiny".to_owned(),
                limit_usd: 0.01,
                warn_at_fraction: 0.8,
                window_seconds: 60,
                rule_match: RuleMatch::default(),
            }],
            policies: Vec::new(),
        }
    }

    fn require_project_policy() -> PolicyFile {
        PolicyFile {
            version: 0,
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
