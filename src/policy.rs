use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::contract::{
    AuthorizeRequest, BudgetRule, DecisionExplanation, PolicyAction, PolicyRule, RuleMatch,
    SpendWindowBy, SpendWindowLimit, SpendWindowMode,
};
use crate::error::NoetError;

const MAX_ROLLING_SPEND_WINDOW_SECONDS: i64 = 60 * 60;
const MIN_ROLLING_SPEND_WINDOW_SECONDS: i64 = 10;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PolicyFile {
    pub version: u16,
    #[serde(default)]
    pub routing: RoutingPolicy,
    #[serde(default)]
    pub budgets: Vec<BudgetRule>,
    #[serde(default, rename = "policies")]
    pub policies: Vec<PolicyRule>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoutingPolicy {
    #[serde(default = "default_routing_mode")]
    pub mode: String,
    #[serde(
        default = "default_specificity",
        rename = "fallback_order",
        alias = "specificity"
    )]
    pub specificity: Vec<String>,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            mode: default_routing_mode(),
            specificity: default_specificity(),
        }
    }
}

pub async fn load_policy(path: &Path) -> Result<PolicyFile, NoetError> {
    let bytes = fs::read(path).await?;
    parse_policy_bytes(&bytes)
}

pub fn parse_policy_bytes(bytes: &[u8]) -> Result<PolicyFile, NoetError> {
    let policy: PolicyFile = serde_yaml::from_slice(bytes)?;
    validate_policy(&policy)?;
    Ok(policy)
}

