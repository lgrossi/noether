use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use thiserror::Error;
use tracing::error;

#[derive(Debug, Error)]
pub enum NoetError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("upstream request failed: {0}")]
    Upstream(#[from] reqwest::Error),

    #[error("invalid upstream URL: {0}")]
    Url(#[from] url::ParseError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("PostgreSQL error: {0}")]
    Postgres(#[from] postgres::Error),

    #[error("invalid upstream method: {0}")]
    Method(String),

    #[error("invalid policy: {0}")]
    InvalidPolicy(String),

    #[error("invalid config: {0}")]
    InvalidConfig(String),

    #[error("not found: {0}")]
    NotFound(String),
}

impl IntoResponse for NoetError {
    fn into_response(self) -> Response {
        error!(error = %self, "request failed");
        let status = match self {
            Self::InvalidPolicy(_) | Self::InvalidConfig(_) | Self::Json(_) | Self::Yaml(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Io(_)
            | Self::Upstream(_)
            | Self::Url(_)
            | Self::Method(_)
            | Self::Sqlite(_)
            | Self::Postgres(_) => StatusCode::BAD_GATEWAY,
        };

        (status, json!({ "error": self.to_string() }).to_string()).into_response()
    }
}
