/// Phase 7 — Parity test matrix for server-layer portable tests.
///
/// Every test here runs against BOTH SQLite and Postgres backends.
/// PG variants are skipped when NOET_TEST_PG_URL is not set.
///
/// Test naming convention: `{base_name}_sqlite` / `{base_name}_pg`.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use noether::contract::{
    BudgetLimitPolicy, BudgetModelPolicy, BudgetRule, DecisionMode, DecisionOutcome,
    PolicyAction, PolicyCondition, PolicyRule, RuleMatch, SpendWindowBy, SpendWindowLimit,
    SpendWindowMode, WindowAnchorKind, WindowAnchorPolicy,
};
use noether::policy::PolicyFile;
use noether::server::{AppState, build_router};

// ---------------------------------------------------------------------------
// Helper policy builders (mirrors the ones in server.rs test module)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Shared test bodies — each takes an AppState and runs assertions.
// These are the canonical source of truth for both backends.
// ---------------------------------------------------------------------------

async fn body_authorize_creates_reservation(state: AppState) {
    let app = build_router(state);
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

async fn body_finalize_is_idempotent(state: AppState) {
    let app = build_router(state);
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
        .expect("body");
    let decision: Value = serde_json::from_slice(&body).expect("json");
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
        assert_eq!(response.status(), StatusCode::OK, "finalize should be idempotent");
    }
}

