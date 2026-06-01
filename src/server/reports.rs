use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use serde::Deserialize;

use crate::error::NoetError;
use crate::reporting;

use super::AppState;

#[derive(Debug, Default, Deserialize)]
pub(super) struct ReportQuery {
    kind: Option<String>,
    trace: Option<String>,
}

pub(super) async fn report_usage(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, NoetError> {
    state
        .read_ledger(|ledger| Ok(Json(reporting::usage_report_value(ledger)?)))
        .await
}

pub(super) async fn report_decisions(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, NoetError> {
    state
        .read_ledger(|ledger| Ok(Json(reporting::decisions_report_value(ledger)?)))
        .await
}

pub(super) async fn report_trace(
    State(state): State<AppState>,
    AxumPath(trace_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, NoetError> {
    state
        .read_ledger(move |ledger| Ok(Json(reporting::trace_report_value(ledger, &trace_id)?)))
        .await
}

pub(super) async fn report_observations(
    State(state): State<AppState>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<serde_json::Value>, NoetError> {
    state
        .read_ledger(move |ledger| {
            Ok(Json(reporting::observations_report_value(
                ledger,
                query.kind.as_deref(),
                query.trace.as_deref(),
            )?))
        })
        .await
}

pub(super) async fn report_approval_audit(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, NoetError> {
    state
        .read_ledger(|ledger| Ok(Json(reporting::approval_audit_report_value(ledger)?)))
        .await
}
