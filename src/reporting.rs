use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::NoetError;
use crate::ledger::{BudgetLedger, TraceReport, TraceReportItem, UsageReport};

#[derive(Debug, Default, Serialize)]
pub struct DashboardUsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub reservations: u64,
    pub active_reservations: u64,
    pub finalized_reservations: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct DashboardDecisionStats {
    pub allow: u64,
    pub warn: u64,
    pub deny: u64,
    pub guard_hits: u64,
    pub lifecycle_guardrails: u64,
}

impl DashboardDecisionStats {
    pub fn total(&self) -> u64 {
        self.allow + self.warn + self.deny
    }
}

#[derive(Debug, Default, Serialize)]
pub struct DashboardActivityCounts {
    pub tools: usize,
    pub agent: usize,
    pub skill_context: usize,
    pub observations: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct DashboardSectionVisibility {
    pub policy: bool,
    pub spend: bool,
    pub evidence: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct DashboardSummary {
    pub usage: DashboardUsageTotals,
    pub decisions: DashboardDecisionStats,
    pub activity: DashboardActivityCounts,
    pub sections: DashboardSectionVisibility,
}

#[derive(Debug, Serialize)]
pub struct DashboardTraceOption {
    pub trace_id: String,
    pub latest_decision_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_decision_kind: Option<String>,
    pub latest_decision_summary: String,
}

#[derive(Debug, Serialize)]
pub struct DashboardReportData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub featured_trace_id: Option<String>,
    pub available_traces: Vec<DashboardTraceOption>,
    pub usage: UsageReport,
    pub decisions: Vec<TraceReportItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceReport>,
    pub observations: Vec<TraceReportItem>,
    pub summary: DashboardSummary,
}

pub fn usage_report(ledger: &BudgetLedger) -> Result<UsageReport, NoetError> {
    ledger.usage_report()
}

pub fn decisions_report(ledger: &BudgetLedger) -> Result<Vec<TraceReportItem>, NoetError> {
    ledger.decisions_report()
}

pub fn trace_report(ledger: &BudgetLedger, trace_id: &str) -> Result<TraceReport, NoetError> {
    ledger.trace_report(trace_id)
}

pub fn observation_kind_prefix(kind: Option<&str>) -> Option<&str> {
    match kind {
        Some("tool") => Some("tool."),
        Some("eval") => Some("eval."),
        Some(value) => Some(value),
        None => None,
    }
}

pub fn observations_report(
    ledger: &BudgetLedger,
    kind: Option<&str>,
    trace_id: Option<&str>,
) -> Result<Vec<TraceReportItem>, NoetError> {
    ledger.observations_report(observation_kind_prefix(kind), trace_id)
}

pub fn dashboard_report(
    ledger: &BudgetLedger,
    requested_trace_id: Option<&str>,
) -> Result<DashboardReportData, NoetError> {
    let usage = usage_report(ledger)?;
    let decisions = decisions_report(ledger)?;
    let featured_trace_id = requested_trace_id.map(ToOwned::to_owned).or_else(|| {
        decisions
            .iter()
            .find_map(|item| summary_value(&item.summary, "trace"))
    });
    let trace = featured_trace_id
        .as_deref()
        .map(|trace_id| trace_report(ledger, trace_id))
        .transpose()?;
    let observations = observations_report(ledger, None, featured_trace_id.as_deref())?;
    let summary = dashboard_summary(&usage, &decisions, trace.as_ref(), &observations);

    Ok(DashboardReportData {
        available_traces: available_traces(&decisions),
        featured_trace_id,
        usage,
        decisions,
        trace,
        observations,
        summary,
    })
}

pub fn summary_value(summary: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    summary
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix).map(ToOwned::to_owned))
}

fn available_traces(decisions: &[TraceReportItem]) -> Vec<DashboardTraceOption> {
    let mut seen = BTreeSet::new();
    let mut traces = Vec::new();
    for item in decisions {
        let Some(trace_id) = summary_value(&item.summary, "trace") else {
            continue;
        };
        if seen.insert(trace_id.clone()) {
            traces.push(DashboardTraceOption {
                trace_id,
                latest_decision_at: item.occurred_at,
                latest_decision_kind: Some(item.kind.clone()),
                latest_decision_summary: item.summary.clone(),
            });
        }
    }
    traces
}

