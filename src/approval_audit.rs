use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::contract::TraceEvent;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Approved,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRiskFlag {
    ActualCostExceededEstimate,
    HighActualCost,
    HighEstimatedCost,
    LifecycleLimitAfterApproval,
    MissingAttribution,
    RepeatedSubjectRuleApproval,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApprovalAuditEvent {
    pub occurred_at: DateTime<Utc>,
    pub outcome: ApprovalOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reservation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risk_flags: Vec<ApprovalRiskFlag>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ApprovalAuditSummary {
    pub total: usize,
    pub approved: usize,
    pub rejected: usize,
    pub high_risk: usize,
    pub repeated_subject_rule_approvals: usize,
    pub missing_attribution: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApprovalAuditReport {
    pub summary: ApprovalAuditSummary,
    pub items: Vec<ApprovalAuditEvent>,
}

#[derive(Clone, Copy, Debug)]
pub struct ApprovalAuditConfig {
    pub high_cost_usd: f64,
    pub cost_overrun_multiplier: f64,
    pub repeated_approval_threshold: usize,
}

impl Default for ApprovalAuditConfig {
    fn default() -> Self {
        Self {
            high_cost_usd: 1.0,
            cost_overrun_multiplier: 2.0,
            repeated_approval_threshold: 3,
        }
    }
}

pub trait ApprovalAuditStore {
    fn approval_audit_events(&self) -> Vec<ApprovalAuditEvent>;
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryApprovalAuditStore {
    events: Vec<ApprovalAuditEvent>,
}

impl InMemoryApprovalAuditStore {
    pub fn new(events: Vec<ApprovalAuditEvent>) -> Self {
        Self { events }
    }

    pub fn from_trace_events(events: &[TraceEvent]) -> Self {
        Self {
            events: events
                .iter()
                .filter_map(ApprovalAuditEvent::from_trace_event)
                .collect(),
        }
    }
}

impl ApprovalAuditStore for InMemoryApprovalAuditStore {
    fn approval_audit_events(&self) -> Vec<ApprovalAuditEvent> {
        self.events.clone()
    }
}

pub fn approval_audit_report(
    store: &impl ApprovalAuditStore,
    config: ApprovalAuditConfig,
) -> ApprovalAuditReport {
    let mut items = store.approval_audit_events();
    let repeated_keys = repeated_approval_keys(&items, config.repeated_approval_threshold);
    for item in &mut items {
        item.risk_flags = risk_flags(item, &repeated_keys, config);
    }
    items.sort_by(|left, right| right.occurred_at.cmp(&left.occurred_at));
    let summary = approval_audit_summary(&items);
    ApprovalAuditReport { summary, items }
}

impl ApprovalAuditEvent {
    pub fn from_trace_event(event: &TraceEvent) -> Option<Self> {
        let outcome = approval_outcome(event)?;
        let payload = event.payload.as_object();
        let request = payload.and_then(|payload| payload.get("request"));
        let metadata = request.and_then(|request| request.get("metadata"));
        let occurred_at = event.occurred_at.unwrap_or_else(Utc::now);

        Some(Self {
            occurred_at,
            outcome,
            trace_id: event
                .trace_id
                .clone()
                .or_else(|| string_path(&event.payload, &["trace_id"]))
                .or_else(|| string_path(metadata?, &["trace_id"])),
            request_id: string_path(&event.payload, &["request_id"])
                .or_else(|| metadata.and_then(|metadata| string_path(metadata, &["request_id"]))),
            agent_run_id: string_path(&event.payload, &["agent_run_id"])
                .or_else(|| metadata.and_then(|metadata| string_path(metadata, &["agent_run_id"]))),
            decision_id: string_path(&event.payload, &["decision_id"]),
            reservation_id: string_path(&event.payload, &["reservation_id"]),
            subject: string_path(&event.payload, &["subject"])
                .or_else(|| request.and_then(|request| string_path(request, &["subject"]))),
            project: string_path(&event.payload, &["project"])
                .or_else(|| request.and_then(|request| string_path(request, &["project"]))),
            rule_id: string_path(&event.payload, &["rule_id"])
                .or_else(|| first_explanation_rule_id(&event.payload)),
            original_action: string_path(&event.payload, &["original_action"])
                .or_else(|| string_path(&event.payload, &["decision_action"])),
            decision_reason: string_path(&event.payload, &["decision_reason"]),
            estimated_cost_usd: number_path(&event.payload, &["estimated_cost_usd"]).or_else(
                || request.and_then(|request| number_path(request, &["estimated_cost_usd"])),
            ),
            actual_cost_usd: number_path(&event.payload, &["actual_cost_usd"]),
            integration: string_path(&event.payload, &["integration"])
                .or_else(|| (event.kind == "pi.authorize").then(|| "pi".to_owned())),
            integration_version: string_path(&event.payload, &["integration_version"]),
            risk_flags: Vec::new(),
        })
    }
}

fn approval_audit_summary(items: &[ApprovalAuditEvent]) -> ApprovalAuditSummary {
    ApprovalAuditSummary {
        total: items.len(),
        approved: items
            .iter()
            .filter(|item| item.outcome == ApprovalOutcome::Approved)
            .count(),
        rejected: items
            .iter()
            .filter(|item| item.outcome == ApprovalOutcome::Rejected)
            .count(),
        high_risk: items
            .iter()
            .filter(|item| !item.risk_flags.is_empty())
            .count(),
        repeated_subject_rule_approvals: items
            .iter()
            .filter(|item| {
                item.risk_flags
                    .contains(&ApprovalRiskFlag::RepeatedSubjectRuleApproval)
            })
            .count(),
        missing_attribution: items
            .iter()
            .filter(|item| {
                item.risk_flags
                    .contains(&ApprovalRiskFlag::MissingAttribution)
            })
            .count(),
    }
}

fn risk_flags(
    item: &ApprovalAuditEvent,
    repeated_keys: &BTreeMap<(String, String), usize>,
    config: ApprovalAuditConfig,
) -> Vec<ApprovalRiskFlag> {
    let mut flags = Vec::new();
    if item.subject.is_none() || item.project.is_none() {
        flags.push(ApprovalRiskFlag::MissingAttribution);
    }
    if item
        .estimated_cost_usd
        .is_some_and(|cost| cost >= config.high_cost_usd)
    {
        flags.push(ApprovalRiskFlag::HighEstimatedCost);
    }
    if item
        .actual_cost_usd
        .is_some_and(|cost| cost >= config.high_cost_usd)
    {
        flags.push(ApprovalRiskFlag::HighActualCost);
    }
    if let (Some(estimated), Some(actual)) = (item.estimated_cost_usd, item.actual_cost_usd)
        && estimated > 0.0
        && actual >= estimated * config.cost_overrun_multiplier
    {
        flags.push(ApprovalRiskFlag::ActualCostExceededEstimate);
    }
    if item.outcome == ApprovalOutcome::Approved
        && item
            .subject
            .as_ref()
            .zip(item.rule_id.as_ref())
            .and_then(|(subject, rule_id)| repeated_keys.get(&(subject.clone(), rule_id.clone())))
            .is_some()
    {
        flags.push(ApprovalRiskFlag::RepeatedSubjectRuleApproval);
    }
    if has_lifecycle_limit_followup(item) {
        flags.push(ApprovalRiskFlag::LifecycleLimitAfterApproval);
    }
    flags.sort();
    flags.dedup();
    flags
}

fn repeated_approval_keys(
    items: &[ApprovalAuditEvent],
    threshold: usize,
) -> BTreeMap<(String, String), usize> {
    if threshold == 0 {
        return BTreeMap::new();
    }
    let mut counts = BTreeMap::new();
    for item in items {
        if item.outcome != ApprovalOutcome::Approved {
            continue;
        }
        let Some(subject) = item.subject.as_ref() else {
            continue;
        };
        let Some(rule_id) = item.rule_id.as_ref() else {
            continue;
        };
        *counts
            .entry((subject.clone(), rule_id.clone()))
            .or_insert(0) += 1;
    }
    counts.retain(|_, count| *count >= threshold);
    counts
}

fn has_lifecycle_limit_followup(item: &ApprovalAuditEvent) -> bool {
    item.decision_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("lifecycle_limit_after_approval=true"))
}

fn approval_outcome(event: &TraceEvent) -> Option<ApprovalOutcome> {
    match event.kind.as_str() {
        "approval.self.approved" => return Some(ApprovalOutcome::Approved),
        "approval.self.rejected" => return Some(ApprovalOutcome::Rejected),
        _ => {}
    }
    match string_path(&event.payload, &["outcome"])
        .or_else(|| string_path(&event.payload, &["user_approval"]))
        .as_deref()
    {
        Some("approved") => Some(ApprovalOutcome::Approved),
        Some("rejected") => Some(ApprovalOutcome::Rejected),
        _ => None,
    }
}

fn first_explanation_rule_id(value: &Value) -> Option<String> {
    value
        .get("explanations")?
        .as_array()?
        .iter()
        .filter_map(|explanation| string_path(explanation, &["rule_id"]))
        .next()
}

fn string_path(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(ToOwned::to_owned)
}

fn number_path(value: &Value, path: &[&str]) -> Option<f64> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_f64()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_existing_pi_authorize_user_approval_events() {
        let event = TraceEvent {
            id: Some("evt-approval".to_owned()),
            trace_id: Some("trace-1".to_owned()),
            occurred_at: Some(Utc.with_ymd_and_hms(2026, 5, 29, 12, 0, 0).unwrap()),
            kind: "pi.authorize".to_owned(),
            payload: json!({
                "request": {
                    "subject": "user:alice",
                    "project": "noether",
                    "estimated_cost_usd": 1.25,
                    "metadata": {
                        "request_id": "req-1",
                        "agent_run_id": "run-1"
                    }
                },
                "decision_action": "ask",
                "policy_action": "approved",
                "user_approval": "approved",
                "decision_reason": "tool access requires explicit approval",
                "explanations": [
                    { "rule_id": "restricted-tool", "reason": "approval required" }
                ]
            }),
        };

        let parsed = ApprovalAuditEvent::from_trace_event(&event).expect("approval event");

        assert_eq!(parsed.outcome, ApprovalOutcome::Approved);
        assert_eq!(parsed.integration.as_deref(), Some("pi"));
        assert_eq!(parsed.trace_id.as_deref(), Some("trace-1"));
        assert_eq!(parsed.request_id.as_deref(), Some("req-1"));
        assert_eq!(parsed.agent_run_id.as_deref(), Some("run-1"));
        assert_eq!(parsed.subject.as_deref(), Some("user:alice"));
        assert_eq!(parsed.project.as_deref(), Some("noether"));
        assert_eq!(parsed.rule_id.as_deref(), Some("restricted-tool"));
        assert_eq!(parsed.original_action.as_deref(), Some("ask"));
        assert_eq!(parsed.estimated_cost_usd, Some(1.25));
    }

    #[test]
    fn parses_normalized_approval_events() {
        let event = TraceEvent {
            id: Some("evt-rejected".to_owned()),
            trace_id: None,
            occurred_at: Some(Utc.with_ymd_and_hms(2026, 5, 29, 12, 1, 0).unwrap()),
            kind: "approval.self.rejected".to_owned(),
            payload: json!({
                "trace_id": "trace-2",
                "request_id": "req-2",
                "decision_id": "dec-2",
                "reservation_id": "res-2",
                "subject": "user:bob",
                "project": "checkout",
                "rule_id": "expensive-request",
                "original_action": "ask",
                "decision_reason": "estimated request cost requires confirmation",
                "estimated_cost_usd": 0.75,
                "integration": "custom-harness",
                "integration_version": "0.2.0"
            }),
        };

        let parsed = ApprovalAuditEvent::from_trace_event(&event).expect("approval event");

        assert_eq!(parsed.outcome, ApprovalOutcome::Rejected);
        assert_eq!(parsed.trace_id.as_deref(), Some("trace-2"));
        assert_eq!(parsed.decision_id.as_deref(), Some("dec-2"));
        assert_eq!(parsed.reservation_id.as_deref(), Some("res-2"));
        assert_eq!(parsed.integration.as_deref(), Some("custom-harness"));
        assert_eq!(parsed.integration_version.as_deref(), Some("0.2.0"));
    }

    #[test]
    fn report_flags_repeated_high_cost_and_missing_attribution() {
        let occurred_at = Utc.with_ymd_and_hms(2026, 5, 29, 12, 0, 0).unwrap();
        let events = vec![
            approved_event(
                occurred_at,
                "user:alice",
                Some("noether"),
                "rule-a",
                0.2,
                None,
            ),
            approved_event(
                occurred_at,
                "user:alice",
                Some("noether"),
                "rule-a",
                1.2,
                Some(3.0),
            ),
            approved_event(
                occurred_at,
                "user:alice",
                Some("noether"),
                "rule-a",
                0.3,
                Some(0.3),
            ),
            approved_event(occurred_at, "user:bob", None, "rule-b", 0.1, None),
            ApprovalAuditEvent {
                outcome: ApprovalOutcome::Rejected,
                subject: Some("user:bob".to_owned()),
                project: Some("checkout".to_owned()),
                rule_id: Some("rule-c".to_owned()),
                ..approved_event(
                    occurred_at,
                    "user:bob",
                    Some("checkout"),
                    "rule-c",
                    0.1,
                    None,
                )
            },
        ];
        let store = InMemoryApprovalAuditStore::new(events);

        let report = approval_audit_report(&store, ApprovalAuditConfig::default());

        assert_eq!(report.summary.total, 5);
        assert_eq!(report.summary.approved, 4);
        assert_eq!(report.summary.rejected, 1);
        assert_eq!(report.summary.repeated_subject_rule_approvals, 3);
        assert_eq!(report.summary.missing_attribution, 1);
        assert!(report.items.iter().any(|item| {
            item.risk_flags
                .contains(&ApprovalRiskFlag::ActualCostExceededEstimate)
        }));
        assert!(report.items.iter().any(|item| {
            item.risk_flags
                .contains(&ApprovalRiskFlag::HighEstimatedCost)
        }));
    }

    #[test]
    fn memory_store_can_be_seeded_from_trace_events() {
        let trace_events = vec![
            TraceEvent {
                id: None,
                trace_id: Some("trace-1".to_owned()),
                occurred_at: Some(Utc.with_ymd_and_hms(2026, 5, 29, 12, 0, 0).unwrap()),
                kind: "approval.self.approved".to_owned(),
                payload: json!({
                    "subject": "user:alice",
                    "project": "noether",
                    "rule_id": "rule-a"
                }),
            },
            TraceEvent {
                id: None,
                trace_id: Some("trace-ignored".to_owned()),
                occurred_at: Some(Utc.with_ymd_and_hms(2026, 5, 29, 12, 0, 1).unwrap()),
                kind: "tool.observed".to_owned(),
                payload: json!({ "name": "bash" }),
            },
        ];

        let store = InMemoryApprovalAuditStore::from_trace_events(&trace_events);
        let report = approval_audit_report(&store, ApprovalAuditConfig::default());

        assert_eq!(report.summary.total, 1);
        assert_eq!(report.items[0].trace_id.as_deref(), Some("trace-1"));
    }

    fn approved_event(
        occurred_at: DateTime<Utc>,
        subject: &str,
        project: Option<&str>,
        rule_id: &str,
        estimated_cost_usd: f64,
        actual_cost_usd: Option<f64>,
    ) -> ApprovalAuditEvent {
        ApprovalAuditEvent {
            occurred_at,
            outcome: ApprovalOutcome::Approved,
            trace_id: Some(format!("trace-{subject}-{rule_id}-{estimated_cost_usd}")),
            request_id: None,
            agent_run_id: None,
            decision_id: None,
            reservation_id: None,
            subject: Some(subject.to_owned()),
            project: project.map(ToOwned::to_owned),
            rule_id: Some(rule_id.to_owned()),
            original_action: Some("ask".to_owned()),
            decision_reason: Some("approval requested".to_owned()),
            estimated_cost_usd: Some(estimated_cost_usd),
            actual_cost_usd,
            integration: Some("stub".to_owned()),
            integration_version: None,
            risk_flags: Vec::new(),
        }
    }
}