pub fn validate_policy(policy: &PolicyFile) -> Result<(), NoetError> {
    let mut errors = Vec::new();
    if policy.version != 0 {
        errors.push(format!("version must be 0, got {}", policy.version));
    }
    if policy.routing.mode != "explicit_then_fallback" {
        errors.push(format!(
            "routing.mode must be explicit_then_fallback, got {}",
            policy.routing.mode
        ));
    }
    for budget in &policy.budgets {
        if budget.id.trim().is_empty() {
            errors.push("budget id must not be empty".to_owned());
        }
        if budget.limits.spend.is_empty() {
            errors.push(format!(
                "budget {} must define at least one limits.spend window",
                budget.id
            ));
        }
        validate_rule_match(
            &format!("budget {}", budget.id),
            &budget.rule_match,
            &mut errors,
        );
        for pattern in &budget.models.allow {
            if pattern.trim().is_empty() {
                errors.push(format!(
                    "budget {} models.allow must not contain empty values",
                    budget.id
                ));
            }
        }
        if let Some(limit) = &budget.limits.request_cost {
            if !limit.max_usd.is_finite() || limit.max_usd <= 0.0 {
                errors.push(format!(
                    "budget {} limits.request_cost.max_usd must be positive",
                    budget.id
                ));
            }
            if !limit_action_is_supported(limit.action) {
                errors.push(format!(
                    "budget {} limits.request_cost.action must be warn, ask, or block",
                    budget.id
                ));
            }
            if let Some(cadence) = limit.warning_cadence.as_deref()
                && parse_limit_window(cadence).is_none()
            {
                errors.push(format!(
                    "budget {} limits.request_cost.warning_cadence must use <number><s|m|h|d>, got {cadence}",
                    budget.id
                ));
            }
        }
        if let Some(limit) = &budget.limits.context_tokens {
            if limit.max_tokens == 0 {
                errors.push(format!(
                    "budget {} limits.context_tokens.max_tokens must be positive",
                    budget.id
                ));
            }
            if !limit_action_is_supported(limit.action) {
                errors.push(format!(
                    "budget {} limits.context_tokens.action must be warn, ask, or block",
                    budget.id
                ));
            }
            if let Some(cadence) = limit.warning_cadence.as_deref()
                && parse_limit_window(cadence).is_none()
            {
                errors.push(format!(
                    "budget {} limits.context_tokens.warning_cadence must use <number><s|m|h|d>, got {cadence}",
                    budget.id
                ));
            }
        }
        let mut spend_window_ids = BTreeSet::new();
        for limit in &budget.limits.spend {
            let parsed_window = parse_limit_window(&limit.window);
            if parsed_window.is_none() {
                errors.push(format!(
                    "budget {} limits.spend.window must use <number><s|m|h|d>, got {}",
                    budget.id, limit.window
                ));
            }
            if matches!(limit.mode, Some(SpendWindowMode::Rolling))
                && parsed_window.is_some_and(|window| {
                    let seconds = window.num_seconds();
                    !(MIN_ROLLING_SPEND_WINDOW_SECONDS..=MAX_ROLLING_SPEND_WINDOW_SECONDS)
                        .contains(&seconds)
                })
            {
                errors.push(format!(
                    "budget {} limits.spend[{}].window must be between 10s and 1h when mode is rolling",
                    budget.id,
                    spend_window_label(limit)
                ));
            }
            if matches!(limit.by, SpendWindowBy::User) && budget.allocation.is_some() {
                // allowed; protected adoption and spend scoping can both be user-based
            }
            if !limit.max_usd.is_finite() || limit.max_usd <= 0.0 {
                errors.push(format!(
                    "budget {} limits.spend.max_usd must be positive",
                    budget.id
                ));
            }
            if limit.warn_at_fractions.is_empty() {
                errors.push(format!(
                    "budget {} limits.spend.warn_at_fraction must include at least one threshold",
                    budget.id
                ));
            }
            for warn_at_fraction in &limit.warn_at_fractions {
                if !(0.0..=1.0).contains(warn_at_fraction) {
                    errors.push(format!(
                        "budget {} limits.spend.warn_at_fraction must be between 0 and 1",
                        budget.id
                    ));
                }
            }
            if !limit_action_is_supported(limit.action) {
                errors.push(format!(
                    "budget {} limits.spend.action must be warn, ask, or block",
                    budget.id
                ));
            }
            if let Some(cadence) = limit.warning_cadence.as_deref()
                && parse_limit_window(cadence).is_none()
            {
                errors.push(format!(
                    "budget {} limits.spend[{}].warning_cadence must use <number><s|m|h|d>, got {cadence}",
                    budget.id,
                    spend_window_label(limit)
                ));
            }
            if let Some(id) = limit.id.as_deref() {
                if id.trim().is_empty() {
                    errors.push(format!(
                        "budget {} limits.spend.id must not be empty",
                        budget.id
                    ));
                } else if !spend_window_ids.insert(id.to_owned()) {
                    errors.push(format!(
                        "budget {} limits.spend ids must be unique, found duplicate id {}",
                        budget.id, id
                    ));
                }
            }
            match (limit.mode, limit.anchor.as_ref()) {
                (None, _) => errors.push(format!(
                    "budget {} limits.spend[{}].mode is required",
                    budget.id,
                    spend_window_label(limit)
                )),
                (Some(SpendWindowMode::Tumbling), None) => errors.push(format!(
                    "budget {} limits.spend[{}].anchor is required when mode is tumbling",
                    budget.id,
                    spend_window_label(limit)
                )),
                (Some(SpendWindowMode::Rolling), Some(_)) => errors.push(format!(
                    "budget {} limits.spend[{}].anchor requires mode tumbling",
                    budget.id,
                    spend_window_label(limit)
                )),
                _ => {}
            }
        }
        if budget.limits.tool_calls == Some(0) {
            errors.push(format!(
                "budget {} limits.tool_calls must be positive",
                budget.id
            ));
        }
        if budget.limits.agent_steps == Some(0) {
            errors.push(format!(
                "budget {} limits.agent_steps must be positive",
                budget.id
            ));
        }
        if budget.limits.retries == Some(0) {
            errors.push(format!(
                "budget {} limits.retries must be positive",
                budget.id
            ));
        }
        if let Some(allocation) = &budget.allocation
            && allocation.standard == "protected_adoption_pool"
        {
            if !matches!(allocation.by.as_deref(), Some("user" | "team")) {
                errors.push(format!(
                    "budget {} allocation.by must be user or team for protected_adoption_pool",
                    budget.id
                ));
            }
            if allocation
                .protected_amount_usd
                .is_none_or(|value| !value.is_finite() || value <= 0.0)
            {
                errors.push(format!(
                    "budget {} allocation.protected_amount_usd must be positive for protected_adoption_pool",
                    budget.id
                ));
            }
            if !matches!(allocation.window.as_deref(), Some("monthly")) {
                errors.push(format!(
                    "budget {} allocation.window must be monthly for protected_adoption_pool",
                    budget.id
                ));
            }
            let Some(carryover) = allocation.carryover.as_ref() else {
                errors.push(format!(
                    "budget {} allocation.carryover is required for protected_adoption_pool",
                    budget.id
                ));
                continue;
            };
            if carryover
                .percent
                .is_none_or(|value| !value.is_finite() || !(0.0..=100.0).contains(&value))
            {
                errors.push(format!(
                    "budget {} allocation.carryover.percent must be between 0 and 100 for protected_adoption_pool",
                    budget.id
                ));
            }
            if carryover
                .cap_usd
                .is_none_or(|value| !value.is_finite() || value < 0.0)
            {
                errors.push(format!(
                    "budget {} allocation.carryover.cap_usd must be zero or positive for protected_adoption_pool",
                    budget.id
                ));
            }
        }
    }

    for policy_rule in &policy.policies {
        if policy_rule.id.trim().is_empty() {
            errors.push("policy id must not be empty".to_owned());
        }
        if policy_rule.reason.trim().is_empty() {
            errors.push(format!(
                "policy {} reason must not be empty",
                policy_rule.id
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(NoetError::InvalidPolicy(errors.join("; ")))
    }
}

pub fn policy_validation_warnings(policy: &PolicyFile) -> Vec<String> {
    let _ = policy;
    Vec::new()
}

pub fn matching_policy_explanations(
    policy: &PolicyFile,
    request: &AuthorizeRequest,
) -> Vec<(PolicyAction, DecisionExplanation)> {
    policy
        .policies
        .iter()
        .filter(|rule| policy_rule_matches(rule, request))
        .map(|rule| {
            let severity = rule.action.decision_severity();
            (
                rule.action,
                DecisionExplanation {
                    rule_id: rule.id.clone(),
                    reason: rule.reason.clone(),
                    severity,
                },
            )
        })
        .collect()
}

pub fn specificity_order(policy: &PolicyFile) -> Vec<String> {
    if policy.routing.specificity.is_empty() {
        default_specificity()
    } else {
        policy.routing.specificity.clone()
    }
}

pub fn budget_rule_matches(rule: &BudgetRule, request: &AuthorizeRequest) -> bool {
    budget_scope_matches(rule, request) && budget_model_allowed(rule, request)
}

pub fn budget_scope_matches(rule: &BudgetRule, request: &AuthorizeRequest) -> bool {
    rule_match_matches(&rule.rule_match, request)
}

pub fn budget_model_allowed(rule: &BudgetRule, request: &AuthorizeRequest) -> bool {
    if rule.models.allow.is_empty() {
        return true;
    }

    let Some(provider) = request.provider.as_deref() else {
        return false;
    };
    let Some(model) = request.model.as_deref() else {
        return false;
    };
    let provider_model = format!("{provider}:{model}");

    rule.models
        .allow
        .iter()
        .any(|pattern| model_pattern_matches(pattern, &provider_model))
}

pub fn rule_match_matches(rule_match: &RuleMatch, request: &AuthorizeRequest) -> bool {
    let base_match = matches_optional(&rule_match.subject, &request.subject)
        && matches_user_optional(&rule_match.user, request)
        && matches_project_optional(&rule_match.project, request)
        && matches_entity_kind_optional(request, "team", &rule_match.team)
        && matches_entity_kind_optional(request, "group", &rule_match.group)
        && matches_entity_kind_optional(request, "org", &rule_match.org)
        && matches_entity_kind_optional(request, "workflow", &rule_match.workflow)
        && matches_entity_kind_optional(request, "surface", &rule_match.surface)
        && matches_optional(&rule_match.provider, &request.provider)
        && matches_optional(&rule_match.model, &request.model);
    let any_match = rule_match.any.is_empty()
        || rule_match
            .any
            .iter()
            .any(|nested| rule_match_matches(nested, request));
    let not_match = rule_match
        .not
        .as_deref()
        .is_none_or(|nested| !rule_match_matches(nested, request));
    base_match && any_match && not_match
}

fn model_pattern_matches(pattern: &str, provider_model: &str) -> bool {
    let pattern = pattern.trim();
    if let Some(prefix) = pattern.strip_suffix('*') {
        provider_model
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    } else {
        provider_model.eq_ignore_ascii_case(pattern)
    }
}

fn request_matches_entity(request: &AuthorizeRequest, expected: &str) -> bool {
    if expected.eq_ignore_ascii_case("global") {
        return true;
    }

    request
        .entities
        .iter()
        .any(|entity| entity.eq_ignore_ascii_case(expected))
        || legacy_request_entities(request)
            .iter()
            .any(|entity| entity.eq_ignore_ascii_case(expected))
}

fn legacy_request_entities(request: &AuthorizeRequest) -> Vec<String> {
    let mut entities = Vec::new();
    if let Some(project) = request.project.as_deref().filter(|value| !value.is_empty()) {
        entities.push(format!("project:{project}"));
    }
    if let Some(subject) = request.subject.as_deref().filter(|value| !value.is_empty()) {
        entities.push(if subject.contains(':') {
            subject.to_owned()
        } else {
            format!("user:{subject}")
        });
    }
    entities
}

fn policy_rule_matches(rule: &PolicyRule, request: &AuthorizeRequest) -> bool {
    if !rule_match_matches(&rule.when.rule_match, request) {
        return false;
    }

    match rule.when.missing.as_deref() {
        Some("subject") => request.subject.as_deref().is_none_or(str::is_empty),
        Some("project") => request.project.as_deref().is_none_or(str::is_empty),
        Some("provider") => request.provider.as_deref().is_none_or(str::is_empty),
        Some("model") => request.model.as_deref().is_none_or(str::is_empty),
        Some("estimated_cost_usd") => request.estimated_cost_usd.is_none(),
        Some("estimated_tokens") => request.estimated_tokens.is_none(),
        Some(_) => false,
        None => true,
    }
}

fn matches_optional(expected: &Option<String>, actual: &Option<String>) -> bool {
    expected.as_ref().is_none_or(|expected| {
        actual
            .as_ref()
            .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
    })
}

fn matches_user_optional(expected: &Option<String>, request: &AuthorizeRequest) -> bool {
    let Some(expected) = expected.as_deref() else {
        return true;
    };
    request_matches_entity(request, &format!("user:{expected}"))
        || request.subject.as_deref().is_some_and(|subject| {
            subject.eq_ignore_ascii_case(expected)
                || subject.eq_ignore_ascii_case(&format!("user:{expected}"))
        })
}

fn matches_project_optional(expected: &Option<String>, request: &AuthorizeRequest) -> bool {
    let Some(expected) = expected.as_deref() else {
        return true;
    };
    request_matches_entity(request, &format!("project:{expected}"))
        || request
            .project
            .as_deref()
            .is_some_and(|project| project.eq_ignore_ascii_case(expected))
}

fn matches_entity_kind_optional(
    request: &AuthorizeRequest,
    kind: &str,
    expected: &Option<String>,
) -> bool {
    let Some(expected) = expected.as_deref() else {
        return true;
    };
    request_matches_entity(request, &format!("{kind}:{expected}"))
}

fn validate_rule_match(prefix: &str, rule_match: &RuleMatch, errors: &mut Vec<String>) {
    for (label, value) in [
        ("subject", rule_match.subject.as_deref()),
        ("user", rule_match.user.as_deref()),
        ("project", rule_match.project.as_deref()),
        ("team", rule_match.team.as_deref()),
        ("group", rule_match.group.as_deref()),
        ("org", rule_match.org.as_deref()),
        ("workflow", rule_match.workflow.as_deref()),
        ("surface", rule_match.surface.as_deref()),
        ("provider", rule_match.provider.as_deref()),
        ("model", rule_match.model.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            errors.push(format!("{prefix} {label} must not be empty"));
        }
    }
    if rule_match.subject.is_some() && rule_match.user.is_some() {
        errors.push(format!(
            "{prefix} match.subject and match.user cannot both be set"
        ));
    }
    for (index, nested) in rule_match.any.iter().enumerate() {
        validate_rule_match(&format!("{prefix} match.any[{index}]"), nested, errors);
    }
    if let Some(nested) = rule_match.not.as_deref() {
        validate_rule_match(&format!("{prefix} match.not"), nested, errors);
    }
}

fn default_routing_mode() -> String {
    "explicit_then_fallback".to_owned()
}

fn default_specificity() -> Vec<String> {
    ["project", "user", "team", "group", "org", "global"]
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect()
}

fn limit_action_is_supported(action: PolicyAction) -> bool {
    matches!(
        action,
        PolicyAction::Warn | PolicyAction::Ask | PolicyAction::Block
    )
}

fn spend_window_label(limit: &SpendWindowLimit) -> &str {
    limit.id.as_deref().unwrap_or(limit.window.as_str())
}

pub fn parse_limit_window(value: &str) -> Option<chrono::Duration> {
    let trimmed = value.trim();
    let (amount, unit) = trimmed.split_at(trimmed.len().checked_sub(1)?);
    let amount: i64 = amount.parse().ok()?;
    if amount <= 0 {
        return None;
    }
    match unit {
        "s" => Some(chrono::Duration::seconds(amount)),
        "m" => Some(chrono::Duration::minutes(amount)),
        "h" => Some(chrono::Duration::hours(amount)),
        "d" => Some(chrono::Duration::days(amount)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_policy_v0() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: dev-daily
    limits:
      request_cost:
        max_usd: 0.25
        action: warn
      context_tokens:
        max_tokens: 120000
        action: block
        warning_cadence: 30m
      spend:
        - id: monthly-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          warn_at_fraction: 0.5
          action: block
          warning_cadence: 1h
        - id: spike-5m
          window: 5m
          mode: rolling
          max_usd: 10
          action: warn
    match:
      project: noether
policies:
  - id: require-project
    action: block
    reason: project is required
    when:
      missing: project
"#,
        )
        .expect("policy parses");

        validate_policy(&policy).expect("policy is valid");
        assert_eq!(
            policy.budgets[0]
                .limits
                .context_tokens
                .as_ref()
                .unwrap()
                .warning_cadence
                .as_deref(),
            Some("30m")
        );
        assert_eq!(
            policy.budgets[0].limits.spend[0].warning_cadence.as_deref(),
            Some("1h")
        );
        assert_eq!(policy.budgets.len(), 1);
        assert_eq!(policy.policies.len(), 1);
    }

    #[test]
    fn accepts_legacy_specificity_but_serializes_fallback_order_and_clean_match() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
routing:
  mode: explicit_then_fallback
  specificity: [project, user, team, group, org, global]
budgets:
  - id: personal-local
    priority: 0
    match: {}
    limits:
      spend:
        - id: monthly-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 100
          action: block
"#,
        )
        .expect("legacy specificity policy parses");

        validate_policy(&policy).expect("policy is valid");
        let yaml = serde_yaml::to_string(&policy).expect("policy serializes");

        assert!(yaml.contains("fallback_order:"));
        assert!(!yaml.contains("specificity:"));
        assert!(!yaml.contains("match:"));
        assert!(!yaml.contains("null"));
        assert!(!yaml.contains("priority: 0"));
    }

    #[test]
    fn rejects_invalid_limit_policy() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: dev-daily
    limits:
      request_cost:
        max_usd: 0
        action: allow
      context_tokens:
        max_tokens: 0
        action: allow
"#,
        )
        .expect("policy parses");

        let error = validate_policy(&policy).expect_err("limit policy should be invalid");
        let message = error.to_string();
        assert!(message.contains("max_usd must be positive"));
        assert!(message.contains("action must be warn, ask, or block"));
        assert!(message.contains("max_tokens must be positive"));
    }

    #[test]
    fn rejects_invalid_spend_window_limit_policy() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: dev-daily
    limits:
      spend:
        - id: bad-window
          window: 5x
          mode: rolling
          max_usd: 0
          action: allow
