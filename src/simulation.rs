use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::contract::{AuthorizeRequest, DecisionOutcome, FinalizeReservation, UsageObservation};
use crate::error::NoetError;
use crate::ledger::BudgetLedger;
use crate::policy::{PolicyFile, validate_policy};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationFile {
    pub version: u16,
    #[serde(default)]
    pub name: Option<String>,
    pub seed: u64,
    pub horizon_days: u32,
    pub company: SyntheticCompany,
    #[serde(default)]
    pub models: Vec<SimulationModel>,
    #[serde(default)]
    pub strategies: Vec<SimulationStrategy>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyntheticCompany {
    pub id: String,
    #[serde(default)]
    pub teams: Vec<SyntheticTeam>,
    #[serde(default)]
    pub projects: Vec<SyntheticProject>,
    #[serde(default)]
    pub users: Vec<SyntheticUser>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyntheticTeam {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyntheticProject {
    pub id: String,
    pub team_id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyntheticUser {
    pub id: String,
    pub team_id: String,
    #[serde(default)]
    pub project_ids: Vec<String>,
    pub profile: UsageProfile,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageProfile {
    PowerUser,
    SteadyUser,
    LowAdopter,
    Experimenter,
    LoopRisk,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationModel {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub cost_per_1k_tokens_usd: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationStrategy {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    pub policy: PolicyFile,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SyntheticDemandRequest {
    pub request_id: String,
    pub day_index: u32,
    pub subject: String,
    pub team_id: String,
    pub project_id: String,
    pub profile: UsageProfile,
    pub model_id: String,
    pub provider: String,
    pub model: String,
    pub estimated_tokens: u64,
    pub estimated_cost_usd: f64,
    pub tool_call_count: u32,
    pub useful_work_score: u32,
    pub loop_risk: bool,
    pub entities: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SimulationComparisonReport {
    pub name: Option<String>,
    pub seed: u64,
    pub horizon_days: u32,
    pub total_requests: u64,
    pub strategies: Vec<SimulationStrategyReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SimulationStrategyReport {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub total_requests: u64,
    pub allowed_requests: u64,
    pub warned_requests: u64,
    pub denied_requests: u64,
    pub fallback_count: u64,
    pub guard_hit_count: u64,
    pub total_cost_usd: f64,
    pub unused_budget_usd: f64,
    pub useful_work_blocked_score: u64,
    pub runaway_spend_prevented_usd: f64,
    pub adoption_coverage: f64,
    pub fairness_score: f64,
    pub model_mix: Vec<SimulationModelMixEntry>,
    pub carryover_liability_usd: f64,
    pub exhaustion_day: Option<u32>,
    pub db_path: PathBuf,
    pub usage_report_path: PathBuf,
    pub decisions_report_path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct SimulationModelMixEntry {
    pub model_id: String,
    pub requests: u64,
    pub total_cost_usd: f64,
}

pub fn validate_simulation(file: &SimulationFile) -> Result<(), NoetError> {
    let mut errors = Vec::new();
    if file.version != 1 {
        errors.push(format!("simulation version must be 1, got {}", file.version));
    }
    if file.company.id.trim().is_empty() {
        errors.push("simulation company id must not be empty".to_owned());
    }
    if file.horizon_days == 0 {
        errors.push("simulation horizon_days must be greater than zero".to_owned());
    }
    if file.company.teams.is_empty() {
        errors.push("simulation must define at least one team".to_owned());
    }
    if file.company.projects.is_empty() {
        errors.push("simulation must define at least one project".to_owned());
    }
    if file.company.users.is_empty() {
        errors.push("simulation must define at least one user".to_owned());
    }
    if file.models.is_empty() {
        errors.push("simulation must define at least one model".to_owned());
    }
    if file.strategies.is_empty() {
        errors.push("simulation must define at least one strategy".to_owned());
    }

    let team_ids = collect_ids(
        file.company.teams.iter().map(|team| team.id.as_str()),
        "team",
        &mut errors,
    );
    let project_ids = collect_ids(
        file.company.projects.iter().map(|project| project.id.as_str()),
        "project",
        &mut errors,
    );
    let _user_ids = collect_ids(
        file.company.users.iter().map(|user| user.id.as_str()),
        "user",
        &mut errors,
    );
    let _model_ids = collect_ids(
        file.models.iter().map(|model| model.id.as_str()),
        "model",
        &mut errors,
    );
    let _strategy_ids = collect_ids(
        file.strategies.iter().map(|strategy| strategy.id.as_str()),
        "strategy",
        &mut errors,
    );

    for project in &file.company.projects {
        if !team_ids.contains(project.team_id.as_str()) {
            errors.push(format!(
                "simulation project {} references unknown team {}",
                project.id, project.team_id
            ));
        }
    }

    for user in &file.company.users {
        if !team_ids.contains(user.team_id.as_str()) {
            errors.push(format!(
                "simulation user {} references unknown team {}",
                user.id, user.team_id
            ));
        }
        if user.project_ids.is_empty() {
            errors.push(format!(
                "simulation user {} must reference at least one project",
                user.id
            ));
        }
        for project_id in &user.project_ids {
            if !project_ids.contains(project_id.as_str()) {
                errors.push(format!(
                    "simulation user {} references unknown project {}",
                    user.id, project_id
                ));
            }
        }
    }

    for model in &file.models {
        if model.provider.trim().is_empty() {
            errors.push(format!(
                "simulation model {} must not have an empty provider",
                model.id
            ));
        }
        if model.model.trim().is_empty() {
            errors.push(format!(
                "simulation model {} must not have an empty model name",
                model.id
            ));
        }
        if model.cost_per_1k_tokens_usd <= 0.0 {
            errors.push(format!(
                "simulation model {} cost_per_1k_tokens_usd must be positive",
                model.id
            ));
        }
    }

    for strategy in &file.strategies {
        if let Err(error) = validate_policy(&strategy.policy) {
            errors.push(format!(
                "simulation strategy {} policy invalid: {error}",
                strategy.id
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(NoetError::InvalidConfig(errors.join("; ")))
    }
}

pub fn generate_synthetic_demand(
    file: &SimulationFile,
) -> Result<Vec<SyntheticDemandRequest>, NoetError> {
    validate_simulation(file)?;

    let mut rng = DeterministicRng::new(file.seed);
    let mut requests = Vec::new();
    for day_index in 0..file.horizon_days {
        for user in &file.company.users {
            let request_count = profile_request_count(user.profile, &mut rng, day_index);
            for request_index in 0..request_count {
                let project_id = user.project_ids
                    [rng.next_bounded(user.project_ids.len() as u64) as usize]
                    .clone();
                let model = choose_model(file, user.profile, &mut rng);
                let estimated_tokens =
                    profile_tokens(user.profile, &mut rng, day_index, request_index);
                let tool_call_count = profile_tool_calls(user.profile, &mut rng, request_index);
                let useful_work_score =
                    profile_useful_work_score(user.profile, &mut rng, tool_call_count);
                let estimated_cost_usd =
                    (estimated_tokens as f64 / 1000.0) * model.cost_per_1k_tokens_usd;
                requests.push(SyntheticDemandRequest {
                    request_id: format!("day-{day_index:02}-{}-{}", user.id, request_index + 1),
                    day_index,
                    subject: format!("user:{}", user.id),
                    team_id: user.team_id.clone(),
                    project_id: project_id.clone(),
                    profile: user.profile,
                    model_id: model.id.clone(),
                    provider: model.provider.clone(),
                    model: model.model.clone(),
                    estimated_tokens,
                    estimated_cost_usd,
                    tool_call_count,
                    useful_work_score,
                    loop_risk: matches!(user.profile, UsageProfile::LoopRisk)
                        && (tool_call_count >= 6 || estimated_tokens >= 40_000),
                    entities: vec![
                        format!("team:{}", user.team_id),
                        format!("project:{project_id}"),
                        format!("user:{}", user.id),
                    ],
                });
            }
        }
    }
    Ok(requests)
}

pub fn compare_strategies(
    file: &SimulationFile,
    out_dir: &Path,
) -> Result<SimulationComparisonReport, NoetError> {
    validate_simulation(file)?;
    std::fs::create_dir_all(out_dir)?;

    let demand = generate_synthetic_demand(file)?;
    let strategies_dir = out_dir.join("strategies");
    std::fs::create_dir_all(&strategies_dir)?;
    let mut strategies = Vec::new();

    for strategy in &file.strategies {
        let strategy_slug = sanitize_simulation_path_component(&strategy.id);
        let strategy_dir = strategies_dir.join(&strategy_slug);
        std::fs::create_dir_all(&strategy_dir)?;
        let db_path = strategy_dir.join("simulation.sqlite");
        if db_path.exists() {
            std::fs::remove_file(&db_path)?;
        }

        let mut ledger = BudgetLedger::open_sqlite(&db_path)?;
        let mut report = SimulationStrategyReport {
            id: strategy.id.clone(),
            description: strategy.description.clone(),
            total_requests: demand.len() as u64,
            allowed_requests: 0,
            warned_requests: 0,
            denied_requests: 0,
            fallback_count: 0,
            guard_hit_count: 0,
            total_cost_usd: 0.0,
            unused_budget_usd: 0.0,
            useful_work_blocked_score: 0,
            runaway_spend_prevented_usd: 0.0,
            adoption_coverage: 0.0,
            fairness_score: 0.0,
            model_mix: Vec::new(),
            carryover_liability_usd: 0.0,
            exhaustion_day: None,
            db_path: db_path.clone(),
            usage_report_path: strategy_dir.join("usage-report.json"),
            decisions_report_path: strategy_dir.join("decisions-report.json"),
        };
        let mut users_with_access = BTreeSet::new();
        let mut user_spend = BTreeMap::new();
        let mut model_mix = BTreeMap::new();

        for request in &demand {
            let authorize = synthetic_authorize_request(request, &strategy.id);
            let decision = ledger.try_authorize(Some(&strategy.policy), &authorize)?;
            let guard_hit_count = decision
                .explanations
                .iter()
                .filter(|explanation| is_guard_rule_id(&explanation.rule_id))
                .count() as u64;
            if decision
                .explanations
                .iter()
                .any(|explanation| explanation.reason.starts_with("selected fallback budget"))
            {
                report.fallback_count += 1;
            }
            report.guard_hit_count += guard_hit_count;
            match decision.outcome {
                DecisionOutcome::Allow => {
                    report.allowed_requests += 1;
                    users_with_access.insert(request.subject.clone());
                }
                DecisionOutcome::Warn => {
                    report.warned_requests += 1;
                    users_with_access.insert(request.subject.clone());
                }
                DecisionOutcome::Deny => {
                    report.denied_requests += 1;
                    report.useful_work_blocked_score += request.useful_work_score as u64;
                    if request.loop_risk || guard_hit_count > 0 {
                        report.runaway_spend_prevented_usd += request.estimated_cost_usd;
                    }
                    if report.exhaustion_day.is_none()
                        && decision.explanations.iter().any(|explanation| {
                            explanation.reason.contains("fixed-window limit")
                                || explanation.rule_id == "no_fallback_budget"
                        })
                    {
                        report.exhaustion_day = Some(request.day_index);
                    }
                    continue;
                }
            }

            if let Some(reservation) = &decision.reservation {
                let finalize = FinalizeReservation {
                    reservation_id: None,
                    usage: Some(UsageObservation {
                        provider: Some(request.provider.clone()),
                        model: Some(request.model.clone()),
                        input_tokens: Some(request.estimated_tokens * 3 / 5),
                        output_tokens: Some(request.estimated_tokens * 2 / 5),
                        total_tokens: Some(request.estimated_tokens),
                        cost_usd: Some(request.estimated_cost_usd),
                        latency_ms: Some(500 + request.tool_call_count as u64 * 50),
                        stop_reason: Some("stop".to_owned()),
                    }),
                    actual_cost_usd: Some(request.estimated_cost_usd),
                    metadata: BTreeMap::from([
                        (
                            "trace_id".to_owned(),
                            serde_json::Value::String(format!(
                                "{}:{}",
                                strategy.id, request.request_id
                            )),
                        ),
                        (
                            "request_id".to_owned(),
                            serde_json::Value::String(request.request_id.clone()),
                        ),
                        (
                            "source".to_owned(),
                            serde_json::Value::String("simulation".to_owned()),
                        ),
                    ]),
                };
                let _ = ledger.finalize(&reservation.id, &finalize)?;
                *user_spend.entry(request.subject.clone()).or_insert(0.0) += request.estimated_cost_usd;
                let entry = model_mix
                    .entry(request.model_id.clone())
                    .or_insert((0_u64, 0.0_f64));
                entry.0 += 1;
                entry.1 += request.estimated_cost_usd;
            }
        }

        let usage = ledger.usage_report()?;
        let decisions = ledger.decisions_report()?;
        report.total_cost_usd = usage.total_cost_usd;
        report.unused_budget_usd = (strategy
            .policy
            .budgets
            .iter()
            .map(|budget| budget.limit_usd)
            .sum::<f64>()
            - usage.total_cost_usd)
            .max(0.0);
        report.adoption_coverage = if file.company.users.is_empty() {
            0.0
        } else {
            users_with_access.len() as f64 / file.company.users.len() as f64
        };
        report.fairness_score = fairness_score(&file.company.users, &user_spend);
        report.model_mix = model_mix
            .into_iter()
            .map(|(model_id, (requests, total_cost_usd))| SimulationModelMixEntry {
                model_id,
                requests,
                total_cost_usd,
            })
            .collect();
        report
            .model_mix
            .sort_by(|left, right| right
                .total_cost_usd
                .partial_cmp(&left.total_cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.model_id.cmp(&right.model_id)));
        report.carryover_liability_usd = usage
            .protected_adoption
            .as_ref()
            .map(|adoption| adoption.carryover_liability_usd)
            .unwrap_or_default();
        std::fs::write(
            &report.usage_report_path,
            serde_json::to_vec_pretty(&usage)?,
        )?;
        std::fs::write(
            &report.decisions_report_path,
            serde_json::to_vec_pretty(&decisions)?,
        )?;
        strategies.push(report);
    }

    Ok(SimulationComparisonReport {
        name: file.name.clone(),
        seed: file.seed,
        horizon_days: file.horizon_days,
        total_requests: demand.len() as u64,
        strategies,
    })
}

fn collect_ids<'a>(
    ids: impl Iterator<Item = &'a str>,
    kind: &str,
    errors: &mut Vec<String>,
) -> BTreeSet<&'a str> {
    let mut unique = BTreeSet::new();
    for id in ids {
        if id.trim().is_empty() {
            errors.push(format!("simulation {kind} id must not be empty"));
        } else if !unique.insert(id) {
            errors.push(format!("duplicate simulation {kind} id {id}"));
        }
    }
    unique
}

fn choose_model<'a>(
    file: &'a SimulationFile,
    profile: UsageProfile,
    rng: &mut DeterministicRng,
) -> &'a SimulationModel {
    let default = &file.models[0];
    let alt = file.models.get(1).unwrap_or(default);
    match profile {
        UsageProfile::PowerUser => {
            if rng.next_bounded(10) < 7 {
                default
            } else {
                alt
            }
        }
        UsageProfile::SteadyUser => {
            if rng.next_bounded(10) < 6 {
                alt
            } else {
                default
            }
        }
        UsageProfile::LowAdopter => alt,
        UsageProfile::Experimenter => {
            if rng.next_bounded(2) == 0 {
                default
            } else {
                alt
            }
        }
        UsageProfile::LoopRisk => default,
    }
}

fn profile_request_count(
    profile: UsageProfile,
    rng: &mut DeterministicRng,
    day_index: u32,
) -> u32 {
    match profile {
        UsageProfile::PowerUser => 3 + rng.next_bounded(3) as u32,
        UsageProfile::SteadyUser => 1 + rng.next_bounded(2) as u32,
        UsageProfile::LowAdopter => {
            if (day_index + rng.next_bounded(10) as u32).is_multiple_of(4) {
                1
            } else {
                0
            }
        }
        UsageProfile::Experimenter => 1 + rng.next_bounded(3) as u32,
        UsageProfile::LoopRisk => 2 + rng.next_bounded(3) as u32,
    }
}

fn profile_tokens(
    profile: UsageProfile,
    rng: &mut DeterministicRng,
    day_index: u32,
    request_index: u32,
) -> u64 {
    match profile {
        UsageProfile::PowerUser => 7_500 + rng.next_bounded(4_500),
        UsageProfile::SteadyUser => 2_500 + rng.next_bounded(2_500),
        UsageProfile::LowAdopter => 800 + rng.next_bounded(1_200),
        UsageProfile::Experimenter => 1_500 + rng.next_bounded(6_500),
        UsageProfile::LoopRisk => {
            if (day_index + request_index).is_multiple_of(3) {
                40_000 + rng.next_bounded(20_000)
            } else {
                5_000 + rng.next_bounded(7_000)
            }
        }
    }
}

fn profile_tool_calls(
    profile: UsageProfile,
    rng: &mut DeterministicRng,
    request_index: u32,
) -> u32 {
    match profile {
        UsageProfile::PowerUser => 2 + rng.next_bounded(3) as u32,
        UsageProfile::SteadyUser => 1 + rng.next_bounded(2) as u32,
        UsageProfile::LowAdopter => rng.next_bounded(2) as u32,
        UsageProfile::Experimenter => 1 + rng.next_bounded(4) as u32,
        UsageProfile::LoopRisk => {
            if request_index.is_multiple_of(2) {
                6 + rng.next_bounded(5) as u32
            } else {
                3 + rng.next_bounded(4) as u32
            }
        }
    }
}

fn profile_useful_work_score(
    profile: UsageProfile,
    rng: &mut DeterministicRng,
    tool_call_count: u32,
) -> u32 {
    let base = match profile {
        UsageProfile::PowerUser => 85,
        UsageProfile::SteadyUser => 70,
        UsageProfile::LowAdopter => 45,
        UsageProfile::Experimenter => 60,
        UsageProfile::LoopRisk => 35,
    };
    base + rng.next_bounded(15) as u32 + tool_call_count.min(5)
}

#[derive(Clone, Debug)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn next_bounded(&mut self, upper: u64) -> u64 {
        if upper <= 1 {
            0
        } else {
            self.next_u64() % upper
        }
    }
}

fn synthetic_authorize_request(
    request: &SyntheticDemandRequest,
    strategy_id: &str,
) -> AuthorizeRequest {
    AuthorizeRequest {
        budget_id: None,
        entities: request.entities.clone(),
        subject: Some(request.subject.clone()),
        project: Some(request.project_id.clone()),
        provider: Some(request.provider.clone()),
        model: Some(request.model.clone()),
        estimated_tokens: Some(request.estimated_tokens),
        estimated_cost_usd: Some(request.estimated_cost_usd),
        metadata: BTreeMap::from([
            (
                "trace_id".to_owned(),
                serde_json::Value::String(format!("{}:{}", strategy_id, request.request_id)),
            ),
            (
                "request_id".to_owned(),
                serde_json::Value::String(request.request_id.clone()),
            ),
            (
                "session_id".to_owned(),
                serde_json::Value::String(format!("simulation:{strategy_id}")),
            ),
        ]),
    }
}

fn is_guard_rule_id(rule_id: &str) -> bool {
    rule_id.contains(".max_estimated_request_cost_usd")
        || rule_id.contains(".max_context_tokens")
        || rule_id.contains(".spend_window.")
}

fn sanitize_simulation_path_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "simulation".to_owned()
    } else {
        sanitized.to_owned()
    }
}

fn fairness_score(users: &[SyntheticUser], user_spend: &BTreeMap<String, f64>) -> f64 {
    if users.is_empty() {
        return 0.0;
    }
    let values: Vec<f64> = users
        .iter()
        .map(|user| user_spend.get(&format!("user:{}", user.id)).copied().unwrap_or(0.0))
        .collect();
    let total: f64 = values.iter().sum();
    if total == 0.0 {
        return 1.0;
    }
    let mean = total / values.len() as f64;
    let mean_abs_deviation =
        values.iter().map(|value| (value - mean).abs()).sum::<f64>() / values.len() as f64;
    (1.0 - (mean_abs_deviation / mean)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_simulation_schema_v1() {
        let simulation: SimulationFile = serde_yaml::from_str(
            r#"
version: 1
name: benchmark company
seed: 42
horizon_days: 30
company:
  id: example-co
  teams:
    - id: platform
    - id: product
  projects:
    - id: editor
      team_id: product
    - id: noether
      team_id: platform
  users:
    - id: alice
      team_id: platform
      project_ids: [noether]
      profile: power_user
    - id: bob
      team_id: product
      project_ids: [editor]
      profile: experimenter
models:
  - id: flagship
    provider: openai
    model: gpt-4.1
    cost_per_1k_tokens_usd: 0.01
strategies:
  - id: pooled
    policy:
      version: 0
      budgets:
        - id: team-platform
          limit_usd: 250
          eligible:
            entities: [team:platform]
"#,
        )
        .expect("simulation parses");

        validate_simulation(&simulation).expect("simulation is valid");
        assert_eq!(simulation.company.users.len(), 2);
        assert_eq!(simulation.company.users[0].profile, UsageProfile::PowerUser);
    }

    #[test]
    fn rejects_invalid_simulation_schema_v1() {
        let simulation: SimulationFile = serde_yaml::from_str(
            r#"
version: 2
seed: 9
horizon_days: 0
company:
  id: ""
  teams:
    - id: platform
    - id: platform
  projects:
    - id: editor
      team_id: missing-team
  users:
    - id: alice
      team_id: platform
      project_ids: [missing-project]
      profile: low_adopter
models:
  - id: cheap
    provider: ""
    model: ""
    cost_per_1k_tokens_usd: 0
strategies:
  - id: pooled
    policy:
      version: 0
      budgets:
        - id: ""
          limit_usd: 0
"#,
        )
        .expect("simulation parses");

        let error = validate_simulation(&simulation).expect_err("simulation should be invalid");
        let message = error.to_string();
        assert!(message.contains("simulation version must be 1"));
        assert!(message.contains("simulation company id must not be empty"));
        assert!(message.contains("simulation horizon_days must be greater than zero"));
        assert!(message.contains("duplicate simulation team id platform"));
        assert!(message.contains("references unknown team missing-team"));
        assert!(message.contains("references unknown project missing-project"));
        assert!(message.contains("must not have an empty provider"));
        assert!(message.contains("cost_per_1k_tokens_usd must be positive"));
        assert!(message.contains("simulation strategy pooled policy invalid"));
    }

    #[test]
    fn checked_in_simulation_example_validates() {
        let simulation: SimulationFile =
            serde_yaml::from_str(include_str!("../examples/simulations/synthetic-company.noet.yaml"))
                .expect("example simulation parses");

        validate_simulation(&simulation).expect("example simulation is valid");
        assert_eq!(simulation.strategies.len(), 2);
    }

    #[test]
    fn synthetic_demand_is_deterministic_for_a_seed() {
        let simulation: SimulationFile =
            serde_yaml::from_str(include_str!("../examples/simulations/synthetic-company.noet.yaml"))
                .expect("example simulation parses");

        let first = generate_synthetic_demand(&simulation).expect("first demand");
        let second = generate_synthetic_demand(&simulation).expect("second demand");
        assert_eq!(first, second);

        let mut changed = simulation.clone();
        changed.seed += 1;
        let different = generate_synthetic_demand(&changed).expect("different seed demand");
        assert_ne!(first, different);
    }

    #[test]
    fn synthetic_demand_reflects_profile_shapes() {
        let simulation: SimulationFile =
            serde_yaml::from_str(include_str!("../examples/simulations/synthetic-company.noet.yaml"))
                .expect("example simulation parses");
        let demand = generate_synthetic_demand(&simulation).expect("demand");

        let power_count = demand.iter().filter(|request| request.subject == "user:alice").count();
        let steady_count = demand.iter().filter(|request| request.subject == "user:ben").count();
        let low_count = demand.iter().filter(|request| request.subject == "user:chloe").count();
        assert!(power_count > steady_count);
        assert!(steady_count > low_count);

        let experimenter_models: BTreeSet<&str> = demand
            .iter()
            .filter(|request| request.subject == "user:diego")
            .map(|request| request.model_id.as_str())
            .collect();
        assert!(experimenter_models.len() >= 2);

        let loop_risk_requests: Vec<&SyntheticDemandRequest> = demand
            .iter()
            .filter(|request| request.subject == "user:eva")
            .collect();
        assert!(loop_risk_requests.iter().any(|request| request.loop_risk));
        assert!(
            loop_risk_requests
                .iter()
                .any(|request| request.tool_call_count >= 6 || request.estimated_tokens >= 40_000)
        );
    }
}
