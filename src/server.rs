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

#[derive(Clone)]
pub struct AppState {
    pub fixture_dir: PathBuf,
    pub upstream: Option<url::Url>,
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
    pub policy: Option<PolicyFile>,
    pub decision_mode: DecisionMode,
}

pub async fn serve(config: ServeConfig) -> Result<(), NoetError> {
    fs::create_dir_all(&config.fixture_dir).await?;
    let bind = config.bind;
    let app = build_router(AppState::new(
        config.fixture_dir,
        config.upstream,
        config.policy,
        config.decision_mode,
    ));

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
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::contract::{BudgetRule, PolicyCondition, PolicyEffect, PolicyRule, RuleMatch};
    use crate::fixture::{ResponseSource, list_fixture_paths, read_fixture};
    use crate::policy::PolicyFile;

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
