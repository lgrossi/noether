use std::collections::BTreeMap;

use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use reqwest::header::{HeaderMap as ReqwestHeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use crate::contract::{AuthorizeRequest, DecisionMode, DecisionOutcome};
use crate::error::NoetError;
use crate::fixture::{
    CaptureFixture, CapturedChunk, CapturedDecision, CapturedRequest, CapturedResponse,
    ResponseSource, capture_body, persist_fixture, request_streamed, text_preview,
};
use crate::mock::{mock_json, mock_stream};
use crate::redaction::{redact_headers, redact_reqwest_headers};
use crate::server::AppState;

struct ForwardContext {
    trace_id: String,
    method: Method,
    headers: HeaderMap,
    path: String,
    body: Bytes,
    request: CapturedRequest,
    decision: Option<CapturedDecision>,
}

pub async fn capture(
    State(state): State<AppState>,
    OriginalUri(original_uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, NoetError> {
    let trace_id = uuid::Uuid::new_v4().to_string();
    let path = original_uri
        .path_and_query()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_else(|| original_uri.path().to_owned());

    let request = CapturedRequest {
        method: method.to_string(),
        path: path.clone(),
        headers: redact_headers(&headers),
        body: capture_body(&body),
    };
    let decision = evaluate_capture_decision(&state, &request).await;

    if state.decision_mode == DecisionMode::Enforce
        && decision
            .as_ref()
            .is_some_and(|decision| decision.decision.outcome == DecisionOutcome::Deny)
    {
        return deny_capture(state, trace_id, request, decision).await;
    }

    match state.upstream.clone() {
        Some(upstream) => {
            let context = ForwardContext {
                trace_id,
                method,
                headers,
                path,
                body,
                request,
                decision,
            };
            forward_upstream(state, upstream, context).await
        }
        None => mock_response(state, trace_id, original_uri, request, decision).await,
    }
}

async fn evaluate_capture_decision(
    state: &AppState,
    request: &CapturedRequest,
) -> Option<CapturedDecision> {
    let policy = state.policy.as_ref()?;
    let authorize_request = authorize_request_from_capture(request);
    let decision = state
        .ledger
        .lock()
        .await
        .authorize(Some(policy.as_ref()), &authorize_request);
    Some(CapturedDecision {
        mode: state.decision_mode,
        decision,
    })
}

async fn deny_capture(
    state: AppState,
    trace_id: String,
    request: CapturedRequest,
    decision: Option<CapturedDecision>,
) -> Result<Response, NoetError> {
    let body = json!({
        "error": "request denied by Noether policy",
        "trace_id": trace_id,
        "decision": decision.as_ref().map(|decision| &decision.decision),
    })
    .to_string();
    let body_bytes = Bytes::from(body);
    let fixture = CaptureFixture::new(
        trace_id.clone(),
        request,
        CapturedResponse {
            source: ResponseSource::DecisionDenied,
            status: StatusCode::FORBIDDEN.as_u16(),
            headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
            body: capture_body(&body_bytes),
            chunks: Vec::new(),
        },
        decision,
    );
    persist_fixture(&state.fixture_dir, &fixture).await?;

    let mut response = (StatusCode::FORBIDDEN, body_bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        "x-noet-trace-id",
        axum::http::HeaderValue::from_str(&trace_id)
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("invalid")),
    );
    Ok(response)
}

async fn forward_upstream(
    state: AppState,
    upstream: url::Url,
    context: ForwardContext,
) -> Result<Response, NoetError> {
    let target = upstream.join(context.path.trim_start_matches('/'))?;
    let reqwest_method = reqwest::Method::from_bytes(context.method.as_str().as_bytes())
        .map_err(|error| NoetError::Method(error.to_string()))?;
    let upstream_headers = upstream_headers(&context.headers);
    let upstream_response = state
        .client
        .request(reqwest_method, target)
        .headers(upstream_headers)
        .body(context.body)
        .send()
        .await?;

    let status = upstream_response.status();
    let response_headers = upstream_response.headers().clone();
    let response_status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let captured_headers = redact_reqwest_headers(&response_headers);
    let public_headers = response_headers_for_client(&response_headers);
    let response_bytes = upstream_response.bytes().await?;
    let fixture = CaptureFixture::new(
        context.trace_id.clone(),
        context.request,
        CapturedResponse {
            source: ResponseSource::Upstream,
            status: response_status.as_u16(),
            headers: captured_headers,
            body: capture_body(&response_bytes),
            chunks: vec![CapturedChunk {
                index: 0,
                bytes: response_bytes.len(),
                text: text_preview(&response_bytes),
            }],
        },
        context.decision,
    );
    persist_fixture(&state.fixture_dir, &fixture).await?;

    let mut response = Response::new(Body::from(response_bytes));
    *response.status_mut() = response_status;
    *response.headers_mut() = public_headers;
    response.headers_mut().insert(
        "x-noet-trace-id",
        axum::http::HeaderValue::from_str(&context.trace_id)
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("invalid")),
    );

    Ok(response)
}

async fn mock_response(
    state: AppState,
    trace_id: String,
    uri: Uri,
    request: CapturedRequest,
    decision: Option<CapturedDecision>,
) -> Result<Response, NoetError> {
    let stream_requested = request_streamed(&request.body);
    let path = uri.path();
    let (status, content_type, body) = if stream_requested {
        mock_stream(path)
    } else {
        mock_json(path)
    };
    let body_bytes = Bytes::from(body);
    let response_headers = BTreeMap::from([("content-type".to_owned(), content_type.to_owned())]);
    let fixture = CaptureFixture::new(
        trace_id.clone(),
        request,
        CapturedResponse {
            source: ResponseSource::Mock,
            status: status.as_u16(),
            headers: response_headers,
            body: capture_body(&body_bytes),
            chunks: vec![CapturedChunk {
                index: 0,
                bytes: body_bytes.len(),
                text: text_preview(&body_bytes),
            }],
        },
        decision,
    );
    persist_fixture(&state.fixture_dir, &fixture).await?;

    let mut response = (status, body_bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(content_type),
    );
    response.headers_mut().insert(
        "x-noet-trace-id",
        axum::http::HeaderValue::from_str(&trace_id)
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("invalid")),
    );
    Ok(response)
}