fn dashboard_summary(
    usage: &UsageReport,
    decisions: &[TraceReportItem],
    trace: Option<&TraceReport>,
    observations: &[TraceReportItem],
) -> DashboardSummary {
    let usage_totals = usage_totals(usage);
    let decisions_summary = decision_stats(decisions, trace);
    let activity = activity_counts(trace, observations);
    let has_spend_breakdown = usage.rows.iter().any(|row| row.total_cost_usd > 0.0);
    let spend_section = has_spend_breakdown
        || usage_totals.total_tokens > 0
        || !usage.rows.is_empty()
        || usage.protected_adoption.is_some();

    DashboardSummary {
        usage: usage_totals,
        sections: DashboardSectionVisibility {
            policy: decisions_summary.total() > 0
                || decisions_summary.guard_hits > 0
                || decisions_summary.lifecycle_guardrails > 0,
            spend: spend_section,
            evidence: trace.is_some()
                || !observations.is_empty()
                || activity.tools > 0
                || activity.agent > 0
                || activity.skill_context > 0,
        },
        decisions: decisions_summary,
        activity,
    }
}

fn usage_totals(usage: &UsageReport) -> DashboardUsageTotals {
    let mut totals = DashboardUsageTotals::default();
    for row in &usage.rows {
        totals.input_tokens += row.input_tokens;
        totals.output_tokens += row.output_tokens;
        totals.cache_read_tokens += row.cache_read_tokens;
        totals.cache_write_tokens += row.cache_write_tokens;
        totals.total_tokens += row.total_tokens;
        totals.reservations += row.reservations;
        totals.active_reservations += row.active_reservations;
        totals.finalized_reservations += row.finalized_reservations;
    }
    totals
}

fn decision_stats(
    decisions: &[TraceReportItem],
    trace: Option<&TraceReport>,
) -> DashboardDecisionStats {
    let mut stats = DashboardDecisionStats::default();
    for item in decisions {
        if item.kind.ends_with(".deny") {
            stats.deny += 1;
        } else if item.kind.ends_with(".warn") {
            stats.warn += 1;
        } else if item.kind.ends_with(".allow") {
            stats.allow += 1;
        }
        stats.guard_hits += item
            .guard_hits
            .as_ref()
            .map(|hits| hits.len() as u64)
            .unwrap_or(0);
    }

    stats.lifecycle_guardrails = trace
        .map(|trace| {
            trace
                .items
                .iter()
                .filter(|item| item.kind.starts_with("guard.report_only."))
                .count() as u64
        })
        .unwrap_or_default();
    stats
}

fn activity_counts(
    trace: Option<&TraceReport>,
    observations: &[TraceReportItem],
) -> DashboardActivityCounts {
    let activity = trace
        .map(|trace| trace.items.as_slice())
        .unwrap_or(observations);

    DashboardActivityCounts {
        tools: activity
            .iter()
            .filter(|item| is_tool_kind(&item.kind))
            .count(),
        agent: activity
            .iter()
            .filter(|item| is_agent_kind(&item.kind))
            .count(),
        skill_context: activity
            .iter()
            .filter(|item| is_skill_context_kind(&item.kind))
            .count(),
        observations: observations.len(),
    }
}

fn is_tool_kind(kind: &str) -> bool {
    kind == "tool.observed" || kind == "pi.tool_call"
}

fn is_agent_kind(kind: &str) -> bool {
    matches!(
        kind,
        "pi.provider_call.started"
            | "pi.message_end"
            | "pi.stream_summary"
            | "pi.turn_end"
            | "pi.agent_end"
            | "pi.authorize"
            | "pi.authorize_error"
    )
}

