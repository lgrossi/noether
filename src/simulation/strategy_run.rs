use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::contract::{AuthorizeDecision, DecisionOutcome, FinalizeReservation, UsageObservation};
use crate::error::NoetError;
use crate::ledger::{
    AsyncPostgresLedgerOptions, BudgetLedger, SimulationLedgerBatch, TraceReportItem, UsageReport,
};

use super::{
    SimulationDatabaseLocation, SimulationFile, SimulationModelMixEntry, SimulationStrategy,
    SimulationStrategyContext, SimulationStrategyReport, SimulationStrategyTimingReport,
    SyntheticDemandRequest, budget_total_cap_usd, create_postgres_schema, elapsed_ms,
    encode_path_component, fairness_score, is_limit_rule_id, postgres_url_with_search_path,
    redact_database_url, simulation_postgres_schema, strategy_policy_moves,
    synthetic_authorize_request,
};

fn initial_strategy_report(
    strategy: &SimulationStrategy,
    total_requests: usize,
    strategy_dir_relative: &Path,
    database: SimulationDatabaseLocation,
    db_path: PathBuf,
) -> SimulationStrategyReport {
    SimulationStrategyReport {
        id: strategy.id.clone(),
        description: strategy.description.clone(),
        policy_moves: strategy_policy_moves(&strategy.policy),
        total_requests: total_requests as u64,
        allowed_requests: 0,
        warned_requests: 0,
        denied_requests: 0,
        fallback_count: 0,
        limit_hit_count: 0,
        total_cost_usd: 0.0,
        unused_budget_usd: 0.0,
        useful_work_blocked_score: 0,
        runaway_spend_prevented_usd: 0.0,
        adoption_coverage: 0.0,
        fairness_score: 0.0,
        unused_protected_opportunity_usd: 0.0,
        low_adopter_count: 0,
        high_adopter_count: 0,
        model_mix: Vec::new(),
        carryover_liability_usd: 0.0,
        exhaustion_day: None,
        database: Some(database),
        timing: None,
        db_path,
        usage_report_path: strategy_dir_relative.join("usage-report.json"),
        decisions_report_path: strategy_dir_relative.join("decisions-report.json"),
    }
}

#[derive(Default)]
struct SimulationStrategyTotals {
    users_with_access: BTreeSet<String>,
    user_spend: BTreeMap<String, f64>,
    model_mix: BTreeMap<String, (u64, f64)>,
}

