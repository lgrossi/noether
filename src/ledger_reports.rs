use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::contract::{AuthorizeRequest, DecisionOutcome, DecisionSeverity, SpendWindowMode};

#[derive(Debug, Deserialize, Serialize)]
pub struct UsageReport {
    pub total_cost_usd: f64,
    pub rows: Vec<UsageReportRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected_adoption: Option<ProtectedAdoptionReport>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UsageReportRow {
    pub subject: Option<String>,
    pub project: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub cache_read_cost_usd: f64,
    pub cache_write_cost_usd: f64,
    pub total_cost_usd: f64,
    pub reservations: u64,
    pub active_reservations: u64,
    pub finalized_reservations: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageActivityRecord {
    pub occurred_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_budget_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_entity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Default)]
pub struct RunTotalsReport {
    pub runs: u64,
    pub allow: u64,
    pub warn: u64,
    pub deny: u64,
    pub ask: u64,
    pub limit_hits: u64,
    pub spend_usd: f64,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RuleStatsReport {
    pub rule: String,
    pub allow: u64,
    pub warn: u64,
    pub deny: u64,
    pub ask: u64,
    pub limit_hits: u64,
    pub top_reason: Option<String>,
    pub top_model: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoricalAuthorizeRequest {
    pub occurred_at: DateTime<Utc>,
    pub decision_id: String,
    pub baseline_outcome: DecisionOutcome,
    pub request: AuthorizeRequest,
}

#[derive(Clone, Debug)]
pub struct ReplaySpendSeed {
    pub rule_id: String,
    pub limit_id: String,
    pub scope_key: String,
    pub amount_usd: f64,
    pub mode: SpendWindowMode,
    pub seeded_at: DateTime<Utc>,
    pub window_started_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct SpendScopeTotal {
    pub scope_key: String,
    pub amount_usd: f64,
    pub window_started_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProtectedAdoptionReport {
    pub unused_protected_opportunity_usd: f64,
    pub carryover_liability_usd: f64,
    pub low_adopters: Vec<ProtectedAdoptionEntityReport>,
    pub high_adopters: Vec<ProtectedAdoptionEntityReport>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProtectedAdoptionEntityReport {
    pub budget_id: String,
    pub entity_key: String,
    pub protected_amount_usd: f64,
    pub current_grant_usd: f64,
    pub carryover_usd: f64,
    pub used_current_grant_usd: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TraceReport {
    pub trace_id: String,
    pub items: Vec<TraceReportItem>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TraceReportItem {
    pub occurred_at: DateTime<Utc>,
    pub kind: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing: Option<DecisionRoutingReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_hits: Option<Vec<DecisionLimitHitReport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_limit: Option<DecisionLimitHitReport>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DecisionRoutingReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_budget_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_budget_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_budget_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_window_remaining_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_window_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_window_started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_window_ends_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DecisionLimitHitReport {
    pub rule_id: String,
    pub reason: String,
    pub severity: DecisionSeverity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_ends_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_spend_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_entity: Option<String>,
}
