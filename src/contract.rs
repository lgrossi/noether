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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
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
    pub action: PolicyAction,
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
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_warn_at_fraction")]
    pub warn_at_fraction: f64,
    #[serde(default = "default_window_seconds")]
    pub window_seconds: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_mode: Option<BudgetWindowMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_anchor: Option<WindowAnchorPolicy>,
    #[serde(default)]
    pub eligible: BudgetEligibility,
    #[serde(default)]
    pub models: BudgetModelPolicy,
    #[serde(default)]
    pub limits: BudgetLimitPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocation: Option<BudgetAllocationPolicy>,
    #[serde(default, rename = "match")]
    pub rule_match: RuleMatch,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BudgetEligibility {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BudgetModelPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BudgetLimitPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_cost: Option<RequestCostLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<ContextTokenLimit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spend: Vec<SpendWindowLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_steps: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestCostLimit {
    pub max_usd: f64,
    #[serde(default = "default_limit_action")]
    pub action: PolicyAction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContextTokenLimit {
    pub max_tokens: u64,
    #[serde(default = "default_limit_action")]
    pub action: PolicyAction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SpendWindowLimit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub window: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SpendWindowMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<WindowAnchorPolicy>,
    pub max_usd: f64,
    #[serde(default = "default_limit_action")]
    pub action: PolicyAction,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetWindowMode {
    Tumbling,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendWindowMode {
    Tumbling,
    Rolling,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WindowAnchorPolicy {
    pub kind: WindowAnchorKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAnchorKind {
    FirstSeen,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BudgetAllocationPolicy {
    pub standard: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected_amount_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carryover: Option<ProtectedCarryoverPolicy>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProtectedCarryoverPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
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
    pub action: PolicyAction,
    pub reason: String,
    #[serde(default)]
    pub when: PolicyCondition,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Allow,
    Warn,
    Block,
    Ask,
}

impl PolicyAction {
    pub fn decision_outcome(self) -> DecisionOutcome {
        match self {
            Self::Allow => DecisionOutcome::Allow,
            Self::Warn => DecisionOutcome::Warn,
            Self::Block | Self::Ask => DecisionOutcome::Deny,
        }
    }

    pub fn decision_severity(self) -> DecisionSeverity {
        match self {
            Self::Allow => DecisionSeverity::Info,
            Self::Warn => DecisionSeverity::Warn,
            Self::Block | Self::Ask => DecisionSeverity::Deny,
        }
    }

    pub fn halts_request(self) -> bool {
        matches!(self, Self::Block | Self::Ask)
    }
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

fn default_limit_action() -> PolicyAction {
    PolicyAction::Block
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_request_accepts_legacy_shape_without_budget_or_entities() {
        let request: AuthorizeRequest = serde_json::from_value(serde_json::json!({
            "subject": "user:alice",
            "project": "noether",
            "provider": "openai",
            "model": "gpt-example",
            "estimated_cost_usd": 0.01,
            "metadata": { "trace_id": "trace-1" }
        }))
        .expect("legacy request");

        assert_eq!(request.budget_id, None);
        assert!(request.entities.is_empty());
        assert_eq!(request.project.as_deref(), Some("noether"));
    }

    #[test]
    fn authorize_request_accepts_budget_and_entities() {
        let request: AuthorizeRequest = serde_json::from_value(serde_json::json!({
            "budget_id": "project-noether",
            "entities": ["project:noether", "user:alice"],
            "metadata": { "trace_id": "trace-1" }
        }))
        .expect("request with attribution");

        assert_eq!(request.budget_id.as_deref(), Some("project-noether"));
        assert_eq!(request.entities, vec!["project:noether", "user:alice"]);
    }

    #[test]
    fn authorize_request_rejects_malformed_entities_field() {
        let error = serde_json::from_value::<AuthorizeRequest>(serde_json::json!({
            "entities": "project:noether"
        }))
        .expect_err("malformed entities should fail closed");

        assert!(error.to_string().contains("invalid type"));
    }

    #[test]
    fn malformed_metadata_entities_do_not_populate_contract_entities() {
        let request: AuthorizeRequest = serde_json::from_value(serde_json::json!({
            "metadata": { "entities": "project:noether" }
        }))
        .expect("metadata remains opaque");

        assert!(request.entities.is_empty());
        assert_eq!(
            request.metadata.get("entities"),
            Some(&Value::String("project:noether".to_owned()))
        );
    }

    #[test]
    fn budget_rule_contract_preserves_legacy_window_shape() {
        let rule: BudgetRule = serde_yaml::from_str(
            r#"
id: legacy-budget
limit_usd: 10
window_seconds: 3600
"#,
        )
        .expect("legacy rule parses");

        let encoded = serde_yaml::to_string(&rule).expect("legacy rule serializes");
        assert!(!encoded.contains("window_mode:"));
        assert!(!encoded.contains("window_anchor:"));
        assert_eq!(rule.window_mode, None);
        assert_eq!(rule.window_anchor, None);
    }

    #[test]
    fn budget_rule_contract_round_trips_explicit_window_shape() {
        let rule: BudgetRule = serde_yaml::from_str(
            r#"
id: explicit-budget
limit_usd: 10
window_seconds: 3600
window_mode: tumbling
window_anchor:
  kind: first_seen
limits:
  spend:
    - id: daily-cap
      window: 1d
      mode: tumbling
      anchor:
        kind: first_seen
      max_usd: 2
      action: warn
"#,
        )
        .expect("explicit rule parses");

        assert_eq!(rule.window_mode, Some(BudgetWindowMode::Tumbling));
        assert_eq!(
            rule.window_anchor,
            Some(WindowAnchorPolicy {
                kind: WindowAnchorKind::FirstSeen,
            })
        );
        assert_eq!(rule.limits.spend[0].id.as_deref(), Some("daily-cap"));
        assert_eq!(rule.limits.spend[0].mode, Some(SpendWindowMode::Tumbling));
        assert_eq!(
            rule.limits.spend[0].anchor,
            Some(WindowAnchorPolicy {
                kind: WindowAnchorKind::FirstSeen,
            })
        );

        let encoded = serde_yaml::to_string(&rule).expect("explicit rule serializes");
        let decoded: BudgetRule = serde_yaml::from_str(&encoded).expect("explicit rule re-parses");
        assert_eq!(decoded.window_mode, Some(BudgetWindowMode::Tumbling));
        assert_eq!(
            decoded.limits.spend[0].mode,
            Some(SpendWindowMode::Tumbling)
        );
    }
}