"#,
        )
        .expect("policy parses");

        let error = validate_policy(&policy).expect_err("spend window limit should be invalid");
        let message = error.to_string();
        assert!(message.contains("limits.spend.window must use <number><s|m|h|d>"));
        assert!(message.contains("limits.spend.max_usd must be positive"));
        assert!(message.contains("limits.spend.action must be warn, ask, or block"));
    }

    #[test]
    fn rejects_invalid_explicit_window_mode_anchor_combinations() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: explicit-budget
    limits:
      spend:
        - id: rolling-with-anchor
          window: 5m
          mode: rolling
          anchor:
            kind: first_seen
          max_usd: 1
          action: warn
        - id: tumbling-without-anchor
          window: 1d
          mode: tumbling
          max_usd: 2
          action: block
        - id: missing-mode
          window: 30d
          max_usd: 5
          action: block
"#,
        )
        .expect("policy parses");

        let error = validate_policy(&policy).expect_err("mode/anchor mismatch should be invalid");
        let message = error.to_string();
        assert!(message.contains(
            "budget explicit-budget limits.spend[rolling-with-anchor].anchor requires mode tumbling"
        ));
        assert!(message.contains("budget explicit-budget limits.spend[tumbling-without-anchor].anchor is required when mode is tumbling"));
        assert!(
            message.contains("budget explicit-budget limits.spend[missing-mode].mode is required")
        );
    }

    #[test]
    fn rejects_duplicate_spend_window_ids_within_a_budget() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: duplicate-limits
    limits:
      spend:
        - id: daily-cap
          window: 1d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1
          action: warn
        - id: daily-cap
          window: 5m
          mode: rolling
          max_usd: 2
          action: block
