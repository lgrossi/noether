use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use uuid::Uuid;

use crate::error::NoetError;
use crate::policy_workbench::{
    app_display_policy_source, app_policy_proposal, app_run_totals_from_report,
};
use crate::replay_workbench::{
    AppReplayJob, AppReplayJobResponse, AppReplayResponse, ReplayScopeOptions, app_replay_proposal,
    app_replay_spend_seeds, string_metadata_value,
};

use super::app_runs::app_usage_by_agent_run;
use super::{
    APP_REPLAY_CHANGED_RUNS_CAP, APP_REPLAY_HISTORY_WINDOW_DAYS, APP_REPLAY_JOB_RETENTION_MINUTES,
    APP_REPLAY_MAX_JOBS, APP_REPLAY_PREVIEW_REQUEST_CAP, AppState,
};

pub(super) async fn app_replay(
    State(state): State<AppState>,
) -> Result<Json<AppReplayResponse>, NoetError> {
    let history_window_end = chrono::Utc::now();
    let history_window_start =
        history_window_end - chrono::Duration::days(APP_REPLAY_HISTORY_WINDOW_DAYS);
    let proposal = app_policy_proposal(&state.policy_proposal_path).await?;
    let has_proposed_policy = proposal.is_some();
    let proposed_policy = proposal
        .as_ref()
        .map(|proposal| crate::policy::parse_policy_bytes(proposal.source.as_bytes()))
        .transpose()?;
    let active_source = state
        .active_policy_source()
        .await
        .map(|(_, _, policy)| app_display_policy_source(policy.as_ref()))
        .transpose()?
        .unwrap_or_default();
    if !has_proposed_policy {
        let baseline = state
            .read_ledger(move |ledger| {
                Ok(app_run_totals_from_report(
                    ledger.run_totals_report_since(Some(history_window_start))?,
                ))
            })
            .await?;
        return Ok(Json(AppReplayResponse {
            baseline,
            has_proposed_policy,
            message: "No proposed policy has been saved for replay yet. Edit Policy first to create a local draft without enforcing it.".to_owned(),
            history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
            history_window_start,
            history_window_end,
            proposal: None,
        }));
    }
    let (total_requests, historical_requests, usage_by_agent_run, baseline, spend_seeds) = state
        .read_ledger(move |ledger| {
            let total_requests =
                ledger.historical_authorize_request_count_since(Some(history_window_start))?;
            let historical_requests = ledger.latest_historical_authorize_requests_since(
                Some(history_window_start),
                APP_REPLAY_PREVIEW_REQUEST_CAP,
            )?;
            let spend_seeds = historical_requests
                .first()
                .zip(proposed_policy.as_ref())
                .map(|(first, policy)| {
                    app_replay_spend_seeds(ledger, policy, history_window_start, first.occurred_at)
                })
                .transpose()?
                .unwrap_or_default();
            let agent_run_ids = historical_requests
                .iter()
                .filter_map(|request| string_metadata_value(&request.request, "agent_run_id"))
                .collect::<Vec<_>>();
            let usage_by_agent_run = app_usage_by_agent_run(
                &ledger.usage_activity_report_for_agent_runs(&agent_run_ids)?,
            );
            let baseline = app_run_totals_from_report(
                ledger.run_totals_report_since(Some(history_window_start))?,
            );
            Ok((
                total_requests,
                historical_requests,
                usage_by_agent_run,
                baseline,
                spend_seeds,
            ))
        })
        .await?;
    let replay_proposal = proposal
        .as_ref()
        .map(|proposal| {
            app_replay_proposal(
                &active_source,
                proposal,
                &historical_requests,
                &usage_by_agent_run,
                &spend_seeds,
                ReplayScopeOptions {
                    mode: "preview".to_owned(),
                    request_cap: Some(APP_REPLAY_PREVIEW_REQUEST_CAP),
                    total_requests_in_window: total_requests,
                    full_replay_available: total_requests > historical_requests.len(),
                    changed_runs_cap: APP_REPLAY_CHANGED_RUNS_CAP,
                    window_seeded: !spend_seeds.is_empty(),
                },
            )
        })
        .transpose()?;
    Ok(Json(AppReplayResponse {
        baseline,
        has_proposed_policy,
        message: if has_proposed_policy {
            "A valid proposed policy is saved locally. Preview replay re-evaluated the most recent recorded authorizations in the 30-day window."
        } else {
            "No proposed policy has been saved for replay yet. Edit Policy first to create a local draft without enforcing it."
        }
        .to_owned(),
        history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
        history_window_start,
        history_window_end,
        proposal: replay_proposal,
    }))
}