fn apply_authorize_decision_to_report(
    report: &mut SimulationStrategyReport,
    totals: &mut SimulationStrategyTotals,
    request: &SyntheticDemandRequest,
    decision: &AuthorizeDecision,
) -> bool {
    let limit_hit_count = decision
        .explanations
        .iter()
        .filter(|explanation| is_limit_rule_id(&explanation.rule_id))
        .count() as u64;
    if decision
        .explanations
        .iter()
        .any(|explanation| explanation.reason.starts_with("selected fallback budget"))
    {
        report.fallback_count += 1;
    }
    report.limit_hit_count += limit_hit_count;
    match decision.outcome {
        DecisionOutcome::Allow => {
            report.allowed_requests += 1;
            totals.users_with_access.insert(request.subject.clone());
            true
        }
        DecisionOutcome::Warn => {
            report.warned_requests += 1;
            totals.users_with_access.insert(request.subject.clone());
            true
        }
        DecisionOutcome::Deny => {
            report.denied_requests += 1;
            report.useful_work_blocked_score += request.useful_work_score as u64;
            if request.loop_risk || limit_hit_count > 0 {
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
            false
        }
    }
}

fn simulation_finalize_payload(
    request: &SyntheticDemandRequest,
    strategy_id: &str,
) -> FinalizeReservation {
    FinalizeReservation {
        reservation_id: None,
        outcome: crate::contract::FinalizeOutcome::Success,
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
                serde_json::Value::String(format!("{}:{}", strategy_id, request.request_id)),
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
    }
}

fn record_finalized_simulation_usage(
    totals: &mut SimulationStrategyTotals,
    request: &SyntheticDemandRequest,
) {
    *totals
        .user_spend
        .entry(request.subject.clone())
        .or_insert(0.0) += request.estimated_cost_usd;
    let entry = totals
        .model_mix
        .entry(request.model_id.clone())
        .or_insert((0_u64, 0.0_f64));
    entry.0 += 1;
    entry.1 += request.estimated_cost_usd;
}

fn finish_strategy_report(
    file: &SimulationFile,
    strategy: &SimulationStrategy,
    report: &mut SimulationStrategyReport,
    usage: &UsageReport,
    totals: SimulationStrategyTotals,
) {
    report.total_cost_usd = usage.total_cost_usd;
    report.unused_budget_usd = (strategy
        .policy
        .budgets
        .iter()
        .filter_map(budget_total_cap_usd)
        .sum::<f64>()
        - usage.total_cost_usd)
        .max(0.0);
    report.adoption_coverage = if file.company.users.is_empty() {
        0.0
    } else {
        totals.users_with_access.len() as f64 / file.company.users.len() as f64
    };
    report.fairness_score = fairness_score(&file.company.users, &totals.user_spend);
    report.model_mix = totals
        .model_mix
        .into_iter()
        .map(
            |(model_id, (requests, total_cost_usd))| SimulationModelMixEntry {
                model_id,
                requests,
                total_cost_usd,
            },
        )
        .collect();
    report.model_mix.sort_by(|left, right| {
        right
            .total_cost_usd
            .partial_cmp(&left.total_cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.model_id.cmp(&right.model_id))
    });
    report.carryover_liability_usd = usage
        .protected_adoption
        .as_ref()
        .map(|adoption| adoption.carryover_liability_usd)
        .unwrap_or_default();
    report.unused_protected_opportunity_usd = usage
        .protected_adoption
        .as_ref()
        .map(|adoption| adoption.unused_protected_opportunity_usd)
        .unwrap_or_default();
    report.low_adopter_count = usage
        .protected_adoption
        .as_ref()
        .map(|adoption| adoption.low_adopters.len() as u64)
        .unwrap_or_default();
    report.high_adopter_count = usage
        .protected_adoption
        .as_ref()
        .map(|adoption| adoption.high_adopters.len() as u64)
        .unwrap_or_default();
}

pub(super) fn run_sqlite_strategy(
    context: SimulationStrategyContext,
) -> Result<SimulationStrategyReport, NoetError> {
    let total_started = std::time::Instant::now();
    let init_started = std::time::Instant::now();
    let strategy_dir = context.out_dir.join(&context.strategy_dir_relative);
    std::fs::create_dir_all(&strategy_dir)?;
    let db_path = strategy_dir.join("simulation.sqlite");
    if db_path.exists() {
        std::fs::remove_file(&db_path)?;
    }

    let mut ledger = BudgetLedger::open_sqlite(&db_path)?;
    let init_ms = elapsed_ms(init_started);
    let mut report = initial_strategy_report(
        &context.strategy,
        context.demand.len(),
        &context.strategy_dir_relative,
        SimulationDatabaseLocation::Sqlite {
            path: context.strategy_dir_relative.join("simulation.sqlite"),
        },
        context.strategy_dir_relative.join("simulation.sqlite"),
    );
    let mut totals = SimulationStrategyTotals::default();

    let replay_started = std::time::Instant::now();
    for request in context.demand.iter() {
        let authorize = synthetic_authorize_request(request, &context.strategy.id);
        let decision = ledger.try_authorize(Some(&context.strategy.policy), &authorize)?;
        if !apply_authorize_decision_to_report(&mut report, &mut totals, request, &decision) {
            continue;
        }

        if let Some(reservation) = &decision.reservation {
            let finalize = simulation_finalize_payload(request, &context.strategy.id);
            let _ = ledger.finalize(&reservation.id, &finalize)?;
            record_finalized_simulation_usage(&mut totals, request);
        }
    }
    let replay_ms = elapsed_ms(replay_started);

    let report_started = std::time::Instant::now();
    let usage = ledger.usage_report()?;
    let decisions = ledger.decisions_report()?;
    finish_strategy_report(
        &context.file,
        &context.strategy,
        &mut report,
        &usage,
        totals,
    );
    let report_ms = elapsed_ms(report_started);
    let artifact_started = std::time::Instant::now();
    write_strategy_artifacts(&context.out_dir, &report, &usage, &decisions)?;
    let artifact_ms = elapsed_ms(artifact_started);
    report.timing = Some(SimulationStrategyTimingReport {
        total_ms: elapsed_ms(total_started),
        init_ms,
        replay_ms,
        persist_ms: 0.0,
        report_ms,
        artifact_ms,
    });
    Ok(report)
}

pub(super) async fn run_postgres_strategy(
    context: SimulationStrategyContext,
    database_url: String,
    options: AsyncPostgresLedgerOptions,
) -> Result<SimulationStrategyReport, NoetError> {
    let total_started = std::time::Instant::now();
    let init_started = std::time::Instant::now();
    let strategy_slug = encode_path_component(&context.strategy.id, "simulation");
    let strategy_dir = context.out_dir.join(&context.strategy_dir_relative);
    std::fs::create_dir_all(&strategy_dir)?;

    let schema = simulation_postgres_schema(&strategy_slug);
    create_postgres_schema(&database_url, &schema).await?;
    let scoped_url = postgres_url_with_search_path(&database_url, &schema);
    tokio::task::spawn_blocking(move || {
        let mut ledger = BudgetLedger::default();
        let mut batch = SimulationLedgerBatch::default();
        let init_ms = elapsed_ms(init_started);
        let mut report = initial_strategy_report(
            &context.strategy,
            context.demand.len(),
            &context.strategy_dir_relative,
            SimulationDatabaseLocation::Postgres {
                url: redact_database_url(&scoped_url),
            },
            context.strategy_dir_relative.join("postgres"),
        );
        let mut totals = SimulationStrategyTotals::default();

        let replay_started = std::time::Instant::now();
        for request in context.demand.iter() {
            let authorize = synthetic_authorize_request(request, &context.strategy.id);
            let decision = ledger.try_authorize(Some(&context.strategy.policy), &authorize)?;
            ledger.capture_simulation_decision(
                &mut batch,
                Some(&context.strategy.policy),
                &authorize,
                &decision,
            )?;
            if !apply_authorize_decision_to_report(&mut report, &mut totals, request, &decision) {
                continue;
            }

            if let Some(reservation) = &decision.reservation {
                let finalize = simulation_finalize_payload(request, &context.strategy.id);
                let reservation = ledger.finalize(&reservation.id, &finalize)?;
                ledger.capture_simulation_finalization(&mut batch, &reservation, &finalize)?;
                record_finalized_simulation_usage(&mut totals, request);
            }
        }
        let replay_ms = elapsed_ms(replay_started);

        let persist_started = std::time::Instant::now();
        ledger.persist_simulation_batch_to_postgres_with_options(&scoped_url, &batch, &options)?;
        let persist_ms = elapsed_ms(persist_started);

        let report_started = std::time::Instant::now();
        let report_ledger = BudgetLedger::open_postgres(&scoped_url)?;
        let usage = report_ledger.usage_report()?;
        let decisions = report_ledger.decisions_report()?;
        finish_strategy_report(
            &context.file,
            &context.strategy,
            &mut report,
            &usage,
            totals,
        );
        let report_ms = elapsed_ms(report_started);
        let artifact_started = std::time::Instant::now();
        write_strategy_artifacts(&context.out_dir, &report, &usage, &decisions)?;
        let artifact_ms = elapsed_ms(artifact_started);
        report.timing = Some(SimulationStrategyTimingReport {
            total_ms: elapsed_ms(total_started),
            init_ms,
            replay_ms,
            persist_ms,
            report_ms,
            artifact_ms,
        });
        Ok(report)
    })
    .await
    .map_err(|err| NoetError::InvalidConfig(format!("Postgres simulation task failed: {err}")))?
}

fn write_strategy_artifacts(
    out_dir: &Path,
    report: &SimulationStrategyReport,
    usage: &UsageReport,
    decisions: &[TraceReportItem],
) -> Result<(), NoetError> {
    std::fs::write(
        out_dir.join(&report.usage_report_path),
        serde_json::to_vec_pretty(usage)?,
    )?;
    std::fs::write(
        out_dir.join(&report.decisions_report_path),
        serde_json::to_vec_pretty(decisions)?,
    )?;
    Ok(())
}
