use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::contract::{
    AuthorizeRequest, BudgetRule, DecisionExplanation, DecisionSeverity, PolicyEffect, PolicyRule,
    RuleMatch,
};
use crate::error::NoetError;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PolicyFile {
    pub version: u16,
    #[serde(default)]
    pub budgets: Vec<BudgetRule>,
    #[serde(default, rename = "policies")]
    pub policies: Vec<PolicyRule>,
}

pub async fn load_policy(path: &Path) -> Result<PolicyFile, NoetError> {
    let bytes = fs::read(path).await?;
    let policy: PolicyFile = serde_yaml::from_slice(&bytes)?;
    validate_policy(&policy)?;
    Ok(policy)
}

pub fn validate_policy(policy: &PolicyFile) -> Result<(), NoetError> {
    let mut errors = Vec::new();
    if policy.version != 0 {
        errors.push(format!("version must be 0, got {}", policy.version));
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

pub fn matching_policy_explanations(
    policy: &PolicyFile,
    request: &AuthorizeRequest,
) -> Vec<(PolicyEffect, DecisionExplanation)> {
    policy
        .policies
        .iter()
        .filter(|rule| policy_rule_matches(rule, request))
        .map(|rule| {
            let severity = match rule.effect {
                PolicyEffect::Allow => DecisionSeverity::Info,
                PolicyEffect::Warn => DecisionSeverity::Warn,
                PolicyEffect::Deny => DecisionSeverity::Deny,
            };
            (
                rule.effect,
                DecisionExplanation {
                    rule_id: rule.id.clone(),
                    reason: rule.reason.clone(),
                    severity,
                },
            )
        })
        .collect()
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
    match:
      project: noether
policies:
  - id: require-project
    effect: deny
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
    fn rejects_invalid_policy() {
        let policy = PolicyFile {
            version: 1,
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
