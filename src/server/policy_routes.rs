use axum::Json;
use axum::extract::{Path as AxumPath, State};
use tokio::fs;

use crate::error::NoetError;
use crate::policy_workbench::{
    AppPolicyEnforceRequest, AppPolicyResponse, AppPolicyRollbackResponse,
    app_display_policy_source, app_policy_proposal, app_policy_suggestions, app_rule_stats,
    app_rule_stats_from_report, apply_suggestion_to_policy_source,
};
use crate::reporting;

use super::{
    AppPolicyApplyResponse, AppPolicyUpdate, AppState, ReportUpdate, append_policy_audit,
    policy_previous_path, write_previous_policy_snapshot,
};

pub(super) async fn app_policy(
    State(state): State<AppState>,
) -> Result<Json<AppPolicyResponse>, NoetError> {
    let Some((path, _, policy)) = state.active_policy_source().await else {
        return Err(NoetError::NotFound("no active policy".to_owned()));
    };
    let report = state
        .read_ledger(|ledger| ledger.rule_stats_report())
        .await?;
    let rule_stats = app_rule_stats_from_report(policy.as_ref(), report);
    let suggestions = app_policy_suggestions(&rule_stats);
    let proposal = app_policy_proposal(&state.policy_proposal_path).await?;
    let source = app_display_policy_source(policy.as_ref())?;
    let reload_error = state.policy_reload_error().await;
    Ok(Json(AppPolicyResponse {
        path: path.map(|path| path.display().to_string()),
        source,
        policy: policy.as_ref().clone(),
        decision_mode: state.decision_mode,
        status: if reload_error.is_some() {
            "reload_error".to_owned()
        } else {
            "ok".to_owned()
        },
        reload_error,
        rule_stats,
        suggestions,
        proposal,
    }))
}

pub(super) async fn update_app_policy_proposal(
    State(state): State<AppState>,
    Json(update): Json<AppPolicyUpdate>,
) -> Result<Json<AppPolicyResponse>, NoetError> {
    crate::policy::parse_policy_bytes(update.source.as_bytes())?;
    if let Some(parent) = state.policy_proposal_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&state.policy_proposal_path, update.source.as_bytes()).await?;
    let response = app_policy(State(state.clone())).await?;
    let _ = state.report_updates.send(ReportUpdate {
        kind: "policy_proposal",
        trace_id: None,
    });
    Ok(response)
}

pub(super) async fn discard_app_policy_proposal(
    State(state): State<AppState>,
) -> Result<Json<AppPolicyResponse>, NoetError> {
    match fs::remove_file(&state.policy_proposal_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let response = app_policy(State(state.clone())).await?;
    let _ = state.report_updates.send(ReportUpdate {
        kind: "policy_proposal",
        trace_id: None,
    });
    Ok(response)
}

pub(super) async fn apply_app_policy_suggestion(
    State(state): State<AppState>,
    AxumPath(suggestion_id): AxumPath<String>,
) -> Result<Json<AppPolicyApplyResponse>, NoetError> {
    let Some((_, active_source, policy)) = state.active_policy_source().await else {
        return Err(NoetError::NotFound("no active policy".to_owned()));
    };
    let decisions = state.read_ledger(reporting::decisions_report).await?;
    let stats = app_rule_stats(&policy, &decisions);
    let suggestions = app_policy_suggestions(&stats);
    let suggestion = suggestions
        .iter()
        .find(|suggestion| suggestion.id == suggestion_id)
        .ok_or_else(|| NoetError::NotFound(format!("suggestion {suggestion_id}")))?;
    let source = app_policy_proposal(&state.policy_proposal_path)
        .await?
        .map(|proposal| proposal.source)
        .unwrap_or(active_source);
    let updated_source = apply_suggestion_to_policy_source(&source, suggestion)?;
    if let Some(parent) = state.policy_proposal_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&state.policy_proposal_path, updated_source.as_bytes()).await?;
    let policy = app_policy(State(state.clone())).await?.0;
    Ok(Json(AppPolicyApplyResponse {
        policy,
        applied: suggestion.title.clone(),
    }))
}

pub(super) async fn enforce_app_policy_proposal(
    State(state): State<AppState>,
    request: Option<Json<AppPolicyEnforceRequest>>,
) -> Result<Json<AppPolicyResponse>, NoetError> {
    let source = match fs::read_to_string(&state.policy_proposal_path).await {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(NoetError::NotFound("no policy proposal saved".to_owned()));
        }
        Err(error) => return Err(error.into()),
    };
    if !request
        .as_ref()
        .map(|request| request.confirm_replay)
        .unwrap_or(false)
    {
        return Err(NoetError::InvalidPolicy(
            "policy enforce requires confirm_replay=true after reviewing replay".to_owned(),
        ));
    }
    if let Some((_, active_source, _)) = state.active_policy_source().await {
        if active_source == source {
            return Err(NoetError::InvalidPolicy(
                "policy proposal matches active policy; nothing to enforce".to_owned(),
            ));
        }
        write_previous_policy_snapshot(&state, &active_source).await?;
        append_policy_audit(&state, "enforce", "saved draft promoted to active policy").await?;
    }
    state.update_policy_source(source).await?;
    match fs::remove_file(&state.policy_proposal_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let response = app_policy(State(state.clone())).await?;
    let _ = state.report_updates.send(ReportUpdate {
        kind: "policy",
        trace_id: None,
    });
    Ok(response)
}

pub(super) async fn rollback_app_policy(
    State(state): State<AppState>,
) -> Result<Json<AppPolicyRollbackResponse>, NoetError> {
    let previous_path = policy_previous_path(&state);
    let source = match fs::read_to_string(&previous_path).await {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(NoetError::NotFound(
                "no previous policy snapshot saved".to_owned(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    state.update_policy_source(source).await?;
    append_policy_audit(&state, "rollback", "previous policy snapshot restored").await?;
    let policy = app_policy(State(state.clone())).await?.0;
    let _ = state.report_updates.send(ReportUpdate {
        kind: "policy",
        trace_id: None,
    });
    Ok(Json(AppPolicyRollbackResponse {
        policy,
        restored_from: previous_path.display().to_string(),
    }))
}