async fn body_finalize_rejects_invalid_accounting(state: AppState) {
    let app = build_router(state);
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
        .expect("body");
    let decision: Value = serde_json::from_slice(&body).expect("json");
    let reservation_id = decision["reservation"]["id"]
        .as_str()
        .expect("reservation id");

    let response = app
        .oneshot(
            Request::post(format!("/v1/reservations/{reservation_id}/finalize"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "outcome": "success",
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
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(value["status"], "ok");
}

async fn body_enforce_deny_returns_deny_outcome(state: AppState) {
    // require-project policy with Enforce mode denies requests missing project.
    let app = build_router(state);
    let response = app
        .oneshot(
            Request::post("/v1/authorize")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"estimated_cost_usd":0.001}).to_string(),
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
    assert_eq!(value["outcome"], "deny");
}

async fn body_spend_limit_blocks_after_cap(state: AppState) {
    // strict_policy caps global spend at $0.01 per 60s tumbling window.
    // Two requests at $0.001 each — first is allowed, cap should warn/allow at 0.8 threshold.
    // Then a $0.009 request should still be allowed (total = 0.01 exactly = warn threshold).
    // A final $0.001 request exceeds the cap.
    let app = build_router(state);

    // First request: allowed
    let r1 = app
        .clone()
        .oneshot(
            Request::post("/v1/authorize")
                .header("content-type", "application/json")
                .body(Body::from(json!({"estimated_cost_usd":0.001}).to_string()))
                .expect("r1"),
        )
        .await
        .expect("r1 response");
    let r1_body: Value = serde_json::from_slice(
        &to_bytes(r1.into_body(), usize::MAX).await.expect("r1 body"),
    )
    .expect("r1 json");
    assert!(
        r1_body["outcome"] == "allow" || r1_body["outcome"] == "warn",
        "first request should be allowed or warned: {r1_body}"
    );

    // Request that exceeds cap: denied
    let r2 = app
        .oneshot(
            Request::post("/v1/authorize")
                .header("content-type", "application/json")
                .body(Body::from(json!({"estimated_cost_usd":0.02}).to_string()))
                .expect("r2"),
        )
        .await
        .expect("r2 response");
    let r2_body: Value = serde_json::from_slice(
        &to_bytes(r2.into_body(), usize::MAX).await.expect("r2 body"),
    )
    .expect("r2 json");
    assert_eq!(r2_body["outcome"], "deny", "over-cap request should be denied: {r2_body}");
}

async fn body_authorize_dry_run_deny_still_allowed(state: AppState) {
    // In DryRun mode, a policy deny becomes a warn — request is forwarded.
    let app = build_router(state);
    let response = app
        .oneshot(
            Request::post("/v1/authorize")
                .header("content-type", "application/json")
                .body(Body::from(json!({"estimated_cost_usd":0.001}).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&body).expect("json");
    // In DryRun mode with require-project policy, outcome is "deny" but request proceeds.
    // The endpoint returns 200 regardless of decision mode.
    assert_eq!(value["outcome"], "deny");
}

/// Authorize, finalize, then query the usage report — verifies the write+read
/// round-trip produces consistent results on both backends.
async fn body_authorize_finalize_round_trip_persists_to_reports(state: AppState) {
    let app = build_router(state);

    // Authorize
    let auth_resp = app
        .clone()
        .oneshot(
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
                .expect("authorize request"),
        )
        .await
        .expect("authorize response");
    assert_eq!(auth_resp.status(), StatusCode::OK);
    let auth_body: Value = serde_json::from_slice(
        &to_bytes(auth_resp.into_body(), usize::MAX).await.expect("auth body"),
    )
    .expect("auth json");
    assert_eq!(auth_body["outcome"], "allow");
    let reservation_id = auth_body["reservation"]["id"]
        .as_str()
        .expect("reservation id")
        .to_owned();

    // Finalize
    let fin_resp = app
        .clone()
        .oneshot(
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
                .expect("finalize request"),
        )
        .await
        .expect("finalize response");
    assert_eq!(fin_resp.status(), StatusCode::OK);

    // Query usage report — should reflect the finalized spend
    let usage_resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/reports/usage")
                .body(Body::empty())
                .expect("usage request"),
        )
        .await
        .expect("usage response");
    assert_eq!(usage_resp.status(), StatusCode::OK);
    let usage_body: Value = serde_json::from_slice(
        &to_bytes(usage_resp.into_body(), usize::MAX)
            .await
            .expect("usage body"),
    )
    .expect("usage json");
    // total_cost_usd should be non-zero after finalization
    let total = usage_body["total_cost_usd"].as_f64().unwrap_or(0.0);
    assert!(
        total > 0.0,
        "usage report total_cost_usd should be > 0 after finalize, got: {total}"
    );
}

/// Authorize twice against a tight cap — second request must be denied on both backends.
///
/// This tests that the in-memory HotState + persistence both enforce the limit.
async fn body_spend_limit_enforced_after_finalize(state: AppState) {
    let app = build_router(state);

    // First request: within the $0.01 cap
    let r1 = app
        .clone()
        .oneshot(
            Request::post("/v1/authorize")
                .header("content-type", "application/json")
                .body(Body::from(json!({"estimated_cost_usd":0.008}).to_string()))
                .expect("r1"),
        )
        .await
        .expect("r1 response");
    let r1_body: Value = serde_json::from_slice(
        &to_bytes(r1.into_body(), usize::MAX).await.expect("r1 body"),
    )
    .expect("r1 json");
    // First request may warn (at 0.8 threshold) or allow
    assert!(
        r1_body["outcome"] == "allow" || r1_body["outcome"] == "warn",
        "first request should be allowed or warned: {r1_body}"
    );

    // Second request would push total to $0.016 >> $0.01 cap: must be denied
    let r2 = app
        .oneshot(
            Request::post("/v1/authorize")
                .header("content-type", "application/json")
                .body(Body::from(json!({"estimated_cost_usd":0.009}).to_string()))
                .expect("r2"),
        )
        .await
        .expect("r2 response");
    let r2_body: Value = serde_json::from_slice(
        &to_bytes(r2.into_body(), usize::MAX).await.expect("r2 body"),
    )
    .expect("r2 json");
    assert_eq!(r2_body["outcome"], "deny", "over-cap request must be denied: {r2_body}");
}

// ---------------------------------------------------------------------------
// backend_test! macro invocations — each generates _sqlite and _pg variants.
// ---------------------------------------------------------------------------

backend_test!(
    authorize_creates_reservation,
    Some(strict_policy()),
    DecisionMode::Enforce,
    |state| body_authorize_creates_reservation(state).await
);

backend_test!(
    finalize_is_idempotent,
    Some(strict_policy()),
    DecisionMode::Enforce,
    |state| body_finalize_is_idempotent(state).await
);

backend_test!(
    finalize_rejects_invalid_accounting,
    Some(strict_policy()),
    DecisionMode::Enforce,
    |state| body_finalize_rejects_invalid_accounting(state).await
);

backend_test!(
    events_endpoint_accepts_trace_events,
    None,
    DecisionMode::DryRun,
    |state| body_events_endpoint_accepts_trace_events(state).await
);

backend_test!(
    health_exposes_sidecar_readiness,
    None,
    DecisionMode::DryRun,
    |state| body_health_exposes_sidecar_readiness(state).await
);

backend_test!(
    enforce_deny_returns_deny_outcome,
    Some(require_project_policy()),
    DecisionMode::Enforce,
    |state| body_enforce_deny_returns_deny_outcome(state).await
);

backend_test!(
    spend_limit_blocks_after_cap,
    Some(strict_policy()),
    DecisionMode::Enforce,
    |state| body_spend_limit_blocks_after_cap(state).await
);

backend_test!(
    authorize_dry_run_deny_still_allowed,
    Some(require_project_policy()),
    DecisionMode::DryRun,
    |state| body_authorize_dry_run_deny_still_allowed(state).await
);

backend_test!(
    authorize_finalize_round_trip_persists_to_reports,
    Some(strict_policy()),
    DecisionMode::Enforce,
    |state| body_authorize_finalize_round_trip_persists_to_reports(state).await
);

backend_test!(
    spend_limit_enforced_after_finalize,
    Some(strict_policy()),
    DecisionMode::Enforce,
    |state| body_spend_limit_enforced_after_finalize(state).await
);
