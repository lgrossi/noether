use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::http::header::HeaderName;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::error::NoetError;
use crate::{noether_app, openapi};

use super::AppState;

pub(super) async fn report_updates_stream(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.report_updates.subscribe()).filter_map(|message| {
        let update = match message {
            Ok(update) => update,
            Err(_) => return None,
        };
        let data = serde_json::to_string(&update).ok()?;
        Some(Ok(Event::default().event("report-update").data(data)))
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

pub(super) async fn noether_app_html() -> impl IntoResponse {
    (
        [(
            HeaderName::from_static("content-security-policy"),
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'",
        )],
        Html(noether_app::app_shell()),
    )
}

pub(super) async fn noether_app_js() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/javascript; charset=utf-8")],
        noether_app::app_js(),
    )
}

pub(super) async fn noether_app_css() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/css; charset=utf-8")],
        noether_app::app_css(),
    )
}

pub(super) async fn noether_app_logo() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        noether_app::logo_svg(),
    )
}

pub(super) async fn noether_app_favicon() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        noether_app::favicon_svg(),
    )
}

pub(super) async fn openapi_json() -> Result<impl IntoResponse, NoetError> {
    openapi::openapi_json_response()
}

pub(super) async fn api_docs() -> impl IntoResponse {
    openapi::api_docs_html()
}

pub(super) async fn deprecated_dashboard_surface() -> impl IntoResponse {
    (
        StatusCode::GONE,
        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        "The old Noether dashboard has been removed. Use /policy, /runs, or /replay.",
    )
}
