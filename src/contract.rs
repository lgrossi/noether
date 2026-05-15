use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMode {
    #[default]
    DryRun,
    Enforce,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthorizeRequest {
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub estimated_tokens: Option<u64>,
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl AuthorizeRequest {
    pub fn estimated_cost(&self) -> f64 {
        self.estimated_cost_usd
            .or_else(|| {
                self.estimated_tokens
                    .map(|tokens| tokens as f64 * 0.000_001)
            })
            .unwrap_or(0.0)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthorizeDecision {
    pub decision_id: String,
    pub outcome: DecisionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reservation: Option<Reservation>,
    pub explanations: Vec<DecisionExplanation>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Allow,
    Warn,
    Deny,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DecisionExplanation {
    pub rule_id: String,
    pub reason: String,
    pub severity: DecisionSeverity,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSeverity {
    Info,
    Warn,
    Deny,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Reservation {
    pub id: String,
    pub amount_usd: f64,
    pub currency: String,
    pub status: ReservationStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationStatus {
    Active,
    Finalized,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FinalizeReservation {
    #[serde(default)]
    pub reservation_id: Option<String>,
    #[serde(default)]
    pub usage: Option<UsageObservation>,
    #[serde(default)]
    pub actual_cost_usd: Option<f64>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UsageObservation {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TraceEvent {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
    #[serde(default)]
    pub occurred_at: Option<DateTime<Utc>>,
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolEvent {
    pub name: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EvalAnnotation {
    pub label: String,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub annotator: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BudgetRule {
    pub id: String,
    pub limit_usd: f64,
    #[serde(default = "default_warn_at_fraction")]
    pub warn_at_fraction: f64,
    #[serde(default = "default_window_seconds")]
    pub window_seconds: i64,
    #[serde(default, rename = "match")]
    pub rule_match: RuleMatch,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RuleMatch {
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PolicyRule {
    pub id: String,
    pub effect: PolicyEffect,
    pub reason: String,
    #[serde(default)]
    pub when: PolicyCondition,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEffect {
    Allow,
    Warn,
    Deny,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PolicyCondition {
    #[serde(default)]
    pub missing: Option<String>,
    #[serde(default, rename = "match")]
    pub rule_match: RuleMatch,
}

fn default_warn_at_fraction() -> f64 {
    0.8
}

fn default_window_seconds() -> i64 {
    86_400
}
