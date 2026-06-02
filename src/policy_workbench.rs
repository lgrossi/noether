use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::contract::DecisionMode;
use crate::error::NoetError;
use crate::ledger::TraceReportItem;
use crate::policy::PolicyFile;

pub(crate) use crate::replay_workbench::AppRunUsage;
use crate::reporting;

#[derive(Debug, Serialize)]
pub(crate) struct AppPolicyResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    pub(crate) source: String,
    pub(crate) policy: PolicyFile,
    pub(crate) decision_mode: DecisionMode,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reload_error: Option<String>,
    pub(crate) rule_stats: Vec<AppRuleStat>,
    pub(crate) suggestions: Vec<AppSuggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) proposal: Option<AppPolicyProposal>,
}

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
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

#[derive(Default)]
struct AppRuleEvidence {
    reasons: std::collections::BTreeMap<String, u64>,
    models: std::collections::BTreeMap<String, u64>,
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