"#,
        )
        .expect("policy parses");

        let error = validate_policy(&policy).expect_err("duplicate limit ids should be invalid");
        assert!(error.to_string().contains(
            "budget duplicate-limits limits.spend ids must be unique, found duplicate id daily-cap"
        ));
    }

    #[test]
    fn rejects_rolling_spend_windows_outside_spike_guard_range() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: long-rolling
    limits:
      spend:
        - id: too-short
          window: 5s
          mode: rolling
          max_usd: 1
          action: warn
        - id: weekly-burn
          window: 7d
          mode: rolling
          max_usd: 10
          action: warn
"#,
        )
        .expect("policy parses");

        let error = validate_policy(&policy).expect_err("long rolling window should be invalid");
        let message = error.to_string();
        assert!(message.contains(
            "budget long-rolling limits.spend[too-short].window must be between 10s and 1h when mode is rolling"
        ));
        assert!(message.contains(
            "budget long-rolling limits.spend[weekly-burn].window must be between 10s and 1h when mode is rolling"
        ));
    }

    #[test]
    fn no_policy_validation_warnings_for_explicit_windows_only_model() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: explicit-budget
    limits:
      spend:
        - id: daily-cap
          window: 1d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1
          action: warn
"#,
        )
        .expect("policy parses");

        validate_policy(&policy).expect("explicit policy validates");
        let warnings = policy_validation_warnings(&policy);
        assert!(warnings.is_empty());
    }

    #[test]
    fn rejects_invalid_lifecycle_limit_policy() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: dev-daily
    limits:
      spend:
        - id: monthly-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1
          action: block
      tool_calls: 0
      agent_steps: 0
      retries: 0
