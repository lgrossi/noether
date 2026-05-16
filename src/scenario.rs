use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contract::{AuthorizeRequest, DecisionOutcome, UsageObservation};
use crate::error::NoetError;
use crate::policy::{PolicyFile, validate_policy};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScenarioFile {
    pub version: u16,
    #[serde(default)]
    pub name: Option<String>,
    pub policy: PolicyFile,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub requests: Vec<ScenarioRequest>,
    #[serde(default)]
    pub assertions: Vec<ScenarioAssertion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScenarioRequest {
    pub id: String,
    pub authorize: AuthorizeRequest,
    #[serde(default)]
    pub model_choice: Option<ScenarioModelChoice>,
    #[serde(default)]
    pub tool_activity: Vec<ScenarioToolActivity>,
    #[serde(default)]
    pub finalize: Option<ScenarioFinalizeStep>,
    #[serde(default)]
    pub denial: Option<ScenarioDenialExpectation>,
    #[serde(default)]
    pub fallback: Option<ScenarioFallbackExpectation>,
    #[serde(default)]
    pub assertions: Vec<ScenarioAssertion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScenarioModelChoice {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScenarioToolActivity {
    pub name: String,
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScenarioFinalizeStep {
    #[serde(default)]
    pub actual_cost_usd: Option<f64>,
    #[serde(default)]
    pub usage: Option<UsageObservation>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScenarioDenialExpectation {
    #[serde(default)]
    pub rule_id: Option<String>,
    #[serde(default)]
    pub reason_contains: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScenarioFallbackExpectation {
    #[serde(default)]
    pub requested_budget_id: Option<String>,
    #[serde(default)]
    pub selected_budget_id: Option<String>,
    #[serde(default)]
    pub matched_entity: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioReportSource {
    Usage,
    Decisions,
    Trace,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioAssertion {
    DecisionOutcome {
        request_id: String,
        outcome: DecisionOutcome,
    },
    SelectedBudget {
        request_id: String,
        budget_id: String,
    },
    Denied {
        request_id: String,
    },
    TotalCostUsd {
        amount_usd: f64,
    },
    GuardHit {
        request_id: String,
        rule_id: String,
    },
    Fallback {
        request_id: String,
        #[serde(default)]
        requested_budget_id: Option<String>,
        #[serde(default)]
        selected_budget_id: Option<String>,
        #[serde(default)]
        matched_entity: Option<String>,
    },
    ReportJson {
        report: ScenarioReportSource,
        #[serde(default)]
        request_id: Option<String>,
        pointer: String,
        equals: Value,
    },
    ReportContains {
        text: String,
    },
    DashboardContains {
        text: String,
    },
}

pub fn validate_scenario(file: &ScenarioFile) -> Result<(), NoetError> {
    let mut errors = Vec::new();
    let mut request_ids = BTreeSet::new();
    if file.version != 1 {
        errors.push(format!("scenario version must be 1, got {}", file.version));
    }
    if let Err(error) = validate_policy(&file.policy) {
        errors.push(format!("scenario policy invalid: {error}"));
    }
    for request in &file.requests {
        if request.id.trim().is_empty() {
            errors.push("scenario request id must not be empty".to_owned());
        } else if !request_ids.insert(request.id.clone()) {
            errors.push(format!("duplicate scenario request id {}", request.id));
        }
        if request.denial.is_some() && request.finalize.is_some() {
            errors.push(format!(
                "scenario request {} cannot declare both denial and finalize",
                request.id
            ));
        }
        if let Some(tool) = request
            .tool_activity
            .iter()
            .find(|tool| tool.name.trim().is_empty())
        {
            errors.push(format!(
                "scenario request {} has tool activity with empty name",
                request.id
            ));
            let _ = tool;
        }
    }
    for request in &file.requests {
        validate_assertions(
            &request.assertions,
            Some(request.id.as_str()),
            &request_ids,
            &mut errors,
        );
    }
    validate_assertions(&file.assertions, None, &request_ids, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(NoetError::InvalidConfig(errors.join("; ")))
    }
}

fn validate_assertions(
    assertions: &[ScenarioAssertion],
    enclosing_request_id: Option<&str>,
    request_ids: &BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    for assertion in assertions {
        if let Some(request_id) = assertion_request_id(assertion)
            && !request_ids.contains(request_id)
        {
            errors.push(format!(
                "scenario assertion references unknown request id {request_id}"
            ));
        }
        if let ScenarioAssertion::ReportJson {
            report,
            request_id,
            pointer,
            ..
        } = assertion
        {
            if pointer.is_empty() || !pointer.starts_with('/') {
                errors.push(format!(
                    "scenario report_json pointer must start with /, got {pointer}"
                ));
            }
            match report {
                ScenarioReportSource::Trace => {
                    if request_id.is_none() && enclosing_request_id.is_none() {
                        errors.push(
                            "scenario trace report_json assertion requires request_id".to_owned(),
                        );
                    }
                }
                ScenarioReportSource::Usage | ScenarioReportSource::Decisions => {
                    if request_id.is_some() {
                        errors.push(
                            "scenario usage/decisions report_json assertions must not set request_id"
                                .to_owned(),
                        );
                    }
                }
            }
        }
    }
}

fn assertion_request_id(assertion: &ScenarioAssertion) -> Option<&str> {
    match assertion {
        ScenarioAssertion::DecisionOutcome { request_id, .. }
        | ScenarioAssertion::SelectedBudget { request_id, .. }
        | ScenarioAssertion::Denied { request_id }
        | ScenarioAssertion::GuardHit { request_id, .. }
        | ScenarioAssertion::Fallback { request_id, .. } => Some(request_id.as_str()),
        ScenarioAssertion::ReportJson {
            report: ScenarioReportSource::Trace,
            request_id,
            ..
        } => request_id.as_deref(),
        ScenarioAssertion::TotalCostUsd { .. }
        | ScenarioAssertion::ReportJson { .. }
        | ScenarioAssertion::ReportContains { .. }
        | ScenarioAssertion::DashboardContains { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_scenario_schema_v1() {
        let scenario: ScenarioFile = serde_yaml::from_str(
            r#"
version: 1
name: local developer
policy:
  version: 0
  budgets:
    - id: project-noether
      limit_usd: 10
      eligible:
        entities: [project:noether]
requests:
  - id: req-1
    authorize:
      project: noether
      provider: openai
      model: gpt-4.1
      estimated_tokens: 1200
      entities: [project:noether, user:alice]
    model_choice:
      provider: openai
      model: gpt-4.1
    tool_activity:
      - name: bash
        success: true
        duration_ms: 42
    finalize:
      actual_cost_usd: 0.002
      usage:
        provider: openai
        model: gpt-4.1
        total_tokens: 1000
    assertions:
      - kind: decision_outcome
        request_id: req-1
        outcome: allow
      - kind: total_cost_usd
        amount_usd: 0.002
      - kind: report_json
        report: usage
        pointer: /rows/0/model
        equals: gpt-4.1
      - kind: report_contains
        text: selected_budget=project-noether
"#,
        )
        .expect("scenario parses");

        validate_scenario(&scenario).expect("scenario is valid");
        assert_eq!(scenario.requests.len(), 1);
        assert_eq!(scenario.requests[0].tool_activity[0].name, "bash");
    }

    #[test]
    fn rejects_invalid_scenario_schema_v1() {
        let scenario: ScenarioFile = serde_yaml::from_str(
            r#"
version: 2
policy:
  version: 0
  budgets: []
requests:
  - id: req-1
    authorize:
      project: noether
    denial:
      rule_id: deny-1
    finalize:
      actual_cost_usd: 0.1
  - id: req-1
    authorize:
      project: noether
    tool_activity:
      - name: ""
assertions:
  - kind: selected_budget
    request_id: missing
    budget_id: project-noether
  - kind: report_json
    report: trace
    pointer: /items/0/kind
    equals: decision.allow
"#,
        )
        .expect("scenario parses");

        let error = validate_scenario(&scenario).expect_err("scenario should be invalid");
        let message = error.to_string();
        assert!(message.contains("scenario version must be 1"));
        assert!(message.contains("duplicate scenario request id req-1"));
        assert!(message.contains("cannot declare both denial and finalize"));
        assert!(message.contains("tool activity with empty name"));
        assert!(message.contains("references unknown request id missing"));
        assert!(message.contains("trace report_json assertion requires request_id"));
    }
}