fn is_skill_context_kind(kind: &str) -> bool {
    kind == "pi.agent_context"
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::contract::{AuthorizeRequest, FinalizeReservation, TraceEvent, UsageObservation};

    use super::*;

    #[test]
    fn dashboard_report_assembles_featured_trace_and_summary_counts() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("ledger.sqlite");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let alpha = ledger
            .try_authorize(None, &request("trace-alpha", "req-alpha", "gpt-4.1", 1.25))
            .expect("authorize alpha");
        let alpha_reservation = alpha.reservation.expect("alpha reservation");
        ledger
            .finalize(
                &alpha_reservation.id,
                &finalize_payload("trace-alpha", "gpt-4.1", 1.25, 1_000, 250),
            )
            .expect("finalize alpha");
        ledger
            .record_event(TraceEvent {
                id: Some("evt-alpha-tool".to_owned()),
                trace_id: Some("trace-alpha".to_owned()),
                occurred_at: None,
                kind: "pi.tool_call".to_owned(),
                payload: json!({"name":"bash","success":true}),
            })
            .expect("record alpha tool");

        let beta = ledger
            .try_authorize(
                None,
                &request("trace-beta", "req-beta", "gpt-4.1-mini", 0.75),
            )
            .expect("authorize beta");
        let beta_reservation = beta.reservation.expect("beta reservation");
        ledger
            .finalize(
                &beta_reservation.id,
                &finalize_payload("trace-beta", "gpt-4.1-mini", 0.75, 400, 120),
            )
            .expect("finalize beta");
        ledger
            .record_event(TraceEvent {
                id: Some("evt-beta-agent".to_owned()),
                trace_id: Some("trace-beta".to_owned()),
                occurred_at: None,
                kind: "pi.agent_context".to_owned(),
                payload: json!({"skill":"research"}),
            })
            .expect("record beta context");

        let report = dashboard_report(&ledger, None).expect("dashboard report");

        assert_eq!(report.featured_trace_id.as_deref(), Some("trace-beta"));
        assert_eq!(report.available_traces.len(), 2);
        assert_eq!(report.available_traces[0].trace_id, "trace-beta");
        assert_eq!(report.summary.usage.total_tokens, 1_770);
        assert_eq!(report.summary.decisions.allow, 2);
        assert_eq!(report.summary.activity.skill_context, 1);
        assert_eq!(report.summary.sections.policy, true);
        assert_eq!(report.summary.sections.spend, true);
        assert_eq!(report.summary.sections.evidence, true);
    }

    #[test]
    fn observations_report_maps_short_kind_prefixes() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("ledger.sqlite");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        ledger
            .record_event(TraceEvent {
                id: Some("evt-tool".to_owned()),
                trace_id: Some("trace-a".to_owned()),
                occurred_at: None,
                kind: "tool.observed".to_owned(),
                payload: json!({"name":"bash"}),
            })
            .expect("record tool event");
        ledger
            .record_event(TraceEvent {
                id: Some("evt-eval".to_owned()),
                trace_id: Some("trace-a".to_owned()),
                occurred_at: None,
                kind: "eval.review".to_owned(),
                payload: json!({"label":"good"}),
            })
            .expect("record eval event");

        let tool_items =
            observations_report(&ledger, Some("tool"), Some("trace-a")).expect("tool observations");
        let eval_items =
            observations_report(&ledger, Some("eval"), Some("trace-a")).expect("eval observations");

        assert_eq!(tool_items.len(), 1);
        assert_eq!(tool_items[0].kind, "tool.observed");
        assert_eq!(eval_items.len(), 1);
        assert_eq!(eval_items[0].kind, "eval.review");
    }

    fn request(
        trace_id: &str,
        request_id: &str,
        model: &str,
        estimated_cost_usd: f64,
    ) -> AuthorizeRequest {
        let mut metadata = BTreeMap::new();
        metadata.insert("trace_id".to_owned(), json!(trace_id));
        metadata.insert("request_id".to_owned(), json!(request_id));
        AuthorizeRequest {
            budget_id: None,
            entities: vec!["project:noether".to_owned()],
            subject: Some("user:local".to_owned()),
            project: Some("noether".to_owned()),
            provider: Some("openai".to_owned()),
            model: Some(model.to_owned()),
            estimated_tokens: Some((estimated_cost_usd * 1_000_000.0) as u64),
            estimated_cost_usd: Some(estimated_cost_usd),
            metadata,
        }
    }

    fn finalize_payload(
        trace_id: &str,
        model: &str,
        actual_cost_usd: f64,
        input_tokens: u64,
        output_tokens: u64,
    ) -> FinalizeReservation {
        let mut metadata = BTreeMap::new();
        metadata.insert("trace_id".to_owned(), json!(trace_id));
        FinalizeReservation {
            reservation_id: None,
            usage: Some(UsageObservation {
                provider: Some("openai".to_owned()),
                model: Some(model.to_owned()),
                input_tokens: Some(input_tokens),
                output_tokens: Some(output_tokens),
                total_tokens: Some(input_tokens + output_tokens),
                cost_usd: Some(actual_cost_usd),
                latency_ms: None,
                stop_reason: None,
            }),
            actual_cost_usd: Some(actual_cost_usd),
            metadata,
        }
    }
}