"#,
        )
        .expect("policy parses");

        let error = validate_policy(&policy).expect_err("lifecycle limit policy should be invalid");
        let message = error.to_string();
        assert!(message.contains("limits.tool_calls must be positive"));
        assert!(message.contains("limits.agent_steps must be positive"));
        assert!(message.contains("limits.retries must be positive"));
    }

    #[test]
    fn parses_and_validates_protected_adoption_pool_allocation() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: ai-adoption
    match:
      org: example
    limits:
      spend:
        - id: monthly-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 2000
          action: block
    allocation:
      standard: protected_adoption_pool
      by: user
      protected_amount_usd: 25
      window: monthly
      carryover:
        percent: 10
        cap_usd: 50
"#,
        )
        .expect("policy parses");

        validate_policy(&policy).expect("policy is valid");
        let allocation = policy.budgets[0]
            .allocation
            .as_ref()
            .expect("allocation should parse");
        assert_eq!(allocation.standard, "protected_adoption_pool");
        assert_eq!(allocation.by.as_deref(), Some("user"));
        assert_eq!(allocation.protected_amount_usd, Some(25.0));
        assert_eq!(allocation.window.as_deref(), Some("monthly"));
        let carryover = allocation
            .carryover
            .as_ref()
            .expect("carryover should parse");
        assert_eq!(carryover.percent, Some(10.0));
        assert_eq!(carryover.cap_usd, Some(50.0));
    }

    #[test]
    fn rejects_invalid_protected_adoption_pool_allocation() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: ai-adoption
    limits:
      spend:
        - id: monthly-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 2000
          action: block
    allocation:
      standard: protected_adoption_pool
      by: org
      protected_amount_usd: 0
      window: weekly
      carryover:
        percent: 120
        cap_usd: -1
