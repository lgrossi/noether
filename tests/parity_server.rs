mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use noether::contract::{
    BudgetLimitPolicy, BudgetModelPolicy, BudgetRule, DecisionMode, PolicyAction, PolicyCondition,
    PolicyRule, RuleMatch, SpendWindowBy, SpendWindowLimit, SpendWindowMode, WindowAnchorKind,
    WindowAnchorPolicy,
};
use noether::policy::PolicyFile;
use noether::server::{AppState, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;

fn strict_policy() -> PolicyFile {
    PolicyFile {
        version: 0,
        routing: Default::default(),
        budgets: vec![BudgetRule {
            id: "tiny".to_owned(),
            priority: 0,
            models: BudgetModelPolicy::default(),
            limits: BudgetLimitPolicy {
                request_cost: None,
                context_tokens: None,
                spend: vec![SpendWindowLimit {
                    id: Some("budget-cap".to_owned()),
                    by: SpendWindowBy::Global,
                    window: "60s".to_owned(),
                    mode: Some(SpendWindowMode::Tumbling),
                    anchor: Some(WindowAnchorPolicy {
                        kind: WindowAnchorKind::FirstSeen,
                    }),
                    max_usd: 0.01,
                    warn_at_fractions: vec![0.8],
                    action: PolicyAction::Block,
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

async fn request_json(app: axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).expect("json response")
    };
    (status, value)
}

async fn body_authorize_creates_reservation(state: AppState) {
    let app = build_router(state);
    let (status, value) = request_json(
        app,
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"project":"noether","estimated_cost_usd":0.001}).to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["outcome"], "allow");
    assert!(value["reservation"]["id"].is_string());
}

async fn body_finalize_is_idempotent(state: AppState) {
    let app = build_router(state);
    let (status, decision) = request_json(
        app.clone(),
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"project":"noether","estimated_cost_usd":0.001}).to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let reservation_id = decision["reservation"]["id"]
        .as_str()
        .expect("reservation id");

    for _ in 0..2 {
        let (status, _) = request_json(
            app.clone(),
            Request::post(format!("/v1/reservations/{reservation_id}/finalize"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"actual_cost_usd":0.001}).to_string()))
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
}

async fn body_finalize_rejects_invalid_accounting(state: AppState) {
    let app = build_router(state);
    let (status, decision) = request_json(
        app.clone(),
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"project":"noether","estimated_cost_usd":0.001}).to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let reservation_id = decision["reservation"]["id"]
        .as_str()
        .expect("reservation id");

    let (status, _) = request_json(
        app,
        Request::post(format!("/v1/reservations/{reservation_id}/finalize"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"outcome":"success","actual_cost_usd":-0.001}).to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

async fn body_events_endpoint_accepts_trace_events(state: AppState) {
    let app = build_router(state);
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

async fn body_health_exposes_sidecar_readiness(state: AppState) {
    let app = build_router(state);
    let (status, value) = request_json(
        app,
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["status"], "ok");
}

async fn body_enforce_deny_returns_deny_outcome(state: AppState) {
    let app = build_router(state);
    let (status, value) = request_json(
        app,
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(json!({"estimated_cost_usd":0.001}).to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["outcome"], "deny");
}

async fn body_spend_limit_blocks_after_cap(state: AppState) {
    let app = build_router(state);

    let (status, first) = request_json(
        app.clone(),
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(json!({"estimated_cost_usd":0.001}).to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        first["outcome"] == "allow" || first["outcome"] == "warn",
        "first request should be allowed or warned: {first}"
    );

    let (status, second) = request_json(
        app,
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(json!({"estimated_cost_usd":0.02}).to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["outcome"], "deny");
}

async fn body_authorize_dry_run_deny_still_returns_decision(state: AppState) {
    let app = build_router(state);
    let (status, value) = request_json(
        app,
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(json!({"estimated_cost_usd":0.001}).to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["outcome"], "deny");
}

async fn body_authorize_finalize_round_trip_persists_to_reports(state: AppState) {
    let app = build_router(state);
    let (status, decision) = request_json(
        app.clone(),
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "project": "parity-test",
                    "provider": "openai",
                    "model": "gpt-4.1",
                    "estimated_cost_usd": 0.001
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(decision["outcome"], "allow");
    let reservation_id = decision["reservation"]["id"]
        .as_str()
        .expect("reservation id");

    let (status, _) = request_json(
        app.clone(),
        Request::post(format!("/v1/reservations/{reservation_id}/finalize"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "outcome": "success",
                    "actual_cost_usd": 0.001,
                    "usage": {
                        "provider": "openai",
                        "model": "gpt-4.1",
                        "input_tokens": 100,
                        "output_tokens": 50,
                        "total_tokens": 150,
                        "cost_usd": 0.001
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, usage) = request_json(
        app,
        Request::builder()
            .uri("/v1/reports/usage")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let total = usage["total_cost_usd"].as_f64().unwrap_or(0.0);
    assert!(
        total > 0.0,
        "usage report did not include finalized spend: {usage}"
    );
}

async fn body_spend_limit_enforced_after_reservation(state: AppState) {
    let app = build_router(state);

    let (status, first) = request_json(
        app.clone(),
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(json!({"estimated_cost_usd":0.008}).to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        first["outcome"] == "allow" || first["outcome"] == "warn",
        "first request should be allowed or warned: {first}"
    );

    let (status, second) = request_json(
        app,
        Request::post("/v1/authorize")
            .header("content-type", "application/json")
            .body(Body::from(json!({"estimated_cost_usd":0.009}).to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["outcome"], "deny");
}

#[tokio::test]
async fn authorize_creates_reservation_on_sqlite_and_postgres() {
    common::run_server_parity(
        "authorize_creates_reservation",
        Some(strict_policy()),
        DecisionMode::Enforce,
        body_authorize_creates_reservation,
    )
    .await;
}

#[tokio::test]
async fn finalize_is_idempotent_on_sqlite_and_postgres() {
    common::run_server_parity(
        "finalize_is_idempotent",
        Some(strict_policy()),
        DecisionMode::Enforce,
        body_finalize_is_idempotent,
    )
    .await;
}

#[tokio::test]
async fn finalize_rejects_invalid_accounting_on_sqlite_and_postgres() {
    common::run_server_parity(
        "finalize_rejects_invalid_accounting",
        Some(strict_policy()),
        DecisionMode::Enforce,
        body_finalize_rejects_invalid_accounting,
    )
    .await;
}

#[tokio::test]
async fn events_endpoint_accepts_trace_events_on_sqlite_and_postgres() {
    common::run_server_parity(
        "events_endpoint_accepts_trace_events",
        None,
        DecisionMode::DryRun,
        body_events_endpoint_accepts_trace_events,
    )
    .await;
}

#[tokio::test]
async fn health_exposes_sidecar_readiness_on_sqlite_and_postgres() {
    common::run_server_parity(
        "health_exposes_sidecar_readiness",
        None,
        DecisionMode::DryRun,
        body_health_exposes_sidecar_readiness,
    )
    .await;
}

#[tokio::test]
async fn enforce_deny_returns_deny_outcome_on_sqlite_and_postgres() {
    common::run_server_parity(
        "enforce_deny_returns_deny_outcome",
        Some(require_project_policy()),
        DecisionMode::Enforce,
        body_enforce_deny_returns_deny_outcome,
    )
    .await;
}

#[tokio::test]
async fn spend_limit_blocks_after_cap_on_sqlite_and_postgres() {
    common::run_server_parity(
        "spend_limit_blocks_after_cap",
        Some(strict_policy()),
        DecisionMode::Enforce,
        body_spend_limit_blocks_after_cap,
    )
    .await;
}

#[tokio::test]
async fn authorize_dry_run_deny_still_returns_decision_on_sqlite_and_postgres() {
    common::run_server_parity(
        "authorize_dry_run_deny_still_returns_decision",
        Some(require_project_policy()),
        DecisionMode::DryRun,
        body_authorize_dry_run_deny_still_returns_decision,
    )
    .await;
}

#[tokio::test]
async fn authorize_finalize_round_trip_persists_to_reports_on_sqlite_and_postgres() {
    common::run_server_parity(
        "authorize_finalize_round_trip_persists_to_reports",
        Some(strict_policy()),
        DecisionMode::Enforce,
        body_authorize_finalize_round_trip_persists_to_reports,
    )
    .await;
}

#[tokio::test]
async fn spend_limit_enforced_after_reservation_on_sqlite_and_postgres() {
    common::run_server_parity(
        "spend_limit_enforced_after_reservation",
        Some(strict_policy()),
        DecisionMode::Enforce,
        body_spend_limit_enforced_after_reservation,
    )
    .await;
}
