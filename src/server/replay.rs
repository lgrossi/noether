use std::collections::BTreeMap;
use std::path::Path;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::contract::SpendWindowMode;
use crate::error::NoetError;
use crate::ledger::{BudgetLedger, ReplaySpendSeed};
use crate::policy_workbench::{
    AppPolicyProposal, app_display_policy_source, app_policy_proposal, app_run_totals_from_report,
};
use crate::replay_workbench::{
    AppReplayJob, AppReplayJobResponse, AppReplayJobStatus, AppReplayResponse, AppReplaySnapshot,
    ReplayScopeOptions, app_replay_proposal,
};

use super::app_runs::app_usage_by_agent_run;
use super::{
    APP_REPLAY_CHANGED_RUNS_CAP, APP_REPLAY_HISTORY_WINDOW_DAYS, APP_REPLAY_JOB_RETENTION_MINUTES,
    APP_REPLAY_MAX_JOBS, AppState,
};

pub(super) async fn app_replay(
    State(state): State<AppState>,
) -> Result<Json<AppReplayResponse>, NoetError> {
    let history_window_end = chrono::Utc::now();
    let history_window_start =
        history_window_end - chrono::Duration::days(APP_REPLAY_HISTORY_WINDOW_DAYS);
    let proposal = app_policy_proposal(&state.policy_proposal_path).await?;
    let has_proposed_policy = proposal.is_some();
    let active_source = state
        .active_policy_source()
        .await
        .map(|(_, _, policy)| app_display_policy_source(policy.as_ref()))
        .transpose()?
        .unwrap_or_default();
    let active_hash = policy_hash(&active_source);
    let proposed_hash = proposal
        .as_ref()
        .map(|proposal| policy_hash(&proposal.source));
    let snapshots = load_replay_snapshots(
        &state.replay_snapshots_path,
        &active_hash,
        proposed_hash.as_deref(),
    )
    .await?;
    let latest_job =
        latest_replay_job_matching_policy(&state, &active_hash, proposed_hash.as_deref()).await;
    if let Some((job_id, job)) = latest_job.as_ref() {
        let job_is_fresh = if let Some(result) = job.result.as_ref() {
            !replay_ledger_changed_since(&state, result.history_window_end).await?
        } else {
            false
        };
        if job.status == "completed"
            && job_is_fresh
            && replay_job_matches_policy(job, &active_hash, proposed_hash.as_deref())
            && let Some(mut result) = job.result.clone()
        {
            result.current_job = Some(app_replay_job_status(job_id.clone(), job));
            result.snapshots = snapshots;
            return Ok(Json(result));
        }
    }
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
            current_job: latest_job
                .as_ref()
                .map(|(job_id, job)| app_replay_job_status(job_id.clone(), job)),
            snapshots,
            proposal: None,
        }));
    }
    let baseline = state
        .read_ledger(move |ledger| {
            Ok(app_run_totals_from_report(
                ledger.run_totals_report_since(Some(history_window_start))?,
            ))
        })
        .await?;
    Ok(Json(AppReplayResponse {
        baseline,
        has_proposed_policy,
        message: "A saved draft policy is ready. Run replay to compare recorded history against it in the background.".to_owned(),
        history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
        history_window_start,
        history_window_end,
        current_job: latest_job
            .as_ref()
            .map(|(job_id, job)| app_replay_job_status(job_id.clone(), job)),
        snapshots,
        proposal: None,
    }))
}

