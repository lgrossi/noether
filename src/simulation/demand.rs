use std::collections::BTreeMap;

use crate::contract::AuthorizeRequest;
use crate::error::NoetError;

use super::{
    SimulationFile, SimulationModel, SyntheticDemandRequest, UsageProfile, validate_simulation,
};

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

fn profile_request_count(profile: UsageProfile, rng: &mut DeterministicRng, day_index: u32) -> u32 {
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

pub(super) fn synthetic_authorize_request(
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
