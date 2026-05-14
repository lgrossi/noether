use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use reqwest::header::{HeaderMap as ReqwestHeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::fs;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use url::Url;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "noet")]
#[command(about = "Noether control sidecar tooling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the local capture server.
    Serve(ServeArgs),
}

#[derive(Parser)]
struct ServeArgs {
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1:4040")]
    bind: SocketAddr,

    /// Directory where redacted capture fixtures are written.
    #[arg(long, default_value = ".noet/fixtures")]
    fixture_dir: PathBuf,

    /// Optional upstream base URL. When omitted, Noether returns mock responses.
    #[arg(long)]
    upstream: Option<Url>,
}

#[derive(Clone)]
struct AppState {
    fixture_dir: PathBuf,
    upstream: Option<Url>,
    client: reqwest::Client,
}

struct ForwardContext {
    trace_id: String,
    method: Method,
    headers: HeaderMap,
    path: String,
    body: Bytes,
    request: CapturedRequest,
}

#[derive(Debug, Error)]
enum NoetError {
    #[error("failed to persist fixture: {0}")]
    Io(#[from] std::io::Error),

    #[error("upstream request failed: {0}")]
    Upstream(#[from] reqwest::Error),

    #[error("invalid upstream URL: {0}")]
    Url(#[from] url::ParseError),

    #[error("invalid upstream method: {0}")]
    Method(String),
}

impl IntoResponse for NoetError {
    fn into_response(self) -> Response {
        error!(error = %self, "request failed");
        let status = match self {
            Self::Io(_) | Self::Upstream(_) | Self::Url(_) | Self::Method(_) => {
                StatusCode::BAD_GATEWAY
            }
        };

        (status, json!({ "error": self.to_string() }).to_string()).into_response()
    }
}

#[derive(Serialize)]
struct CaptureFixture {
    schema: &'static str,
    trace_id: String,
    captured_at: DateTime<Utc>,
    request: CapturedRequest,
    response: CapturedResponse,
}

#[derive(Serialize)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: CapturedBody,
}

#[derive(Serialize)]
struct CapturedResponse {
    source: ResponseSource,
    status: u16,
    headers: BTreeMap<String, String>,
    body: CapturedBody,
    chunks: Vec<CapturedChunk>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ResponseSource {
    Mock,
    Upstream,
}

#[derive(Serialize)]
struct CapturedChunk {
    index: usize,
    bytes: usize,
    text: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CapturedBody {
    Json { value: Value },
    Text { value: String },
    Binary { bytes: usize },
    Empty,
}

#[derive(Deserialize)]
struct IncomingBody {
    #[serde(default)]
    stream: bool,
}

#[tokio::main]
async fn main() -> Result<(), NoetError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Serve(args) => serve(args).await,
    }
}

async fn serve(args: ServeArgs) -> Result<(), NoetError> {
    fs::create_dir_all(&args.fixture_dir).await?;

    let state = AppState {
        fixture_dir: args.fixture_dir,
        upstream: args.upstream,
        client: reqwest::Client::new(),
    };

    let app = Router::new()
        .route("/v1/chat/completions", any(capture))
        .route("/v1/messages", any(capture))
        .route("/v1/responses", any(capture))
        .route("/health", any(health))
        .fallback(any(capture))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    info!(bind = %args.bind, "starting noet capture server");
    let listener = TcpListener::bind(args.bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn capture(
    State(state): State<AppState>,
    OriginalUri(original_uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, NoetError> {
    let trace_id = Uuid::new_v4().to_string();
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

    match state.upstream.clone() {
        Some(upstream) => {
            let context = ForwardContext {
                trace_id,
                method,
                headers,
                path,
                body,
                request,
            };
            forward_upstream(state, upstream, context).await
        }
        None => mock_response(state, trace_id, original_uri, request).await,
    }
}

async fn forward_upstream(
    state: AppState,
    upstream: Url,
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
    let fixture = CaptureFixture {
        schema: "noether.capture.v1",
        trace_id: context.trace_id.clone(),
        captured_at: Utc::now(),
        request: context.request,
        response: CapturedResponse {
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
    };
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
    let fixture = CaptureFixture {
        schema: "noether.capture.v1",
        trace_id: trace_id.clone(),
        captured_at: Utc::now(),
        request,
        response: CapturedResponse {
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
    };
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

fn mock_json(path: &str) -> (StatusCode, &'static str, String) {
    if path == "/v1/messages" {
        (
            StatusCode::OK,
            "application/json",
            json!({
                "id": format!("msg_{}", Uuid::new_v4()),
                "type": "message",
                "role": "assistant",
                "model": "noether-mock",
                "content": [{ "type": "text", "text": "Noether mock response" }],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": { "input_tokens": 1, "output_tokens": 4 }
            })
            .to_string(),
        )
    } else {
        (
            StatusCode::OK,
            "application/json",
            json!({
                "id": format!("chatcmpl-{}", Uuid::new_v4()),
                "object": "chat.completion",
                "created": Utc::now().timestamp(),
                "model": "noether-mock",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "Noether mock response" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 1, "completion_tokens": 4, "total_tokens": 5 }
            })
            .to_string(),
        )
    }
}

fn mock_stream(path: &str) -> (StatusCode, &'static str, String) {
    if path == "/v1/messages" {
        (
            StatusCode::OK,
            "text/event-stream",
            [
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_mock\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"noether-mock\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Noether mock response\"}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ]
            .concat(),
        )
    } else {
        (
            StatusCode::OK,
            "text/event-stream",
            [
                "data: {\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\",\"model\":\"noether-mock\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Noether mock response\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-mock\",\"object\":\"chat.completion.chunk\",\"model\":\"noether-mock\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ]
            .concat(),
        )
    }
}

async fn persist_fixture(dir: &Path, fixture: &CaptureFixture) -> Result<(), NoetError> {
    fs::create_dir_all(dir).await?;
    let path = dir.join(format!("{}.json", fixture.trace_id));
    let bytes =
        serde_json::to_vec_pretty(fixture).expect("serializing capture fixture cannot fail");
    fs::write(path, bytes).await?;
    Ok(())
}

fn request_streamed(body: &CapturedBody) -> bool {
    match body {
        CapturedBody::Json { value } => serde_json::from_value::<IncomingBody>(value.clone())
            .map(|incoming| incoming.stream)
            .unwrap_or(false),
        CapturedBody::Text { .. } | CapturedBody::Binary { .. } | CapturedBody::Empty => false,
    }
}

fn capture_body(bytes: &[u8]) -> CapturedBody {
    if bytes.is_empty() {
        return CapturedBody::Empty;
    }

    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => CapturedBody::Json { value },
        Err(_) => match std::str::from_utf8(bytes) {
            Ok(value) => CapturedBody::Text {
                value: value.to_owned(),
            },
            Err(_) => CapturedBody::Binary { bytes: bytes.len() },
        },
    }
}

fn text_preview(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(|text| {
        if text.len() > 4_096 {
            format!("{}...", &text[..4_096])
        } else {
            text.to_owned()
        }
    })
}

fn redact_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            let value = if is_secret_header(&name) {
                "<redacted>".to_owned()
            } else {
                value.to_str().unwrap_or("<non-utf8>").to_owned()
            };
            (name, value)
        })
        .collect()
}

fn redact_reqwest_headers(headers: &ReqwestHeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            let value = if is_secret_header(&name) {
                "<redacted>".to_owned()
            } else {
                value.to_str().unwrap_or("<non-utf8>").to_owned()
            };
            (name, value)
        })
        .collect()
}

fn is_secret_header(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "proxy-authorization"
            | "x-api-key"
            | "api-key"
            | "anthropic-api-key"
            | "openai-api-key"
            | "cookie"
            | "set-cookie"
    ) || name.contains("token")
        || name.contains("secret")
        || name.contains("credential")
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

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn redacts_secret_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer sk-test"),
        );
        headers.insert("x-trace-id", HeaderValue::from_static("trace"));

        let redacted = redact_headers(&headers);

        assert_eq!(
            redacted.get("authorization"),
            Some(&"<redacted>".to_owned())
        );
        assert_eq!(redacted.get("x-trace-id"), Some(&"trace".to_owned()));
    }

    #[test]
    fn detects_streaming_json_requests() {
        let body = capture_body(br#"{"model":"x","stream":true}"#);

        assert!(request_streamed(&body));
    }

    #[test]
    fn captures_non_json_text() {
        let body = capture_body(b"hello");

        match body {
            CapturedBody::Text { value } => assert_eq!(value, "hello"),
            _ => panic!("expected text body"),
        }
    }
}