"#,
        )
        .expect("policy parses");

        let error = validate_policy(&policy).expect_err("policy should be invalid");
        let message = error.to_string();
        assert!(message.contains("allocation.by must be user or team"));
        assert!(message.contains("allocation.protected_amount_usd must be positive"));
        assert!(message.contains("allocation.window must be monthly"));
        assert!(message.contains("allocation.carryover.percent must be between 0 and 100"));
        assert!(message.contains("allocation.carryover.cap_usd must be zero or positive"));
    }

    #[test]
    fn rejects_invalid_policy() {
        let policy = PolicyFile {
            version: 1,
            routing: Default::default(),
            budgets: Vec::new(),
            policies: Vec::new(),
        };

        assert!(validate_policy(&policy).is_err());
    }

    #[test]
    fn budget_rule_matches_project_user_team_org_and_global_entities() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: project-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      project: noether
  - id: user-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      user: alice
  - id: team-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      team: core
  - id: org-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      org: example
  - id: global-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
"#,
        )
        .expect("policy parses");
        let request =
            request_with_entities(["project:noether", "user:alice", "team:core", "org:example"]);

        validate_policy(&policy).expect("policy is valid");
        assert!(
            policy
                .budgets
                .iter()
                .all(|rule| budget_rule_matches(rule, &request))
        );
    }

    #[test]
    fn eligible_entities_can_match_legacy_project_and_subject_fields() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: project-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      project: noether
  - id: subject-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      user: alice
