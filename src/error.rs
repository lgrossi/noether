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

    #[error("database error: {0}")]
    Database(String),

    #[error("invalid upstream method: {0}")]
    Method(String),

    #[error("invalid policy: {0}")]
    InvalidPolicy(String),

    #[error("invalid config: {0}")]
    InvalidConfig(String),

    #[error("not found: {0}")]
    NotFound(String),
}

impl From<rusqlite::Error> for NoetError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<deadpool_postgres::PoolError> for NoetError {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<tokio_postgres::Error> for NoetError {
    fn from(e: tokio_postgres::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl IntoResponse for NoetError {
    fn into_response(self) -> Response {
        error!(error = %self, "request failed");
        let status = match self {
            Self::InvalidPolicy(_) | Self::InvalidConfig(_) | Self::Json(_) | Self::Yaml(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Io(_) | Self::Upstream(_) | Self::Url(_) | Self::Method(_) | Self::Database(_) => {
                StatusCode::BAD_GATEWAY
            }
        };

        (status, json!({ "error": self.to_string() }).to_string()).into_response()
    }
}
