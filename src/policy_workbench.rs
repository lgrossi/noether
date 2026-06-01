use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::contract::{AuthorizeRequest, DecisionMode, DecisionOutcome, SpendWindowMode};
use crate::error::NoetError;
use crate::ledger::{BudgetLedger, ReplaySpendSeed, TraceReportItem};
use crate::policy::PolicyFile;
use crate::reporting;

#[derive(Debug, Serialize)]
pub(crate) struct AppPolicyResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    pub(crate) source: String,
    pub(crate) policy: PolicyFile,
    pub(crate) decision_mode: DecisionMode,
    pub(crate) rule_stats: Vec<AppRuleStat>,
    pub(crate) suggestions: Vec<AppSuggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) proposal: Option<AppPolicyProposal>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AppPolicyProposal {
    pub(crate) path: String,
    pub(crate) source: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AppPolicyEnforceRequest {
    #[serde(default)]
    pub(crate) confirm_replay: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct AppPolicyRollbackResponse {
    pub(crate) policy: AppPolicyResponse,
    pub(crate) restored_from: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct AppRuleStat {
    pub(crate) rule: String,
    pub(crate) allow: u64,
    pub(crate) warn: u64,
    pub(crate) deny: u64,
    pub(crate) ask: u64,
    pub(crate) limit_hits: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_model: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AppSuggestion {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) rule: String,
    pub(crate) action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) apply_label: Option<String>,
    pub(crate) evidence: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct AppRunTotals {
    pub(crate) runs: u64,
    pub(crate) allow: u64,
    pub(crate) warn: u64,
    pub(crate) deny: u64,
    pub(crate) ask: u64,
    pub(crate) limit_hits: u64,
    pub(crate) spend_usd: f64,
    pub(crate) tokens: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AppRunUsage {
    pub(crate) cost_usd: f64,
    pub(crate) tokens: u64,
    pub(crate) request_count: u64,
}

#[derive(Clone, Debug, Default)]
struct ReplayRunAggregate {
    run_id: String,
    trace_id: Option<String>,
    baseline_decision: String,
    proposed_decision: String,
    cost_usd: f64,
    tokens: u64,
    rule: Option<String>,
    summary: String,
}

#[derive(Default)]
struct AppRuleEvidence {
    reasons: std::collections::BTreeMap<String, u64>,
    models: std::collections::BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AppReplayResponse {
    pub(crate) baseline: AppRunTotals,
    pub(crate) has_proposed_policy: bool,
    pub(crate) message: String,
    pub(crate) history_window_days: i64,
    pub(crate) history_window_start: chrono::DateTime<chrono::Utc>,
    pub(crate) history_window_end: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) proposal: Option<AppReplayProposal>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AppReplayScope {
    pub(crate) mode: String,
    pub(crate) request_cap: Option<usize>,
    pub(crate) requests_replayed: usize,
    pub(crate) total_requests_in_window: usize,
    pub(crate) has_more_history: bool,
    pub(crate) changed_runs_cap: usize,
    pub(crate) changed_runs_returned: usize,
    pub(crate) changed_runs_total: usize,
    pub(crate) full_replay_available: bool,
    pub(crate) window_seeded: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AppReplayProposal {
    pub(crate) path: String,
    pub(crate) mode: String,
    pub(crate) can_enforce: bool,
    pub(crate) explanation: String,
    pub(crate) changed_lines: u64,
    pub(crate) added_lines: u64,
    pub(crate) removed_lines: u64,
    pub(crate) proposed: AppRunTotals,
    pub(crate) changed_runs: Vec<AppReplayChangedRun>,
    pub(crate) recommendations: Vec<AppReplayRecommendation>,
    pub(crate) spend_delta_usd: f64,
    pub(crate) preview: Vec<AppReplayDiffLine>,
    pub(crate) scope: AppReplayScope,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AppReplayRecommendation {
    pub(crate) title: String,
    pub(crate) body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rule: Option<String>,
    pub(crate) action: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AppReplayChangedRun {
    pub(crate) run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) trace_id: Option<String>,
    #[serde(rename = "from")]
    pub(crate) from_decision: String,
    #[serde(rename = "to")]
    pub(crate) to_decision: String,
    pub(crate) cost_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rule: Option<String>,
    pub(crate) summary: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AppReplayJobResponse {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) history_window_days: i64,
    pub(crate) created_at: chrono::DateTime<chrono::Utc>,
    pub(crate) completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<AppReplayResponse>,
}

#[derive(Clone, Debug)]
pub(crate) struct AppReplayJob {
    pub(crate) status: String,
    pub(crate) history_window_days: i64,
    pub(crate) created_at: chrono::DateTime<chrono::Utc>,
    pub(crate) completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) error: Option<String>,
    pub(crate) result: Option<AppReplayResponse>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AppReplayDiffLine {
    pub(crate) kind: String,
    pub(crate) line: String,
}

pub(crate) async fn app_policy_proposal(
    path: &Path,
) -> Result<Option<AppPolicyProposal>, NoetError> {
    match fs::read_to_string(path).await {
        Ok(source) => {
            let policy = crate::policy::parse_policy_bytes(source.as_bytes())?;
            Ok(Some(AppPolicyProposal {
                path: path.display().to_string(),
                source: app_display_policy_source(&policy)?,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn app_display_policy_source(policy: &PolicyFile) -> Result<String, NoetError> {
    serde_yaml::to_string(policy).map_err(NoetError::from)
}

pub(crate) fn app_rule_stats(
    policy: &PolicyFile,
    decisions: &[TraceReportItem],
) -> Vec<AppRuleStat> {
    let mut stats = policy
        .budgets
        .iter()
        .map(|budget| {
            (
                budget.id.clone(),
                AppRuleStat {
                    rule: budget.id.clone(),
                    allow: 0,
                    warn: 0,
                    deny: 0,
                    ask: 0,
                    limit_hits: 0,
                    top_reason: None,
                    top_model: None,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut evidence = std::collections::BTreeMap::<String, AppRuleEvidence>::new();

    for item in decisions {
        let rule = item
            .routing
            .as_ref()
            .and_then(|routing| routing.selected_budget_id.clone())
            .unwrap_or_else(|| "unattributed".to_owned());
        let stat = stats.entry(rule.clone()).or_insert_with(|| AppRuleStat {
            rule,
            allow: 0,
            warn: 0,
            deny: 0,
            ask: 0,
            limit_hits: 0,
            top_reason: None,
            top_model: None,
        });
        let decision = app_decision_label(&item.kind);
        match decision.as_str() {
            "allow" => stat.allow += 1,
            "warn" => stat.warn += 1,
            "deny" => stat.deny += 1,
            "ask" => stat.ask += 1,
            _ => {}
        }
        stat.limit_hits += item
            .limit_hits
            .as_ref()
            .map(|hits| hits.len() as u64)
            .unwrap_or(0);
        if decision == "deny"
            || item
                .limit_hits
                .as_ref()
                .is_some_and(|hits| !hits.is_empty())
        {
            let evidence = evidence.entry(stat.rule.clone()).or_default();
            if let Some(reason) = app_decision_reason(item) {
                *evidence.reasons.entry(reason).or_default() += 1;
            }
            if let Some(model) = reporting::summary_value(&item.summary, "model") {
                *evidence.models.entry(model).or_default() += 1;
            }
        }
    }

    let mut stats = stats.into_values().collect::<Vec<_>>();
    for stat in &mut stats {
        if let Some(evidence) = evidence.get(&stat.rule) {
            stat.top_reason = most_common(&evidence.reasons);
            stat.top_model = most_common(&evidence.models);
        }
    }
    stats
}

pub(crate) fn app_rule_stats_from_report(
    policy: &PolicyFile,
    report: Vec<crate::ledger::RuleStatsReport>,
) -> Vec<AppRuleStat> {
    let mut stats = policy
        .budgets
        .iter()
        .map(|budget| {
            (
                budget.id.clone(),
                AppRuleStat {
                    rule: budget.id.clone(),
                    allow: 0,
                    warn: 0,
                    deny: 0,
                    ask: 0,
                    limit_hits: 0,
                    top_reason: None,
                    top_model: None,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for row in report {
        stats.insert(
            row.rule.clone(),
            AppRuleStat {
                rule: row.rule,
                allow: row.allow,
                warn: row.warn,
                deny: row.deny,
                ask: row.ask,
                limit_hits: row.limit_hits,
                top_reason: row.top_reason,
                top_model: row.top_model,
            },
        );
    }
    stats.into_values().collect()
}

pub(crate) fn app_policy_suggestions(stats: &[AppRuleStat]) -> Vec<AppSuggestion> {
    let mut suggestions = Vec::new();
    for stat in stats {
        if stat.deny > 0 {
            let mut evidence = Vec::new();
            if let Some(reason) = &stat.top_reason {
                evidence.push(format!("Reason: {reason}"));
            }
            if let Some(model) = &stat.top_model {
                evidence.push(format!("Top model: {model}"));
            }
            evidence.push(format!("Denied runs: {}", stat.deny));
            let body = match (&stat.top_reason, &stat.top_model) {
                (Some(reason), Some(model)) if reason.contains("provider/model is not allowed") => {
                    let pattern = model_ref_to_policy_pattern(model);
                    format!(
                        "If this is intended, keep the denial. If not, add {pattern} to {}.models.allow or route it to another budget, then replay.",
                        stat.rule
                    )
                }
                (Some(reason), _) => format!(
                    "Most denials are because: {reason}. Inspect affected runs, edit the specific rule if needed, then replay."
                ),
                _ => "Inspect affected runs, edit the specific rule if needed, then replay."
                    .to_owned(),
            };
            suggestions.push(AppSuggestion {
                id: format!("{}-denies", stat.rule),
                title: format!("{} blocked {} run(s)", stat.rule, stat.deny),
                body,
                rule: stat.rule.clone(),
                action: "open_runs_filtered_to_rule".to_owned(),
                apply_label: stat
                    .top_reason
                    .as_deref()
                    .filter(|reason| reason.contains("provider/model is not allowed"))
                    .and(stat.top_model.as_deref())
                    .map(|model| format!("Allow {}", model_ref_to_policy_pattern(model))),
                evidence,
            });
        } else if stat.limit_hits > 0 {
            let evidence = stat
                .top_reason
                .iter()
                .map(|reason| format!("Limit evidence: {reason}"))
                .collect::<Vec<_>>();
            suggestions.push(AppSuggestion {
                id: format!("{}-limit-hits", stat.rule),
                title: format!("{} hit limits {} time(s)", stat.rule, stat.limit_hits),
                body: "This rule is close to its boundary. Replay a stricter or roomier policy against real history.".to_owned(),
                rule: stat.rule.clone(),
                action: "replay_rule_change".to_owned(),
                apply_label: None,
                evidence,
            });
        }
    }
    suggestions.truncate(3);
    suggestions
}

pub(crate) fn app_decision_reason(item: &TraceReportItem) -> Option<String> {
    if let Some(hit) = item
        .binding_limit
        .as_ref()
        .or_else(|| item.limit_hits.as_ref().and_then(|hits| hits.first()))
    {
        return Some(hit.reason.clone());
    }
    let routing = item.routing.as_ref()?;
    if routing.model_check.as_deref() == Some("denied") {
        return Some("provider/model is not allowed by budget".to_owned());
    }
    routing.rejected_budget_reason.clone()
}

fn model_ref_to_policy_pattern(model_ref: &str) -> String {
    model_ref
        .split_once('/')
        .map(|(provider, model)| format!("{provider}:{model}"))
        .unwrap_or_else(|| model_ref.to_owned())
}

pub(crate) fn apply_suggestion_to_policy_source(
    source: &str,
    suggestion: &AppSuggestion,
) -> Result<String, NoetError> {
    let model = suggestion
        .apply_label
        .as_deref()
        .and_then(|label| label.strip_prefix("Allow "))
        .ok_or_else(|| {
            NoetError::InvalidPolicy("suggestion cannot be applied automatically".to_owned())
        })?;
    let mut policy = crate::policy::parse_policy_bytes(source.as_bytes())?;
    let budget = policy
        .budgets
        .iter_mut()
        .find(|budget| budget.id == suggestion.rule)
        .ok_or_else(|| NoetError::NotFound(format!("budget {}", suggestion.rule)))?;
    if !budget.models.allow.iter().any(|value| value == model) {
        budget.models.allow.push(model.to_owned());
        budget.models.allow.sort();
    }
    serde_yaml::to_string(&policy).map_err(NoetError::from)
}

fn most_common(values: &std::collections::BTreeMap<String, u64>) -> Option<String> {
    values
        .iter()
        .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
        .map(|(value, _)| value.clone())
}

pub(crate) fn app_replay_proposal(
    active_source: &str,
    proposal: &AppPolicyProposal,
    historical_requests: &[crate::ledger::HistoricalAuthorizeRequest],
    usage_by_agent_run: &std::collections::BTreeMap<String, AppRunUsage>,
    spend_seeds: &[ReplaySpendSeed],
    scope_options: ReplayScopeOptions,
) -> Result<AppReplayProposal, NoetError> {
    let active_lines = active_source
        .lines()
        .collect::<std::collections::BTreeSet<_>>();
    let proposal_lines = proposal
        .source
        .lines()
        .collect::<std::collections::BTreeSet<_>>();
    let mut preview = Vec::new();

    for line in active_lines.difference(&proposal_lines).take(8) {
        preview.push(AppReplayDiffLine {
            kind: "removed".to_owned(),
            line: (*line).to_owned(),
        });
    }
    for line in proposal_lines.difference(&active_lines).take(8) {
        preview.push(AppReplayDiffLine {
            kind: "added".to_owned(),
            line: (*line).to_owned(),
        });
    }

    let added_lines = proposal_lines.difference(&active_lines).count() as u64;
    let removed_lines = active_lines.difference(&proposal_lines).count() as u64;
    let changed_lines = added_lines + removed_lines;
    let proposed_policy = crate::policy::parse_policy_bytes(proposal.source.as_bytes())?;
    let (proposed, mut changed_runs, spend_delta_usd, changed_runs_total) =
        replay_historical_requests(
            &proposed_policy,
            historical_requests,
            usage_by_agent_run,
            spend_seeds,
        )?;
    let recommendations = app_replay_recommendations(&changed_runs, spend_delta_usd);
    changed_runs.truncate(scope_options.changed_runs_cap);
    let changed_runs_returned = changed_runs.len();
    let (mode, explanation) = if changed_lines == 0 {
        (
            "current_policy_backtest",
            "No pending source edit. This backtests the currently saved policy against recorded historical decisions.",
        )
    } else {
        (
            "draft_impact",
            "This compares the active policy to the saved draft by replaying recorded historical authorizations.",
        )
    };
    Ok(AppReplayProposal {
        path: proposal.path.clone(),
        mode: mode.to_owned(),
        can_enforce: changed_lines > 0,
        explanation: explanation.to_owned(),
        changed_lines,
        added_lines,
        removed_lines,
        proposed,
        changed_runs,
        recommendations,
        spend_delta_usd,
        preview,
        scope: AppReplayScope {
            mode: scope_options.mode,
            request_cap: scope_options.request_cap,
            requests_replayed: historical_requests.len(),
            total_requests_in_window: scope_options.total_requests_in_window,
            has_more_history: scope_options.total_requests_in_window > historical_requests.len(),
            changed_runs_cap: scope_options.changed_runs_cap,
            changed_runs_returned,
            changed_runs_total,
            full_replay_available: scope_options.full_replay_available,
            window_seeded: scope_options.window_seeded,
        },
    })
}

pub(crate) struct ReplayScopeOptions {
    pub(crate) mode: String,
    pub(crate) request_cap: Option<usize>,
    pub(crate) total_requests_in_window: usize,
    pub(crate) full_replay_available: bool,
    pub(crate) changed_runs_cap: usize,
    pub(crate) window_seeded: bool,
}

pub(crate) fn app_replay_spend_seeds(
    ledger: &BudgetLedger,
    proposed_policy: &PolicyFile,
    history_window_start: chrono::DateTime<chrono::Utc>,
    preview_start: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<ReplaySpendSeed>, NoetError> {
    if preview_start <= history_window_start {
        return Ok(Vec::new());
    }
    let seed_at = preview_start - chrono::Duration::nanoseconds(1);
    let mut seeds = Vec::new();
    for rule in &proposed_policy.budgets {
        for limit in &rule.limits.spend {
            let Some(window) = crate::policy::parse_limit_window(&limit.window) else {
                continue;
            };
            let limit_id = limit.id.as_deref().unwrap_or(limit.window.as_str());
            let mode = limit.mode.unwrap_or(SpendWindowMode::Tumbling);
            let since = match mode {
                SpendWindowMode::Rolling => (preview_start - window).max(history_window_start),
                SpendWindowMode::Tumbling => (preview_start - window).max(history_window_start),
            };
            let totals = ledger.spend_scope_totals(&rule.id, limit_id, since, preview_start)?;
            for total in totals {
                seeds.push(ReplaySpendSeed {
                    rule_id: rule.id.clone(),
                    limit_id: limit_id.to_owned(),
                    scope_key: total.scope_key,
                    amount_usd: total.amount_usd,
                    mode,
                    seeded_at: seed_at,
                    window_started_at: since,
                });
            }
        }
    }
    Ok(seeds)
}

fn app_replay_recommendations(
    changed_runs: &[AppReplayChangedRun],
    spend_delta_usd: f64,
) -> Vec<AppReplayRecommendation> {
    if changed_runs.is_empty() {
        return vec![AppReplayRecommendation {
            title: "Draft matches recorded history".to_owned(),
            body: "No recorded run decisions would change. This is safe from a historical-decision perspective, but it may still affect future traffic.".to_owned(),
            rule: None,
            action: "review_policy_diff".to_owned(),
        }];
    }

    let newly_blocked = changed_runs
        .iter()
        .filter(|run| run.to_decision == "deny" && run.from_decision != "deny")
        .count();
    let newly_warned = changed_runs
        .iter()
        .filter(|run| run.to_decision == "warn" && run.from_decision != "warn")
        .count();
    let newly_allowed = changed_runs
        .iter()
        .filter(|run| run.from_decision == "deny" && run.to_decision != "deny")
        .count();
    let mut by_rule = std::collections::BTreeMap::<String, (u64, f64)>::new();
    for run in changed_runs {
        let rule = run
            .rule
            .clone()
            .unwrap_or_else(|| "unattributed".to_owned());
        let entry = by_rule.entry(rule).or_default();
        entry.0 += 1;
        entry.1 += run.cost_usd;
    }
    let (rule, (count, cost)) = by_rule
        .into_iter()
        .max_by(|(_, left), (_, right)| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
        })
        .expect("changed runs are non-empty");
    let title = if newly_blocked > 0 {
        format!("{newly_blocked} run(s) would be newly blocked")
    } else if newly_warned > 0 && newly_allowed == 0 {
        format!("{newly_warned} run(s) would become warnings")
    } else if newly_warned > 0 && newly_allowed > 0 {
        format!("{newly_warned} warning(s), {newly_allowed} previously denied run(s) loosened")
    } else if newly_allowed > 0 {
        format!("{newly_allowed} previously denied run(s) would be allowed or warned")
    } else {
        format!("{count} recorded outcome(s) would change")
    };
    let body = if newly_blocked > 0 {
        format!(
            "This draft blocks traffic that previously ran. The largest affected rule is {rule}, covering ${cost:.2}. Projected spend delta is {spend_delta_usd:+.2}; inspect examples before adopting."
        )
    } else if newly_warned > 0 {
        format!(
            "This draft mostly changes enforcement posture, not spend: affected runs would warn under {rule}. Projected spend delta is {spend_delta_usd:+.2}."
        )
    } else {
        format!(
            "The largest affected rule is {rule}, covering ${cost:.2}. Projected spend delta is {spend_delta_usd:+.2}; inspect examples before adopting."
        )
    };
    vec![AppReplayRecommendation {
        title,
        body,
        rule: Some(rule),
        action: "review_changed_runs".to_owned(),
    }]
}

pub(crate) fn replay_historical_requests(
    proposed_policy: &PolicyFile,
    historical_requests: &[crate::ledger::HistoricalAuthorizeRequest],
    usage_by_agent_run: &std::collections::BTreeMap<String, AppRunUsage>,
    spend_seeds: &[ReplaySpendSeed],
) -> Result<(AppRunTotals, Vec<AppReplayChangedRun>, f64, usize), NoetError> {
    let mut replay_ledger = BudgetLedger::default();
    for seed in spend_seeds {
        replay_ledger.seed_replay_spend(seed.clone());
    }
    let mut runs = std::collections::BTreeMap::<String, ReplayRunAggregate>::new();

    for historical in historical_requests {
        let decision = replay_ledger.try_authorize_replay_at(
            Some(proposed_policy),
            &historical.request,
            historical.occurred_at,
        )?;
        let proposed_label = decision_outcome_label(decision.outcome);
        let baseline_label = decision_outcome_label(historical.baseline_outcome);
        let agent_run_id = string_metadata_value(&historical.request, "agent_run_id");
        let trace_id = string_metadata_value(&historical.request, "trace_id");
        let key = agent_run_id
            .clone()
            .map(|id| format!("agent-run:{id}"))
            .or_else(|| trace_id.clone().map(|id| format!("trace:{id}")))
            .unwrap_or_else(|| {
                let minute_bucket = historical.occurred_at.timestamp() / 60;
                format!(
                    "untraced:{}:{}:{}:{minute_bucket}",
                    baseline_label,
                    "unattributed",
                    historical.request.model.as_deref().unwrap_or("unknown")
                )
            });
        let run_id = agent_run_id
            .clone()
            .or_else(|| trace_id.clone())
            .unwrap_or_else(|| historical.decision_id.clone());
        let usage = agent_run_id
            .as_deref()
            .and_then(|id| usage_by_agent_run.get(id).copied());
        let entry = runs.entry(key).or_insert_with(|| ReplayRunAggregate {
            run_id,
            trace_id,
            baseline_decision: baseline_label.to_owned(),
            proposed_decision: proposed_label.to_owned(),
            cost_usd: usage.map(|usage| usage.cost_usd).unwrap_or(0.0),
            tokens: usage.map(|usage| usage.tokens).unwrap_or(0),
            rule: None,
            summary: replay_change_summary(&historical.request),
        });
        if usage.is_none() {
            entry.cost_usd += historical.request.estimated_cost_usd.unwrap_or(0.0);
            entry.tokens += historical.request.estimated_tokens.unwrap_or(0);
        }
        if app_decision_rank(baseline_label) > app_decision_rank(&entry.baseline_decision) {
            entry.baseline_decision = baseline_label.to_owned();
        }
        if app_decision_rank(proposed_label) > app_decision_rank(&entry.proposed_decision) {
            entry.proposed_decision = proposed_label.to_owned();
        }
        if entry.rule.is_none() {
            entry.rule = decision
                .explanations
                .iter()
                .find(|explanation| explanation.severity == decision.action.decision_severity())
                .map(|explanation| explanation.rule_id.clone());
        }
    }

    let mut totals = AppRunTotals {
        runs: runs.len() as u64,
        ..AppRunTotals::default()
    };
    let mut changed_runs = Vec::new();
    let mut baseline_spend = 0.0;
    let mut proposed_spend = 0.0;
    for run in runs.into_values() {
        totals.tokens += run.tokens;
        match run.proposed_decision.as_str() {
            "allow" => totals.allow += 1,
            "warn" => totals.warn += 1,
            "deny" => totals.deny += 1,
            "ask" => totals.ask += 1,
            _ => {}
        }
        if run.baseline_decision != "deny" {
            baseline_spend += run.cost_usd;
        }
        if run.proposed_decision != "deny" {
            proposed_spend += run.cost_usd;
            totals.spend_usd += run.cost_usd;
        }
        if run.baseline_decision != run.proposed_decision {
            changed_runs.push(AppReplayChangedRun {
                run_id: run.run_id,
                trace_id: run.trace_id,
                from_decision: run.baseline_decision,
                to_decision: run.proposed_decision,
                cost_usd: run.cost_usd,
                rule: run.rule,
                summary: run.summary,
            });
        }
    }
    changed_runs.sort_by(|left, right| right.cost_usd.total_cmp(&left.cost_usd));
    let changed_runs_total = changed_runs.len();
    Ok((
        totals,
        changed_runs,
        proposed_spend - baseline_spend,
        changed_runs_total,
    ))
}

fn decision_outcome_label(outcome: DecisionOutcome) -> &'static str {
    match outcome {
        DecisionOutcome::Allow => "allow",
        DecisionOutcome::Warn => "warn",
        DecisionOutcome::Deny => "deny",
    }
}

fn app_decision_rank(decision: &str) -> u8 {
    match decision {
        "deny" => 4,
        "ask" => 3,
        "warn" => 2,
        "allow" => 1,
        _ => 0,
    }
}

pub(crate) fn string_metadata_value(request: &AuthorizeRequest, key: &str) -> Option<String> {
    request
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn replay_change_summary(request: &AuthorizeRequest) -> String {
    [
        request.project.as_deref(),
        request.subject.as_deref(),
        request.provider.as_deref(),
        request.model.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

pub(crate) fn app_run_totals_from_report(report: crate::ledger::RunTotalsReport) -> AppRunTotals {
    AppRunTotals {
        runs: report.runs,
        allow: report.allow,
        warn: report.warn,
        deny: report.deny,
        ask: report.ask,
        limit_hits: report.limit_hits,
        spend_usd: report.spend_usd,
        tokens: report.tokens,
    }
}

pub(crate) fn app_decision_label(kind: &str) -> String {
    if kind.ends_with(".allow") {
        "allow"
    } else if kind.ends_with(".warn") {
        "warn"
    } else if kind.ends_with(".deny") {
        "deny"
    } else if kind.ends_with(".ask") {
        "ask"
    } else {
        kind
    }
    .to_owned()
}