"#,
        )
        .expect("policy parses");
        let mut request = request_with_entities([]);
        request.project = Some("noether".to_owned());
        request.subject = Some("alice".to_owned());

        assert!(
            policy
                .budgets
                .iter()
                .all(|rule| budget_rule_matches(rule, &request))
        );
    }

    #[test]
    fn flat_v0_match_remains_budget_default_when_eligible_entities_are_absent() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: v0-flat-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      project: noether
"#,
        )
        .expect("policy parses");
        let matching_request = request_with_project("noether");
        let other_request = request_with_project("other");

        assert!(budget_rule_matches(&policy.budgets[0], &matching_request));
        assert!(!budget_rule_matches(&policy.budgets[0], &other_request));
    }

    #[test]
    fn model_allowlist_matches_provider_model_exactly_or_by_wildcard_suffix() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: premium-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      project: noether
    models:
      allow:
        - openai:gpt-4.1
        - anthropic:claude-sonnet-*
"#,
        )
        .expect("policy parses");
        let rule = &policy.budgets[0];

        assert!(budget_rule_matches(
            rule,
            &request_with_model("openai", "gpt-4.1")
        ));
        assert!(budget_rule_matches(
            rule,
            &request_with_model("anthropic", "claude-sonnet-4")
        ));
        assert!(!budget_rule_matches(
            rule,
            &request_with_model("openai", "gpt-4.1-mini")
        ));
    }

    #[test]
    fn omitted_model_allowlist_allows_all_models() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: unrestricted-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      project: noether
"#,
        )
        .expect("policy parses");

        assert!(budget_rule_matches(
            &policy.budgets[0],
            &request_with_model("openai", "any-model")
        ));
    }

    #[test]
    fn model_allowlist_requires_provider_and_model() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: restricted-budget
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.0
          action: block
    match:
      project: noether
    models:
      allow: [openai:gpt-4.1]
"#,
        )
        .expect("policy parses");
        let request = request_with_entities(["project:noether"]);

        assert!(!budget_rule_matches(&policy.budgets[0], &request));
    }

    fn request_with_project(project: &str) -> AuthorizeRequest {
        let mut request = request_with_entities([]);
        request.project = Some(project.to_owned());
        request
    }

    fn request_with_model(provider: &str, model: &str) -> AuthorizeRequest {
        let mut request = request_with_entities(["project:noether"]);
        request.provider = Some(provider.to_owned());
        request.model = Some(model.to_owned());
        request
    }

    fn request_with_entities<const N: usize>(entities: [&str; N]) -> AuthorizeRequest {
        AuthorizeRequest {
            budget_id: None,
            entities: entities.iter().map(|entity| (*entity).to_owned()).collect(),
            subject: None,
            project: None,
            provider: None,
            model: None,
            estimated_tokens: None,
            estimated_cost_usd: None,
            metadata: Default::default(),
        }
    }
}