pub(super) async fn start_app_replay_job(
    State(state): State<AppState>,
) -> Result<Json<AppReplayJobResponse>, NoetError> {
    if app_policy_proposal(&state.policy_proposal_path)
        .await?
        .is_none()
    {
        return Err(NoetError::InvalidPolicy(
            "full replay requires a saved proposed policy".to_owned(),
        ));
    }
    let id = Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now();
    {
        let mut jobs = state.replay_jobs.lock().await;
        prune_replay_jobs(&mut jobs, created_at);
        if jobs.values().any(|job| job.status == "running") {
            return Err(NoetError::TooManyRequests(
                "a full replay job is already running".to_owned(),
            ));
        }
        if jobs.len() >= APP_REPLAY_MAX_JOBS {
            return Err(NoetError::TooManyRequests(format!(
                "at most {APP_REPLAY_MAX_JOBS} replay jobs are retained"
            )));
        }
        jobs.insert(
            id.clone(),
            AppReplayJob {
                status: "running".to_owned(),
                history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
                created_at,
                completed_at: None,
                error: None,
                result: None,
            },
        );
    }
    let jobs = state.replay_jobs.clone();
    let replay_state = state.clone();
    let job_id = id.clone();
    tokio::spawn(async move {
        let result = app_replay_full_month_response(replay_state).await;
        let mut jobs = jobs.lock().await;
        if let Some(job) = jobs.get_mut(&job_id) {
            job.completed_at = Some(chrono::Utc::now());
            match result {
                Ok(result) => {
                    job.status = "completed".to_owned();
                    job.result = Some(result);
                }
                Err(error) => {
                    job.status = "failed".to_owned();
                    job.error = Some(error.to_string());
                }
            }
        }
    });
    let jobs = state.replay_jobs.lock().await;
    let job = jobs
        .get(&id)
        .expect("job was inserted before response")
        .clone();
    Ok(Json(app_replay_job_response(id, job)))
}

fn prune_replay_jobs(
    jobs: &mut BTreeMap<String, AppReplayJob>,
    now: chrono::DateTime<chrono::Utc>,
) {
    let retention = chrono::Duration::minutes(APP_REPLAY_JOB_RETENTION_MINUTES);
    jobs.retain(|_, job| {
        job.status == "running"
            || job
                .completed_at
                .map(|completed_at| now - completed_at < retention)
                .unwrap_or(true)
    });
}

pub(super) async fn app_replay_job(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<AppReplayJobResponse>, NoetError> {
    let jobs = state.replay_jobs.lock().await;
    let job = jobs
        .get(&job_id)
        .cloned()
        .ok_or_else(|| NoetError::NotFound(format!("replay job {job_id}")))?;
    Ok(Json(app_replay_job_response(job_id, job)))
}

fn app_replay_job_response(id: String, job: AppReplayJob) -> AppReplayJobResponse {
    AppReplayJobResponse {
        id,
        status: job.status,
        history_window_days: job.history_window_days,
        created_at: job.created_at,
        completed_at: job.completed_at,
        error: job.error,
        result: job.result,
    }
}

async fn app_replay_full_month_response(state: AppState) -> Result<AppReplayResponse, NoetError> {
    let history_window_end = chrono::Utc::now();
    let history_window_start =
        history_window_end - chrono::Duration::days(APP_REPLAY_HISTORY_WINDOW_DAYS);
    let proposal = app_policy_proposal(&state.policy_proposal_path)
        .await?
        .ok_or_else(|| {
            NoetError::InvalidPolicy("full replay requires a saved proposed policy".to_owned())
        })?;
    let active_source = state
        .active_policy_source()
        .await
        .map(|(_, _, policy)| app_display_policy_source(policy.as_ref()))
        .transpose()?
        .unwrap_or_default();
    let (total_requests, historical_requests, usage_by_agent_run, baseline) = state
        .read_ledger(move |ledger| {
            let total_requests =
                ledger.historical_authorize_request_count_since(Some(history_window_start))?;
            let historical_requests =
                ledger.historical_authorize_requests_since(Some(history_window_start))?;
            let usage_by_agent_run = app_usage_by_agent_run(
                &ledger.usage_activity_report_since(Some(history_window_start))?,
            );
            let baseline = app_run_totals_from_report(
                ledger.run_totals_report_since(Some(history_window_start))?,
            );
            Ok((
                total_requests,
                historical_requests,
                usage_by_agent_run,
                baseline,
            ))
        })
        .await?;
    let proposal = app_replay_proposal(
        &active_source,
        &proposal,
        &historical_requests,
        &usage_by_agent_run,
        &[],
        ReplayScopeOptions {
            mode: "full_month".to_owned(),
            request_cap: None,
            total_requests_in_window: total_requests,
            full_replay_available: false,
            changed_runs_cap: APP_REPLAY_CHANGED_RUNS_CAP,
            window_seeded: false,
        },
    )?;
    Ok(AppReplayResponse {
        baseline,
        has_proposed_policy: true,
        message: "Full 30-day replay completed against the saved draft policy.".to_owned(),
        history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
        history_window_start,
        history_window_end,
        proposal: Some(proposal),
    })
}
