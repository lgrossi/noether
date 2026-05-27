use std::time::Duration;

use noether::contract::{
    AuthorizeDecision, AuthorizeRequest, DecisionExplanation, DecisionOutcome, DecisionSeverity,
    FinalizeReservation, PolicyAction, Reservation, TraceEvent,
};
use reqwest::StatusCode;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailMode {
    FailOpen,
    FailClosed,
}

#[derive(Clone)]
pub struct NoetherClient {
    url: reqwest::Url,
    client: reqwest::Client,
    fail_mode: FailMode,
}

#[derive(Debug, Error)]
pub enum NoetherClientError {
    #[error("invalid Noether URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("Noether request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Noether request failed with HTTP {status}: {body}")]
    Http { status: StatusCode, body: String },
    #[error("Noether denied request: {0}")]
    Denied(String, Box<AuthorizeDecision>),
}

impl NoetherClient {
    pub fn new(url: &str) -> Result<Self, NoetherClientError> {
        Ok(Self {
            url: reqwest::Url::parse(url.trim_end_matches('/'))?,
            client: reqwest::Client::builder()
                .timeout(Duration::from_millis(1_000))
                .build()?,
            fail_mode: FailMode::FailClosed,
        })
    }

    pub fn with_timeout(self, timeout: Duration) -> Result<Self, NoetherClientError> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(timeout).build()?,
            ..self
        })
    }

    pub fn with_fail_mode(mut self, fail_mode: FailMode) -> Self {
        self.fail_mode = fail_mode;
        self
    }

    pub async fn authorize(&self, request: &AuthorizeRequest) -> AuthorizeDecision {
        match self.post("v1/authorize", request).await {
            Ok(decision) => decision,
            Err(error) => synthetic_decision(self.fail_mode, &error),
        }
    }

    pub async fn require_authorization(
        &self,
        request: &AuthorizeRequest,
    ) -> Result<AuthorizeDecision, NoetherClientError> {
        let decision = self.authorize(request).await;
        if decision.outcome == DecisionOutcome::Deny {
            let reason = decision
                .explanations
                .iter()
                .map(|explanation| explanation.reason.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(NoetherClientError::Denied(reason, Box::new(decision)));
        }
        Ok(decision)
    }

    pub async fn finalize(
        &self,
        reservation_id: &str,
        payload: &FinalizeReservation,
    ) -> Result<Reservation, NoetherClientError> {
        self.post(
            &format!(
                "v1/reservations/{}/finalize",
                percent_encode_path_component(reservation_id)
            ),
            payload,
        )
        .await
    }

    pub async fn event(&self, event: &TraceEvent) -> Result<serde_json::Value, NoetherClientError> {
        self.post("v1/events", event).await
    }

    pub async fn health(&self) -> Result<HealthResponse, NoetherClientError> {
        self.get("health").await
    }

    pub async fn with_decision<T, F>(
        &self,
        request: &AuthorizeRequest,
        run: F,
    ) -> Result<T, NoetherClientError>
    where
        F: FnOnce(AuthorizeDecision) -> T,
    {
        let decision = self.require_authorization(request).await?;
        Ok(run(decision))
    }

    async fn get<T>(&self, path: &str) -> Result<T, NoetherClientError>
    where
        T: serde::de::DeserializeOwned,
    {
        let url = self.url.join(path)?;
        let response = self.client.get(url).send().await?;
        decode_response(response).await
    }

    async fn post<T, B>(&self, path: &str, body: &B) -> Result<T, NoetherClientError>
    where
        T: serde::de::DeserializeOwned,
        B: serde::Serialize + ?Sized,
    {
        let url = self.url.join(path)?;
        let response = self.client.post(url).json(body).send().await?;
        decode_response(response).await
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub decision_mode: String,
    pub policy_loaded: bool,
    pub upstream_configured: bool,
    pub route_count: u64,
}

async fn decode_response<T>(response: reqwest::Response) -> Result<T, NoetherClientError>
where
    T: serde::de::DeserializeOwned,
{
    let status = response.status();
    if !status.is_success() {
        return Err(NoetherClientError::Http {
            status,
            body: response.text().await?,
        });
    }
    response.json::<T>().await.map_err(NoetherClientError::from)
}

fn synthetic_decision(fail_mode: FailMode, error: &NoetherClientError) -> AuthorizeDecision {
    let (outcome, action, severity) = match fail_mode {
        FailMode::FailOpen => (
            DecisionOutcome::Allow,
            PolicyAction::Allow,
            DecisionSeverity::Warn,
        ),
        FailMode::FailClosed => (
            DecisionOutcome::Deny,
            PolicyAction::Block,
            DecisionSeverity::Deny,
        ),
    };
    AuthorizeDecision {
        decision_id: match fail_mode {
            FailMode::FailOpen => "sdk-fail_open".to_owned(),
            FailMode::FailClosed => "sdk-fail_closed".to_owned(),
        },
        outcome,
        action,
        reservation: None,
        explanations: vec![DecisionExplanation {
            rule_id: "sdk.sidecar_unavailable".to_owned(),
            reason: format!("Noether sidecar unavailable; applying {fail_mode:?}: {error}"),
            severity,
        }],
        created_at: chrono::Utc::now(),
    }
}

fn percent_encode_path_component(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(&mut encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;
    use axum::http::{Method, Uri};
    use axum::routing::any;
    use axum::{Json, Router};
    use noether::contract::{ReservationStatus, UsageObservation};
    use serde_json::json;
    use std::collections::BTreeMap;
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn client_calls_authorize_finalize_event_and_health_endpoints() {
        let server = TestServer::start().await;
        let client = NoetherClient::new(&server.url).expect("client");

        let decision = client
            .authorize(&AuthorizeRequest {
                project: Some("noether".to_owned()),
                subject: Some("user:local".to_owned()),
                provider: Some("openai".to_owned()),
                model: Some("gpt-4.1".to_owned()),
                ..authorize_request()
            })
            .await;
        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert_eq!(
            decision.reservation.as_ref().map(|reservation| reservation.id.as_str()),
            Some("reservation-1")
        );

        let reservation = client
            .finalize(
                "reservation-1",
                &FinalizeReservation {
                    actual_cost_usd: Some(0.10),
                    outcome: noether::contract::FinalizeOutcome::Success,
                    usage: Some(UsageObservation {
                        provider: Some("openai".to_owned()),
                        model: Some("gpt-4.1".to_owned()),
                        total_tokens: Some(1500),
                        input_tokens: None,
                        output_tokens: None,
                        cost_usd: None,
                        latency_ms: None,
                        stop_reason: None,
                    }),
                    reservation_id: None,
                    metadata: BTreeMap::new(),
                },
            )
            .await
            .expect("finalize");
        assert_eq!(reservation.status, ReservationStatus::Finalized);

        let event_response = client
            .event(&TraceEvent {
                kind: "tool.observed".to_owned(),
                payload: json!({"name":"bash"}),
                id: None,
                trace_id: None,
                occurred_at: None,
            })
            .await
            .expect("event");
        assert_eq!(event_response["accepted"], true);
        assert_eq!(client.health().await.expect("health").status, "ok");
    }

    #[tokio::test]
    async fn fail_modes_return_synthetic_decisions_when_sidecar_is_unavailable() {
        let fail_open = NoetherClient::new("http://127.0.0.1:9")
            .expect("client")
            .with_timeout(Duration::from_millis(50))
            .expect("timeout")
            .with_fail_mode(FailMode::FailOpen);
        let open_decision = fail_open.authorize(&authorize_request()).await;
        assert_eq!(open_decision.outcome, DecisionOutcome::Allow);
        assert_eq!(open_decision.action, PolicyAction::Allow);

        let fail_closed = NoetherClient::new("http://127.0.0.1:9")
            .expect("client")
            .with_timeout(Duration::from_millis(50))
            .expect("timeout");
        let closed_decision = fail_closed.authorize(&authorize_request()).await;
        assert_eq!(closed_decision.outcome, DecisionOutcome::Deny);
        assert_eq!(closed_decision.action, PolicyAction::Block);

        let result = fail_closed
            .with_decision(&authorize_request(), |_| "called")
            .await;
        assert!(matches!(result, Err(NoetherClientError::Denied(_, _))));
    }

    struct TestServer {
        url: String,
    }

    impl TestServer {
        async fn start() -> Self {
            let app = Router::new().fallback(any(
                |method: Method, uri: Uri, body: Bytes| async move {
                    let path = uri.path();
                    if method == Method::POST && path == "/v1/authorize" {
                        return Json(json!({
                            "decision_id":"decision-1",
                            "outcome":"allow",
                            "action":"allow",
                            "reservation":{
                                "id":"reservation-1",
                                "amount_usd":0.12,
                                "currency":"USD",
                                "status":"active",
                                "created_at":"2026-05-27T00:00:00Z",
                                "expires_at":"2026-05-27T01:00:00Z"
                            },
                            "explanations":[],
                            "created_at":"2026-05-27T00:00:00Z"
                        }));
                    }
                    if method == Method::POST && path == "/v1/reservations/reservation-1/finalize"
                    {
                        let value: serde_json::Value =
                            serde_json::from_slice(&body).expect("json body");
                        assert_eq!(value["actual_cost_usd"], 0.10);
                        return Json(json!({
                            "id":"reservation-1",
                            "amount_usd":0.10,
                            "currency":"USD",
                            "status":"finalized",
                            "created_at":"2026-05-27T00:00:00Z",
                            "expires_at":"2026-05-27T01:00:00Z"
                        }));
                    }
                    if method == Method::POST && path == "/v1/events" {
                        return Json(json!({"accepted":true}));
                    }
                    if method == Method::GET && path == "/health" {
                        return Json(json!({
                            "status":"ok",
                            "decision_mode":"dry_run",
                            "policy_loaded":true,
                            "upstream_configured":false,
                            "route_count":0
                        }));
                    }
                    Json(json!({"error":"not found"}))
                },
            ));
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let addr = listener.local_addr().expect("addr");
            tokio::spawn(async move {
                axum::serve(listener, app).await.expect("server");
            });
            Self {
                url: format!("http://{addr}"),
            }
        }
    }

    fn authorize_request() -> AuthorizeRequest {
        AuthorizeRequest {
            budget_id: None,
            entities: Vec::new(),
            subject: None,
            project: Some("noether".to_owned()),
            provider: None,
            model: None,
            estimated_tokens: None,
            estimated_cost_usd: None,
            metadata: BTreeMap::new(),
        }
    }
}
