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

pub fn rule_match_matches(rule_match: &RuleMatch, request: &AuthorizeRequest) -> bool {
    matches_optional(&rule_match.subject, &request.subject)
        && matches_optional(&rule_match.project, &request.project)
        && matches_optional(&rule_match.provider, &request.provider)
        && matches_optional(&rule_match.model, &request.model)
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
}