fn authorize_request_from_capture(request: &CapturedRequest) -> AuthorizeRequest {
    let body = match &request.body {
        crate::fixture::CapturedBody::Json { value } => Some(value),
        crate::fixture::CapturedBody::Text { .. }
        | crate::fixture::CapturedBody::Binary { .. }
        | crate::fixture::CapturedBody::Empty => None,
    };

    let metadata = body
        .and_then(|body| body.get("metadata"))
        .and_then(Value::as_object)
        .map(|metadata| {
            metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();

    AuthorizeRequest {
        subject: request
            .headers
            .get("x-noet-subject")
            .cloned()
            .or_else(|| string_at(body, &["metadata", "subject"])),
        project: request
            .headers
            .get("x-noet-project")
            .cloned()
            .or_else(|| string_at(body, &["metadata", "project"])),
        provider: request
            .headers
            .get("x-noet-provider")
            .cloned()
            .or_else(|| string_at(body, &["provider"])),
        model: string_at(body, &["model"]),
        estimated_tokens: u64_at(body, &["estimated_tokens"])
            .or_else(|| u64_at(body, &["max_tokens"]))
            .or_else(|| u64_at(body, &["max_completion_tokens"])),
        estimated_cost_usd: f64_at(body, &["estimated_cost_usd"]),
        metadata,
    }
}

fn string_at(value: Option<&Value>, path: &[&str]) -> Option<String> {
    value
        .and_then(|value| {
            path.iter()
                .try_fold(value, |current, key| current.get(*key))
        })
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn u64_at(value: Option<&Value>, path: &[&str]) -> Option<u64> {
    value
        .and_then(|value| {
            path.iter()
                .try_fold(value, |current, key| current.get(*key))
        })
        .and_then(Value::as_u64)
}

fn f64_at(value: Option<&Value>, path: &[&str]) -> Option<f64> {
    value
        .and_then(|value| {
            path.iter()
                .try_fold(value, |current, key| current.get(*key))
        })
        .and_then(Value::as_f64)
}

fn upstream_headers(headers: &HeaderMap) -> ReqwestHeaderMap {
    let mut upstream_headers = ReqwestHeaderMap::new();
    for (name, value) in headers {
        if hop_by_hop_header(name.as_str()) || name == header::HOST {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            upstream_headers.insert(name, value);
        }
    }
    upstream_headers
}

fn response_headers_for_client(headers: &ReqwestHeaderMap) -> HeaderMap {
    let mut client_headers = HeaderMap::new();
    for (name, value) in headers {
        if hop_by_hop_header(name.as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(name.as_str().as_bytes()),
            axum::http::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            client_headers.insert(name, value);
        }
    }
    client_headers
}

fn hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}
