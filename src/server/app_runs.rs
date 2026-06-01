use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use serde::{Deserialize, Serialize};

use crate::error::NoetError;
use crate::ledger::{BudgetLedger, TraceReportItem};
use crate::policy_workbench::{
    AppRunTotals, AppRunUsage, app_decision_label, app_decision_reason, app_run_totals_from_report,
};
use crate::reporting;

use super::AppState;

#[derive(Debug, Serialize)]
pub(super) struct AppRunsResponse {
    runs: Vec<AppRunRow>,
    totals: AppRunTotals,
    filtered_total: u64,
    next_offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AppRunsQuery {
    #[serde(default = "default_app_runs_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
    decision: Option<String>,
    rule: Option<String>,
    q: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct AppRunRow {
    occurred_at: chrono::DateTime<chrono::Utc>,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_run_id: Option<String>,
    decision: String,
    summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_check: Option<String>,
    limit_hits: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matched_entity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    timeline: Vec<AppRunTimelineItem>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct AppRunTimelineItem {
    occurred_at: chrono::DateTime<chrono::Utc>,
    kind: String,
    summary: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    fields: BTreeMap<String, String>,
}

fn default_app_runs_limit() -> usize {
    80
}

pub(super) async fn app_runs(
    State(state): State<AppState>,
    Query(query): Query<AppRunsQuery>,
) -> Result<Json<AppRunsResponse>, NoetError> {
    let limit = query.limit.clamp(1, 250);
    let offset = query.offset;
    state
        .read_ledger(move |ledger| {
            if app_runs_query_is_unfiltered(&query) {
                let decisions = ledger.decisions_report_for_run_page(limit, offset)?;
                let agent_run_ids = app_agent_run_ids_from_decisions(&decisions);
                let usage_by_agent_run = app_usage_by_agent_run(
                    &ledger.usage_activity_report_for_agent_runs(&agent_run_ids)?,
                );
                let runs = app_agent_runs(&decisions, &usage_by_agent_run);
                let totals = app_run_totals_from_report(ledger.run_totals_report()?);
                let filtered_total = totals.runs;
                let next_offset =
                    (offset + runs.len() < filtered_total as usize).then_some(offset + runs.len());
                return Ok(Json(AppRunsResponse {
                    runs,
                    totals,
                    filtered_total,
                    next_offset,
                }));
            }
            let decisions = reporting::decisions_report(ledger)?;
            let usage_by_agent_run = app_usage_by_agent_run(&ledger.usage_activity_report()?);
            let all_runs = app_agent_runs(&decisions, &usage_by_agent_run);
            let totals = app_run_totals_from_rows(&all_runs);
            let filtered = all_runs
                .into_iter()
                .filter(|run| app_run_matches_query(run, &query))
                .collect::<Vec<_>>();
            let filtered_total = filtered.len() as u64;
            let runs = filtered
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>();
            let next_offset =
                (offset + runs.len() < filtered_total as usize).then_some(offset + runs.len());
            Ok(Json(AppRunsResponse {
                runs,
                totals,
                filtered_total,
                next_offset,
            }))
        })
        .await
}

fn app_runs_query_is_unfiltered(query: &AppRunsQuery) -> bool {
    query
        .decision
        .as_deref()
        .map(|value| value.is_empty() || value == "any")
        .unwrap_or(true)
        && query
            .rule
            .as_deref()
            .map(|value| value.is_empty() || value == "any")
            .unwrap_or(true)
        && query.q.as_deref().map(str::trim).unwrap_or("").is_empty()
}

pub(super) async fn app_run_detail(
    State(state): State<AppState>,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<AppRunRow>, NoetError> {
    state
        .read_ledger(move |ledger| {
            let decisions = reporting::decisions_report(ledger)?;
            let usage_by_agent_run = app_usage_by_agent_run(&ledger.usage_activity_report()?);
            let mut run = app_agent_runs(&decisions, &usage_by_agent_run)
                .into_iter()
                .find(|run| {
                    run.id == run_id
                        || run.agent_run_id.as_deref() == Some(run_id.as_str())
                        || run.trace_id.as_deref() == Some(run_id.as_str())
                })
                .ok_or_else(|| NoetError::NotFound(format!("run {run_id}")))?;
            run.timeline = app_run_timeline(ledger, &run)?;
            Ok(Json(run))
        })
        .await
}

fn app_agent_runs(
    decisions: &[TraceReportItem],
    usage_by_agent_run: &std::collections::BTreeMap<String, AppRunUsage>,
) -> Vec<AppRunRow> {
    let mut runs = std::collections::BTreeMap::<String, AppRunRow>::new();
    for item in decisions {
        let run = app_run_row(item, usage_by_agent_run);
        let key = app_run_group_key(&run);
        runs.entry(key)
            .and_modify(|existing| merge_app_run(existing, &run))
            .or_insert(run);
    }
    let mut runs = runs.into_values().collect::<Vec<_>>();
    runs.sort_by_key(|run| std::cmp::Reverse(run.occurred_at));
    runs
}

fn app_run_group_key(run: &AppRunRow) -> String {
    if let Some(agent_run_id) = run.agent_run_id.as_deref() {
        return format!("agent-run:{agent_run_id}");
    }
    if let Some(trace_id) = run.trace_id.as_deref() {
        return format!("trace-fallback:{trace_id}");
    }
    let minute_bucket = run.occurred_at.timestamp() / 60;
    format!(
        "untraced:{}:{}:{}:{minute_bucket}",
        run.decision,
        run.rule.as_deref().unwrap_or("unattributed"),
        run.model.as_deref().unwrap_or("unknown")
    )
}

fn app_run_row(
    item: &TraceReportItem,
    usage_by_agent_run: &std::collections::BTreeMap<String, AppRunUsage>,
) -> AppRunRow {
    let trace_id = item
        .trace_id
        .clone()
        .or_else(|| reporting::summary_value(&item.summary, "trace"));
    let request_id = reporting::summary_value(&item.summary, "request")
        .or_else(|| reporting::summary_value(&item.summary, "request_id"));
    let id = request_id
        .or_else(|| trace_id.clone())
        .unwrap_or_else(|| format!("{}-{}", item.kind, item.occurred_at.timestamp_millis()));
    let agent_run_id = item.agent_run_id.clone();
    let model_ref = reporting::summary_value(&item.summary, "model");
    let (provider, model) = model_ref
        .as_deref()
        .and_then(|value| value.split_once('/'))
        .map(|(provider, model)| (Some(provider.to_owned()), Some(model.to_owned())))
        .unwrap_or_else(|| (None, model_ref.clone()));
    let run_usage = agent_run_id
        .as_deref()
        .and_then(|agent_run_id| usage_by_agent_run.get(agent_run_id).copied());
    let estimated_cost = reporting::summary_value(&item.summary, "estimated_cost")
        .and_then(|value| value.parse::<f64>().ok());
    AppRunRow {
        occurred_at: item.occurred_at,
        id: agent_run_id.clone().unwrap_or(id),
        agent_run_id,
        decision: app_decision_label(&item.kind),
        summary: item.summary.clone(),
        trace_id,
        rule: item
            .routing
            .as_ref()
            .and_then(|routing| routing.selected_budget_id.clone()),
        decision_reason: app_decision_reason(item),
        model_check: item
            .routing
            .as_ref()
            .and_then(|routing| routing.model_check.clone())
            .or_else(|| reporting::summary_value(&item.summary, "model_check")),
        limit_hits: item
            .limit_hits
            .as_ref()
            .map(|hits| hits.len() as u64)
            .unwrap_or(0),
        provider,
        model,
        cost_usd: estimated_cost.or_else(|| {
            run_usage
                .map(|usage| usage.cost_usd)
                .filter(|cost| *cost > 0.0)
        }),
        estimated_tokens: reporting::summary_value(&item.summary, "estimated_tokens")
            .and_then(|value| value.parse::<u64>().ok()),
        actual_tokens: run_usage
            .map(|usage| usage.tokens)
            .filter(|tokens| *tokens > 0),
        tool_calls: reporting::summary_value(&item.summary, "tools_count")
            .and_then(|value| value.parse::<u64>().ok()),
        matched_entity: item
            .routing
            .as_ref()
            .and_then(|routing| routing.matched_entity.clone()),
        entities: item.entities.clone(),
        timeline: Vec::new(),
    }
}

fn merge_app_run(existing: &mut AppRunRow, next: &AppRunRow) {
    if next.occurred_at > existing.occurred_at {
        existing.occurred_at = next.occurred_at;
        existing.id = next.id.clone();
        existing.summary = next.summary.clone();
        existing.provider = next.provider.clone().or_else(|| existing.provider.clone());
        existing.model = next.model.clone().or_else(|| existing.model.clone());
        existing.estimated_tokens = next.estimated_tokens.or(existing.estimated_tokens);
        existing.tool_calls = next.tool_calls.or(existing.tool_calls);
    }
    existing.limit_hits += next.limit_hits;
    existing.cost_usd = existing.cost_usd.or(next.cost_usd);
    existing.actual_tokens = existing.actual_tokens.or(next.actual_tokens);
    if app_decision_rank(&next.decision) > app_decision_rank(&existing.decision) {
        existing.decision = next.decision.clone();
        existing.rule = next.rule.clone().or_else(|| existing.rule.clone());
        existing.decision_reason = next
            .decision_reason
            .clone()
            .or_else(|| existing.decision_reason.clone());
        existing.model_check = next
            .model_check
            .clone()
            .or_else(|| existing.model_check.clone());
    }
    if existing.rule.is_none() {
        existing.rule = next.rule.clone();
    }
    if existing.decision_reason.is_none() {
        existing.decision_reason = next.decision_reason.clone();
    }
    if existing.model_check.is_none() {
        existing.model_check = next.model_check.clone();
    }
    if existing.matched_entity.is_none() {
        existing.matched_entity = next.matched_entity.clone();
    }
    for entity in &next.entities {
        if !existing.entities.contains(entity) {
            existing.entities.push(entity.clone());
        }
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

fn app_run_totals_from_rows(runs: &[AppRunRow]) -> AppRunTotals {
    let mut totals = AppRunTotals {
        runs: runs.len() as u64,
        ..AppRunTotals::default()
    };
    for run in runs {
        match run.decision.as_str() {
            "allow" => totals.allow += 1,
            "warn" => totals.warn += 1,
            "deny" => totals.deny += 1,
            "ask" => totals.ask += 1,
            _ => {}
        }
        totals.limit_hits += run.limit_hits;
        totals.spend_usd += run.cost_usd.unwrap_or(0.0);
        totals.tokens += run.actual_tokens.or(run.estimated_tokens).unwrap_or(0);
    }
    totals
}

pub(super) fn app_usage_by_agent_run(
    usage: &[crate::ledger::UsageActivityRecord],
) -> std::collections::BTreeMap<String, AppRunUsage> {
    let mut by_agent_run = std::collections::BTreeMap::new();
    for record in usage {
        let Some(agent_run_id) = record.agent_run_id.as_deref() else {
            continue;
        };
        let entry = by_agent_run
            .entry(agent_run_id.to_owned())
            .or_insert_with(AppRunUsage::default);
        entry.cost_usd += record.cost_usd;
        entry.tokens += record.total_tokens;
        entry.request_count += 1;
    }
    by_agent_run
}

fn app_agent_run_ids_from_decisions(decisions: &[TraceReportItem]) -> Vec<String> {
    decisions
        .iter()
        .filter_map(|decision| decision.agent_run_id.clone())
        .collect()
}

fn app_run_timeline(
    ledger: &BudgetLedger,
    run: &AppRunRow,
) -> Result<Vec<AppRunTimelineItem>, NoetError> {
    let Some(trace_id) = run.trace_id.as_deref() else {
        return Ok(vec![AppRunTimelineItem {
            occurred_at: run.occurred_at,
            kind: format!("decision.{}", run.decision),
            summary: run.summary.clone(),
            fields: app_summary_fields(&run.summary),
        }]);
    };
    let trace = ledger.trace_report(trace_id)?;
    let mut items = trace
        .items
        .into_iter()
        .filter(|item| {
            run.agent_run_id.is_none()
                || item.agent_run_id.as_deref() == run.agent_run_id.as_deref()
        })
        .map(|item| AppRunTimelineItem {
            occurred_at: item.occurred_at,
            kind: item.kind,
            fields: app_summary_fields(&item.summary),
            summary: item.summary,
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        items.push(AppRunTimelineItem {
            occurred_at: run.occurred_at,
            kind: format!("decision.{}", run.decision),
            summary: run.summary.clone(),
            fields: app_summary_fields(&run.summary),
        });
    }
    items.sort_by_key(|item| item.occurred_at);
    items.truncate(80);
    Ok(items)
}

fn app_summary_fields(summary: &str) -> BTreeMap<String, String> {
    summary
        .split_whitespace()
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn app_run_matches_query(run: &AppRunRow, query: &AppRunsQuery) -> bool {
    if let Some(decision) = query.decision.as_deref()
        && !decision.is_empty()
        && decision != "any"
        && run.decision != decision
    {
        return false;
    }
    if let Some(rule) = query.rule.as_deref()
        && !rule.is_empty()
        && rule != "any"
        && run.rule.as_deref() != Some(rule)
    {
        return false;
    }
    if let Some(q) = query.q.as_deref() {
        let q = q.trim().to_ascii_lowercase();
        if !q.is_empty()
            && ![
                run.id.as_str(),
                run.agent_run_id.as_deref().unwrap_or(""),
                run.summary.as_str(),
                run.trace_id.as_deref().unwrap_or(""),
                run.rule.as_deref().unwrap_or(""),
                run.provider.as_deref().unwrap_or(""),
                run.model.as_deref().unwrap_or(""),
                run.matched_entity.as_deref().unwrap_or(""),
            ]
            .iter()
            .any(|value| value.to_ascii_lowercase().contains(&q))
        {
            return false;
        }
    }
    true
}
