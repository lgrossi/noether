use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::fs;

use crate::contract::{AuthorizeDecision, DecisionMode};
use crate::error::NoetError;
use crate::redaction::redact_json_value;

pub const CAPTURE_SCHEMA_V1: &str = "noether.capture.v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CaptureFixture {
    pub schema: String,
    pub trace_id: String,
    pub captured_at: DateTime<Utc>,
    pub request: CapturedRequest,
    pub response: CapturedResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<CapturedDecision>,
}

impl CaptureFixture {
    pub fn new(
        trace_id: String,
        request: CapturedRequest,
        response: CapturedResponse,
        decision: Option<CapturedDecision>,
    ) -> Self {
        Self {
            schema: CAPTURE_SCHEMA_V1.to_owned(),
            trace_id,
            captured_at: Utc::now(),
            request,
            response,
            decision,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: CapturedBody,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CapturedResponse {
    pub source: ResponseSource,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: CapturedBody,
    pub chunks: Vec<CapturedChunk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CapturedDecision {
    pub mode: DecisionMode,
    pub decision: AuthorizeDecision,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseSource {
    Mock,
    Upstream,
    DecisionDenied,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CapturedChunk {
    pub index: usize,
    pub bytes: usize,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapturedBody {
    Json { value: Value },
    Text { value: String },
    Binary { bytes: usize },
    Empty,
}

pub async fn persist_fixture(dir: &Path, fixture: &CaptureFixture) -> Result<PathBuf, NoetError> {
    fs::create_dir_all(dir).await?;
    let path = dir.join(format!("{}.json", fixture.trace_id));
    let bytes = serde_json::to_vec_pretty(fixture)?;
    fs::write(&path, bytes).await?;
    Ok(path)
}

pub async fn read_fixture(path: &Path) -> Result<CaptureFixture, NoetError> {
    let bytes = fs::read(path).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn list_fixture_paths(dir: &Path) -> Result<Vec<PathBuf>, NoetError> {
    let mut entries = fs::read_dir(dir).await?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub fn request_streamed(body: &CapturedBody) -> bool {
    match body {
        CapturedBody::Json { value } => value
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        CapturedBody::Text { .. } | CapturedBody::Binary { .. } | CapturedBody::Empty => false,
    }
}

pub fn capture_body(bytes: &[u8]) -> CapturedBody {
    if bytes.is_empty() {
        return CapturedBody::Empty;
    }

    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => CapturedBody::Json {
            value: redact_json_value(&value),
        },
        Err(_) => match std::str::from_utf8(bytes) {
            Ok(value) => CapturedBody::Text {
                value: value.to_owned(),
            },
            Err(_) => CapturedBody::Binary { bytes: bytes.len() },
        },
    }
}

pub fn text_preview(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(|text| {
        if text.len() > 4_096 {
            format!("{}...", &text[..4_096])
        } else {
            text.to_owned()
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fixture_schema_v1_round_trips() {
        let fixture = CaptureFixture::new(
            "trace-1".to_owned(),
            CapturedRequest {
                method: "POST".to_owned(),
                path: "/v1/chat/completions".to_owned(),
                headers: BTreeMap::new(),
                body: CapturedBody::Json {
                    value: json!({"model":"x"}),
                },
            },
            CapturedResponse {
                source: ResponseSource::Mock,
                status: 200,
                headers: BTreeMap::new(),
                body: CapturedBody::Text {
                    value: "ok".to_owned(),
                },
                chunks: Vec::new(),
                error: None,
            },
            None,
        );

        let encoded = serde_json::to_string(&fixture).expect("fixture serializes");
        let decoded: CaptureFixture = serde_json::from_str(&encoded).expect("fixture deserializes");

        assert_eq!(decoded.schema, CAPTURE_SCHEMA_V1);
        assert_eq!(decoded.request.path, "/v1/chat/completions");
        assert_eq!(decoded.response.source, ResponseSource::Mock);
    }

    #[test]
    fn captures_json_body_with_recursive_credential_redaction() {
        let body = capture_body(br#"{"prompt":"keep","api_key":"sk-test"}"#);

        match body {
            CapturedBody::Json { value } => {
                assert_eq!(value["prompt"], "keep");
                assert_eq!(value["api_key"], crate::redaction::REDACTED);
            }
            CapturedBody::Text { .. } | CapturedBody::Binary { .. } | CapturedBody::Empty => {
                panic!("expected JSON body")
            }
        }
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
            CapturedBody::Json { .. } | CapturedBody::Binary { .. } | CapturedBody::Empty => {
                panic!("expected text body")
            }
        }
    }
}
