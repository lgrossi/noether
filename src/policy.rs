use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::contract::{
    AuthorizeRequest, BudgetRule, BudgetWindowMode, DecisionExplanation, DecisionSeverity,
    PolicyAction, PolicyRule, RuleMatch, SpendWindowLimit, SpendWindowMode,
};
use crate::error::NoetError;

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
    #[serde(default = "default_specificity")]
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
        if budget.limit_usd <= 0.0 {
            errors.push(format!("budget {} limit_usd must be positive", budget.id));
        }
        if !(0.0..=1.0).contains(&budget.warn_at_fraction) {
            errors.push(format!(
                "budget {} warn_at_fraction must be between 0 and 1",
                budget.id
            ));
        }
        if budget.window_seconds <= 0 {
            errors.push(format!(
                "budget {} window_seconds must be positive",
                budget.id
            ));
        }
        match (budget.window_mode, budget.window_anchor.as_ref()) {
            (Some(BudgetWindowMode::Tumbling), None) => errors.push(format!(
                "budget {} window_anchor is required when window_mode is tumbling",
                budget.id
            )),
            (None, Some(_)) => errors.push(format!(
                "budget {} window_anchor requires window_mode tumbling",
                budget.id
            )),
            _ => {}
        }
        for entity in &budget.eligible.entities {
            if entity.trim().is_empty() {
                errors.push(format!(
                    "budget {} eligible.entities must not contain empty values",
                    budget.id
                ));
            }
        }
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
        }
        let mut spend_window_ids = BTreeSet::new();
        for limit in &budget.limits.spend {
            if parse_limit_window(&limit.window).is_none() {
                errors.push(format!(
                    "budget {} limits.spend.window must use <number><s|m|h|d>, got {}",
                    budget.id, limit.window
                ));
            }
            if !limit.max_usd.is_finite() || limit.max_usd <= 0.0 {
                errors.push(format!(
                    "budget {} limits.spend.max_usd must be positive",
                    budget.id
                ));
            }
            if !limit_action_is_supported(limit.action) {
                errors.push(format!(
                    "budget {} limits.spend.action must be warn, ask, or block",
                    budget.id
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
                (Some(SpendWindowMode::Tumbling), None) => errors.push(format!(
                    "budget {} limits.spend[{}].anchor is required when mode is tumbling",
                    budget.id,
                    spend_window_label(limit)
                )),
                (Some(SpendWindowMode::Rolling) | None, Some(_)) => errors.push(format!(
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
    let mut warnings = Vec::new();

    for budget in &policy.budgets {
        if budget.window_mode.is_none() && budget.window_anchor.is_none() {
            warnings.push(format!(
                "budget {} uses implicit legacy window semantics; set window_mode/window_anchor explicitly",
                budget.id
            ));
        }

        for limit in &budget.limits.spend {
            if limit.mode.is_none() && limit.anchor.is_none() {
                warnings.push(format!(
                    "budget {} limits.spend[{}] uses implicit legacy rolling semantics; set id/mode/anchor explicitly",
                    budget.id,
                    spend_window_label(limit)
                ));
            }
        }
    }

    warnings
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
    if !rule.eligible.entities.is_empty() {
        return rule
            .eligible
            .entities
            .iter()
            .any(|eligible_entity| request_matches_entity(request, eligible_entity));
    }

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
    matches_optional(&rule_match.subject, &request.subject)
        && matches_optional(&rule_match.project, &request.project)
        && matches_optional(&rule_match.provider, &request.provider)
        && matches_optional(&rule_match.model, &request.model)
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
    limit_usd: 1.0
    warn_at_fraction: 0.5
    window_seconds: 60
    limits:
      request_cost:
        max_usd: 0.25
        action: warn
      context_tokens:
        max_tokens: 120000
        action: block
      spend:
        - window: 5h
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
        assert_eq!(policy.budgets.len(), 1);
        assert_eq!(policy.policies.len(), 1);
    }

    #[test]
    fn rejects_invalid_limit_policy() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: dev-daily
    limit_usd: 1.0
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
    limit_usd: 1.0
    limits:
      spend:
        - window: 5x
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
    limit_usd: 1.0
    window_mode: tumbling
    limits:
      spend:
        - id: rolling-with-anchor
          window: 5h
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
"#,
        )
        .expect("policy parses");

        let error = validate_policy(&policy).expect_err("mode/anchor mismatch should be invalid");
        let message = error.to_string();
        assert!(message.contains(
            "budget explicit-budget window_anchor is required when window_mode is tumbling"
        ));
        assert!(message.contains(
            "budget explicit-budget limits.spend[rolling-with-anchor].anchor requires mode tumbling"
        ));
        assert!(message.contains("budget explicit-budget limits.spend[tumbling-without-anchor].anchor is required when mode is tumbling"));
    }

    #[test]
    fn rejects_duplicate_spend_window_ids_within_a_budget() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: duplicate-limits
    limit_usd: 1.0
    limits:
      spend:
        - id: daily-cap
          window: 1d
          max_usd: 1
          action: warn
        - id: daily-cap
          window: 5h
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
    fn reports_legacy_window_warnings_for_implicit_budget_and_limit_semantics() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: legacy-budget
    limit_usd: 1.0
    limits:
      spend:
        - window: 1d
          max_usd: 1
          action: warn
"#,
        )
        .expect("policy parses");

        validate_policy(&policy).expect("legacy policy still validates");
        let warnings = policy_validation_warnings(&policy);
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|warning| {
            warning.contains("budget legacy-budget uses implicit legacy window semantics")
        }));
        assert!(warnings.iter().any(|warning| {
            warning.contains(
                "budget legacy-budget limits.spend[1d] uses implicit legacy rolling semantics",
            )
        }));
    }

    #[test]
    fn rejects_invalid_lifecycle_limit_policy() {
        let policy: PolicyFile = serde_yaml::from_str(
            r#"
version: 0
budgets:
  - id: dev-daily
    limit_usd: 1.0
    limits:
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
    limit_usd: 2000
    eligible:
      entities: [org:example]
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
    limit_usd: 2000
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
    limit_usd: 1.0
    eligible:
      entities: [project:noether]
  - id: user-budget
    limit_usd: 1.0
    eligible:
      entities: [user:alice]
  - id: team-budget
    limit_usd: 1.0
    eligible:
      entities: [team:core]
  - id: org-budget
    limit_usd: 1.0
    eligible:
      entities: [org:example]
  - id: global-budget
    limit_usd: 1.0
    eligible:
      entities: [global]
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
    limit_usd: 1.0
    eligible:
      entities: [project:noether]
  - id: subject-budget
    limit_usd: 1.0
    eligible:
      entities: [user:alice]
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
    limit_usd: 1.0
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
    limit_usd: 1.0
    eligible:
      entities: [project:noether]
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
    limit_usd: 1.0
    eligible:
      entities: [project:noether]
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
    limit_usd: 1.0
    eligible:
      entities: [project:noether]
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