pub(super) async fn start_app_replay_job(
    State(state): State<AppState>,
) -> Result<Json<AppReplayJobResponse>, NoetError> {
    let proposal = if let Some(proposal) = app_policy_proposal(&state.policy_proposal_path).await? {
        proposal
    } else {
        return Err(NoetError::InvalidPolicy(
            "full replay requires a saved proposed policy".to_owned(),
        ));
    };
    let active_policy = state.active_policy_source().await;
    let (active_policy_path, active_source) =
        if let Some((path, _, policy)) = active_policy.as_ref() {
            (
                path.as_ref().map(|path| path.display().to_string()),
                app_display_policy_source(policy.as_ref())?,
            )
        } else {
            (None, String::new())
        };
    let active_policy_hash = policy_hash(&active_source);
    let proposed_policy_hash = policy_hash(&proposal.source);
    let proposed_policy_path = proposal.path.clone();
    let id = Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now();
    {
        let mut jobs = state.replay_jobs.lock().await;
        prune_replay_jobs(&mut jobs, created_at);
        if jobs.values().any(|job| {
            job.status == "running"
                && replay_job_matches_policy_hashes(
                    job,
                    &active_policy_hash,
                    Some(&proposed_policy_hash),
                )
        }) {
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
                active_policy_hash: active_policy_hash.clone(),
                proposed_policy_hash: proposed_policy_hash.clone(),
                error: None,
                result: None,
                snapshot: None,
            },
        );
    }
    let jobs = state.replay_jobs.clone();
    let snapshots_path = state.replay_snapshots_path.clone();
    let replay_state = state.clone();
    let replay_active_source = active_source.clone();
    let replay_proposal = proposal.clone();
    let job_id = id.clone();
    tokio::spawn(async move {
        let result =
            app_replay_full_month_response(replay_state, replay_active_source, replay_proposal)
                .await;
        let completed_at = chrono::Utc::now();
        let snapshot_result = if let Ok(result) = &result {
            let context = ReplaySnapshotContext {
                id: &job_id,
                created_at,
                completed_at,
                active_policy_hash: &active_policy_hash,
                proposed_policy_hash: &proposed_policy_hash,
                active_policy_path: active_policy_path.as_deref(),
                proposed_policy_path: &proposed_policy_path,
            };
            let snapshot = replay_snapshot_from_result(context, result);
            append_replay_snapshot(&snapshots_path, snapshot.clone())
                .await
                .map(|_| snapshot)
        } else {
            Err(NoetError::InvalidConfig(
                "replay result failed before snapshot persistence".to_owned(),
            ))
        };
        let mut jobs = jobs.lock().await;
        if let Some(job) = jobs.get_mut(&job_id) {
            job.completed_at = Some(completed_at);
            match result {
                Ok(result) => {
                    match snapshot_result {
                        Ok(snapshot) => {
                            job.status = "completed".to_owned();
                            job.snapshot = Some(snapshot);
                        }
                        Err(error) => {
                            job.status = "failed".to_owned();
                            job.error = Some(error.to_string());
                        }
                    }
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

async fn latest_replay_job_matching_policy(
    state: &AppState,
    active_policy_hash: &str,
    proposed_policy_hash: Option<&str>,
) -> Option<(String, AppReplayJob)> {
    let jobs = state.replay_jobs.lock().await;
    jobs.iter()
        .filter(|(_, job)| {
            replay_job_matches_policy_hashes(job, active_policy_hash, proposed_policy_hash)
        })
        .max_by_key(|(_, job)| (job.status == "running", job.created_at))
        .map(|(id, job)| (id.clone(), job.clone()))
}

async fn replay_ledger_changed_since(
    state: &AppState,
    since: chrono::DateTime<chrono::Utc>,
) -> Result<bool, NoetError> {
    state
        .read_ledger(move |ledger| {
            Ok(!ledger.decisions_report_since(Some(since))?.is_empty()
                || !ledger.usage_activity_report_since(Some(since))?.is_empty())
        })
        .await
}

fn full_replay_spend_seeds(
    ledger: &BudgetLedger,
    policy: &crate::policy::PolicyFile,
    history_window_start: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<ReplaySpendSeed>, NoetError> {
    let mut seeds = Vec::new();
    for rule in &policy.budgets {
        for limit in &rule.limits.spend {
            let Some(window) = crate::policy::parse_limit_window(&limit.window) else {
                continue;
            };
            let Some(mode) = limit.mode else {
                continue;
            };
            let limit_id = limit.id.as_deref().unwrap_or(limit.window.as_str());
            let since = history_window_start - window;
            for total in
                ledger.spend_scope_totals(&rule.id, limit_id, since, history_window_start)?
            {
                let seeded_at = history_window_start - chrono::Duration::seconds(1);
                if mode == SpendWindowMode::Tumbling
                    && total.first_spend_at + window <= history_window_start
                {
                    continue;
                }
                seeds.push(ReplaySpendSeed {
                    rule_id: rule.id.clone(),
                    limit_id: limit_id.to_owned(),
                    scope_key: total.scope_key,
                    amount_usd: total.amount_usd,
                    mode,
                    seeded_at,
                    window_started_at: match mode {
                        SpendWindowMode::Tumbling => total.first_spend_at,
                        SpendWindowMode::Rolling => since,
                    },
                });
            }
        }
    }
    Ok(seeds)
}

fn replay_job_matches_policy(
    job: &AppReplayJob,
    active_policy_hash: &str,
    proposed_policy_hash: Option<&str>,
) -> bool {
    let Some(snapshot) = job.snapshot.as_ref() else {
        return false;
    };
    snapshot.active_policy_hash == active_policy_hash
        && proposed_policy_hash
            .map(|hash| snapshot.proposed_policy_hash == hash)
            .unwrap_or(false)
}

fn replay_job_matches_policy_hashes(
    job: &AppReplayJob,
    active_policy_hash: &str,
    proposed_policy_hash: Option<&str>,
) -> bool {
    job.active_policy_hash == active_policy_hash
        && proposed_policy_hash
            .map(|hash| job.proposed_policy_hash == hash)
            .unwrap_or(false)
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
    drop(jobs);
    let active_source = state
        .active_policy_source()
        .await
        .map(|(_, _, policy)| app_display_policy_source(policy.as_ref()))
        .transpose()?
        .unwrap_or_default();
    let active_hash = policy_hash(&active_source);
    let proposed_hash = app_policy_proposal(&state.policy_proposal_path)
        .await?
        .as_ref()
        .map(|proposal| policy_hash(&proposal.source));
    if !replay_job_matches_policy_hashes(&job, &active_hash, proposed_hash.as_deref()) {
        return Err(NoetError::NotFound(format!("replay job {job_id}")));
    }
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
        snapshot: job.snapshot,
    }
}

fn app_replay_job_status(id: String, job: &AppReplayJob) -> AppReplayJobStatus {
    AppReplayJobStatus {
        id,
        status: job.status.clone(),
        history_window_days: job.history_window_days,
        created_at: job.created_at,
        completed_at: job.completed_at,
        error: job.error.clone(),
    }
}

async fn app_replay_full_month_response(
    state: AppState,
    active_source: String,
    proposal: AppPolicyProposal,
) -> Result<AppReplayResponse, NoetError> {
    let history_window_end = chrono::Utc::now();
    let history_window_start =
        history_window_end - chrono::Duration::days(APP_REPLAY_HISTORY_WINDOW_DAYS);
    let proposed_policy = crate::policy::parse_policy_bytes(proposal.source.as_bytes())?;
    let (total_requests, historical_requests, usage_by_agent_run, baseline, spend_seeds) = state
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
            let spend_seeds =
                full_replay_spend_seeds(ledger, &proposed_policy, history_window_start)?;
            Ok((
                total_requests,
                historical_requests,
                usage_by_agent_run,
                baseline,
                spend_seeds,
            ))
        })
        .await?;
    let proposal = app_replay_proposal(
        &active_source,
        &proposal,
        &historical_requests,
        &usage_by_agent_run,
        &spend_seeds,
        ReplayScopeOptions {
            mode: "full_month".to_owned(),
            request_cap: None,
            total_requests_in_window: total_requests,
            full_replay_available: false,
            changed_runs_cap: APP_REPLAY_CHANGED_RUNS_CAP,
            window_seeded: !spend_seeds.is_empty(),
        },
    )?;
    Ok(AppReplayResponse {
        baseline,
        has_proposed_policy: true,
        message: "Full 30-day replay completed against the saved draft policy.".to_owned(),
        history_window_days: APP_REPLAY_HISTORY_WINDOW_DAYS,
        history_window_start,
        history_window_end,
        current_job: None,
        snapshots: Vec::new(),
        proposal: Some(proposal),
    })
}

struct ReplaySnapshotContext<'a> {
    id: &'a str,
    created_at: chrono::DateTime<chrono::Utc>,
    completed_at: chrono::DateTime<chrono::Utc>,
    active_policy_hash: &'a str,
    proposed_policy_hash: &'a str,
    active_policy_path: Option<&'a str>,
    proposed_policy_path: &'a str,
}

fn replay_snapshot_from_result(
    context: ReplaySnapshotContext<'_>,
    result: &AppReplayResponse,
) -> AppReplaySnapshot {
    let proposal = result
        .proposal
        .as_ref()
        .expect("completed replay snapshot requires proposal");
    AppReplaySnapshot {
        id: context.id.to_owned(),
        created_at: context.created_at,
        completed_at: context.completed_at,
        active_policy_hash: context.active_policy_hash.to_owned(),
        proposed_policy_hash: context.proposed_policy_hash.to_owned(),
        active_policy_path: context.active_policy_path.map(str::to_owned),
        proposed_policy_path: context.proposed_policy_path.to_owned(),
        policy_stale: false,
        scope: proposal.scope.clone(),
        baseline: proposal.baseline.clone(),
        proposed: proposal.proposed.clone(),
        spend_delta_usd: proposal.spend_delta_usd,
    }
}

async fn load_replay_snapshots(
    path: &Path,
    active_policy_hash: &str,
    proposed_policy_hash: Option<&str>,
) -> Result<Vec<AppReplaySnapshot>, NoetError> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut snapshots: Vec<AppReplaySnapshot> = match serde_json::from_slice(&bytes) {
        Ok(snapshots) => snapshots,
        Err(_) => return Ok(Vec::new()),
    };
    for snapshot in &mut snapshots {
        snapshot.policy_stale = snapshot.active_policy_hash != active_policy_hash
            || proposed_policy_hash
                .map(|hash| snapshot.proposed_policy_hash != hash)
                .unwrap_or(false);
    }
    snapshots.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.completed_at));
    snapshots.truncate(APP_REPLAY_MAX_JOBS);
    Ok(snapshots)
}

async fn append_replay_snapshot(path: &Path, snapshot: AppReplaySnapshot) -> Result<(), NoetError> {
    let active_hash = snapshot.active_policy_hash.clone();
    let proposed_hash = snapshot.proposed_policy_hash.clone();
    let mut snapshots = load_replay_snapshots(path, &active_hash, Some(&proposed_hash)).await?;
    snapshots.insert(0, snapshot);
    snapshots.truncate(APP_REPLAY_MAX_JOBS);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temp_path = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("json")
    ));
    tokio::fs::write(&temp_path, serde_json::to_vec_pretty(&snapshots)?).await?;
    tokio::fs::rename(temp_path, path).await?;
    Ok(())
}

fn policy_hash(source: &str) -> String {
    let digest = Sha256::digest(source.as_bytes());
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}
