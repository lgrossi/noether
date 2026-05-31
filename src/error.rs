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

    #[error("PostgreSQL TLS error: {0}")]
    PostgresTls(#[from] native_tls::Error),

    #[error("invalid upstream method: {0}")]
    Method(String),

    #[error("gateway timeout: {0}")]
    GatewayTimeout(String),

    #[error("too many requests: {0}")]
    TooManyRequests(String),

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
        let status = match &self {
            Self::InvalidPolicy(_) | Self::InvalidConfig(_) | Self::Json(_) | Self::Yaml(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::GatewayTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
            Self::Io(_)
            | Self::Upstream(_)
            | Self::Url(_)
            | Self::Method(_)
            | Self::Sqlite(_)
            | Self::Postgres(_)
            | Self::PostgresTls(_) => StatusCode::BAD_GATEWAY,
        };
        let message = match &self {
            Self::InvalidPolicy(_)
            | Self::InvalidConfig(_)
            | Self::Json(_)
            | Self::Yaml(_)
            | Self::NotFound(_)
            | Self::TooManyRequests(_)
            | Self::GatewayTimeout(_) => self.to_string(),
            Self::Upstream(error) if error.is_timeout() => "upstream request timed out".to_owned(),
            Self::Upstream(_) => "upstream request failed".to_owned(),
            Self::Io(_) => "I/O operation failed".to_owned(),
            Self::Url(_) => "invalid upstream URL".to_owned(),
            Self::Method(_) => "invalid upstream method".to_owned(),
            Self::Sqlite(_) => "SQLite operation failed".to_owned(),
            Self::Postgres(_) => "PostgreSQL operation failed".to_owned(),
            Self::PostgresTls(_) => "PostgreSQL TLS setup failed".to_owned(),
        };

        (status, json!({ "error": message }).to_string()).into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::response::IntoResponse;
    use serde_json::Value;

    use super::*;

    #[tokio::test]
    async fn gateway_timeout_maps_to_504() {
        let response =
            NoetError::GatewayTimeout("upstream was too slow".to_owned()).into_response();

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn database_errors_are_sanitized_for_clients() {
        let response = NoetError::Sqlite(rusqlite::Error::InvalidQuery).into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let payload: Value = serde_json::from_slice(&body).expect("json error response");

        assert_eq!(payload["error"], "SQLite operation failed");
    }
}
