use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use native_tls::TlsConnector;
use postgres::{
    Client as PostgresClient, NoTls, Row as PostgresRow, Transaction as PostgresTransaction,
    types::ToSql as PostgresToSql,
};
use postgres_native_tls::MakeTlsConnector as PostgresTls;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_postgres::{
    Client as AsyncPostgresClient, GenericClient, NoTls as AsyncNoTls, Row as AsyncPostgresRow,
    Statement,
};
use uuid::Uuid;

use crate::approval_audit::{ApprovalAuditEvent, ApprovalAuditStore};
use crate::config::{AdvisoryConfig, TipCatalogMode};
use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, BudgetRule, DecisionExplanation, DecisionOutcome,
    DecisionSeverity, EvalAnnotation, FinalizeReservation, PolicyAction, Reservation,
    ReservationStatus, RuleMatch, SpendWindowBy, SpendWindowMode, ToolEvent, TraceEvent,
    UsageObservation,
};
use crate::error::NoetError;
use crate::policy::{
    PolicyFile, budget_model_allowed, budget_rule_matches, budget_scope_matches,
    matching_policy_explanations, parse_limit_window, specificity_order,
};

#[derive(Default)]
pub struct BudgetLedger {
    advisory_config: AdvisoryConfig,
    limit_windows: HashMap<(String, String, String), WindowState>,
    allocation_buckets: HashMap<(String, String), AllocationBucketState>,
    advisory_cadence: HashMap<(String, String, String), DateTime<Utc>>,
    pending_advisory_cadence: HashMap<(String, String, String), DateTime<Utc>>,
    rolling_spend_buckets: HashMap<(String, String, String, DateTime<Utc>), f64>,
    reservations: HashMap<String, StoredReservation>,
    last_selected_budget_id: Option<String>,
    last_limit_hits: Vec<DecisionLimitHitReport>,
    events_count: u64,
    store: LedgerStore,
}

const WARN_ADVISORY_COOLDOWN: Duration = Duration::hours(4);
const ENABLEMENT_NOTIFICATION_COOLDOWN: Duration = Duration::hours(4);
const ENABLEMENT_UNDER_PACE_FRACTION: f64 = 0.5;

#[derive(Default)]
enum LedgerStore {
    #[default]
    InMemory,
    Sqlite(Connection),
    Postgres(Arc<SyncPostgresClient>),
}

#[derive(Clone, Default)]
struct LedgerStateSnapshot {
    limit_windows: HashMap<(String, String, String), WindowState>,
    allocation_buckets: HashMap<(String, String), AllocationBucketState>,
    rolling_spend_buckets: HashMap<(String, String, String, DateTime<Utc>), f64>,
    pending_advisory_cadence: HashMap<(String, String, String), DateTime<Utc>>,
    reservations: HashMap<String, StoredReservation>,
    selected_budget_id: Option<String>,
    limit_hits: Vec<DecisionLimitHitReport>,
}

#[derive(Default)]
pub struct SimulationLedgerBatch {
    decisions: Vec<SimulationDecisionRow>,
    reservations: Vec<SimulationReservationRow>,
    reservation_index: HashMap<String, usize>,
    reservation_scopes: Vec<SimulationReservationScopeRow>,
    usage_observations: Vec<SimulationUsageObservationRow>,
}

struct SimulationDecisionRow {
    decision_id: String,
    trace_id: Option<String>,
    session_id: Option<String>,
    request_id: Option<String>,
    subject: Option<String>,
    project: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    estimated_tokens: Option<i64>,
    estimated_cost_usd: Option<f64>,
    outcome: &'static str,
    action: &'static str,
    explanations_json: String,
    metadata_json: String,
    entities_json: String,
    selected_budget_id: Option<String>,
    matched_entity: Option<String>,
    selection_reason: Option<String>,
    rejected_budget_id: Option<String>,
    rejected_budget_reason: Option<String>,
    model_check: Option<String>,
    budget_window_remaining_usd: Option<f64>,
    routing_json: String,
    limit_hits_json: String,
    max_tool_calls: Option<i64>,
    max_agent_steps: Option<i64>,
    max_retries: Option<i64>,
    app_run_key: String,
    created_at: String,
}

struct SimulationReservationRow {
    id: String,
    decision_id: String,
    trace_id: Option<String>,
    amount_usd: f64,
    estimated_amount_usd: f64,
    actual_amount_usd: Option<f64>,
    currency: String,
    status: &'static str,
    created_at: String,
    expires_at: String,
    finalized_at: Option<String>,
    budget_rule_ids_json: String,
    limit_window_spends_json: String,
    allocation_spends_json: String,
}

struct SimulationReservationScopeRow {
    reservation_id: String,
    rule_id: String,
    limit_id: String,
    scope_key: String,
    amount_usd: f64,
    created_at: String,
}

struct SimulationUsageObservationRow {
    id: String,
    reservation_id: String,
    trace_id: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    cost_usd: Option<f64>,
    latency_ms: Option<i64>,
    stop_reason: Option<String>,
    source: &'static str,
    metadata_json: String,
    created_at: String,
}

fn insert_simulation_decisions_postgres(
    tx: &mut PostgresTransaction<'_>,
    rows: &[SimulationDecisionRow],
) -> Result<(), NoetError> {
    if rows.is_empty() {
        return Ok(());
    }
    let decision_ids = rows
        .iter()
        .map(|row| row.decision_id.clone())
        .collect::<Vec<_>>();
    let trace_ids = rows
        .iter()
        .map(|row| row.trace_id.clone())
        .collect::<Vec<_>>();
    let session_ids = rows
        .iter()
        .map(|row| row.session_id.clone())
        .collect::<Vec<_>>();
    let request_ids = rows
        .iter()
        .map(|row| row.request_id.clone())
        .collect::<Vec<_>>();
    let subjects = rows
        .iter()
        .map(|row| row.subject.clone())
        .collect::<Vec<_>>();
    let projects = rows
        .iter()
        .map(|row| row.project.clone())
        .collect::<Vec<_>>();
    let providers = rows
        .iter()
        .map(|row| row.provider.clone())
        .collect::<Vec<_>>();
    let models = rows.iter().map(|row| row.model.clone()).collect::<Vec<_>>();
    let estimated_tokens = rows
        .iter()
        .map(|row| row.estimated_tokens)
        .collect::<Vec<_>>();
    let estimated_costs = rows
        .iter()
        .map(|row| row.estimated_cost_usd)
        .collect::<Vec<_>>();
    let outcomes = rows
        .iter()
        .map(|row| row.outcome.to_owned())
        .collect::<Vec<_>>();
    let actions = rows
        .iter()
        .map(|row| row.action.to_owned())
        .collect::<Vec<_>>();
    let explanations = rows
        .iter()
        .map(|row| row.explanations_json.clone())
        .collect::<Vec<_>>();
    let metadata = rows
        .iter()
        .map(|row| row.metadata_json.clone())
        .collect::<Vec<_>>();
    let entities = rows
        .iter()
        .map(|row| row.entities_json.clone())
        .collect::<Vec<_>>();
    let selected_budget_ids = rows
        .iter()
        .map(|row| row.selected_budget_id.clone())
        .collect::<Vec<_>>();
    let matched_entities = rows
        .iter()
        .map(|row| row.matched_entity.clone())
        .collect::<Vec<_>>();
    let selection_reasons = rows
        .iter()
        .map(|row| row.selection_reason.clone())
        .collect::<Vec<_>>();
    let rejected_budget_ids = rows
        .iter()
        .map(|row| row.rejected_budget_id.clone())
        .collect::<Vec<_>>();
    let rejected_budget_reasons = rows
        .iter()
        .map(|row| row.rejected_budget_reason.clone())
        .collect::<Vec<_>>();
    let model_checks = rows
        .iter()
        .map(|row| row.model_check.clone())
        .collect::<Vec<_>>();
    let budget_remaining = rows
        .iter()
        .map(|row| row.budget_window_remaining_usd)
        .collect::<Vec<_>>();
    let routing_json = rows
        .iter()
        .map(|row| row.routing_json.clone())
        .collect::<Vec<_>>();
    let limit_hits_json = rows
        .iter()
        .map(|row| row.limit_hits_json.clone())
        .collect::<Vec<_>>();
    let max_tool_calls = rows
        .iter()
        .map(|row| row.max_tool_calls)
        .collect::<Vec<_>>();
    let max_agent_steps = rows
        .iter()
        .map(|row| row.max_agent_steps)
        .collect::<Vec<_>>();
    let max_retries = rows.iter().map(|row| row.max_retries).collect::<Vec<_>>();
    let app_run_keys = rows
        .iter()
        .map(|row| row.app_run_key.clone())
        .collect::<Vec<_>>();
    let created_at = rows
        .iter()
        .map(|row| row.created_at.clone())
        .collect::<Vec<_>>();

    tx.execute(
        "
        INSERT INTO decisions (
            decision_id, trace_id, session_id, request_id, subject, project, provider, model,
            estimated_tokens, estimated_cost_usd, outcome, action, explanations_json, metadata_json,
            entities_json, selected_budget_id, matched_entity, selection_reason, rejected_budget_id,
            rejected_budget_reason, model_check, budget_window_remaining_usd, routing_json,
            limit_hits_json, max_tool_calls, max_agent_steps, max_retries, app_run_key, created_at
        )
        SELECT * FROM UNNEST(
            $1::text[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[],
            $8::text[], $9::bigint[], $10::double precision[], $11::text[], $12::text[],
            $13::text[], $14::text[], $15::text[], $16::text[], $17::text[], $18::text[],
            $19::text[], $20::text[], $21::text[], $22::double precision[], $23::text[],
            $24::text[], $25::bigint[], $26::bigint[], $27::bigint[], $28::text[], $29::text[]
        )
        ",
        &[
            &decision_ids,
            &trace_ids,
            &session_ids,
            &request_ids,
            &subjects,
            &projects,
            &providers,
            &models,
            &estimated_tokens,
            &estimated_costs,
            &outcomes,
            &actions,
            &explanations,
            &metadata,
            &entities,
            &selected_budget_ids,
            &matched_entities,
            &selection_reasons,
            &rejected_budget_ids,
            &rejected_budget_reasons,
            &model_checks,
            &budget_remaining,
            &routing_json,
            &limit_hits_json,
            &max_tool_calls,
            &max_agent_steps,
            &max_retries,
            &app_run_keys,
            &created_at,
        ],
    )?;
    Ok(())
}

fn insert_simulation_reservations_postgres(
    tx: &mut PostgresTransaction<'_>,
    rows: &[SimulationReservationRow],
) -> Result<(), NoetError> {
    if rows.is_empty() {
        return Ok(());
    }
    let ids = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
    let decision_ids = rows
        .iter()
        .map(|row| row.decision_id.clone())
        .collect::<Vec<_>>();
    let amounts = rows.iter().map(|row| row.amount_usd).collect::<Vec<_>>();
    let estimated_amounts = rows
        .iter()
        .map(|row| row.estimated_amount_usd)
        .collect::<Vec<_>>();
    let actual_amounts = rows
        .iter()
        .map(|row| row.actual_amount_usd)
        .collect::<Vec<_>>();
    let currencies = rows
        .iter()
        .map(|row| row.currency.clone())
        .collect::<Vec<_>>();
    let statuses = rows
        .iter()
        .map(|row| row.status.to_owned())
        .collect::<Vec<_>>();
    let created_at = rows
        .iter()
        .map(|row| row.created_at.clone())
        .collect::<Vec<_>>();
    let expires_at = rows
        .iter()
        .map(|row| row.expires_at.clone())
        .collect::<Vec<_>>();
    let finalized_at = rows
        .iter()
        .map(|row| row.finalized_at.clone())
        .collect::<Vec<_>>();
    let budget_rule_ids = rows
        .iter()
        .map(|row| row.budget_rule_ids_json.clone())
        .collect::<Vec<_>>();
    let limit_window_spends = rows
        .iter()
        .map(|row| row.limit_window_spends_json.clone())
        .collect::<Vec<_>>();
    let allocation_spends = rows
        .iter()
        .map(|row| row.allocation_spends_json.clone())
        .collect::<Vec<_>>();

    tx.execute(
        "
        INSERT INTO reservations (
            id, decision_id, amount_usd, estimated_amount_usd, actual_amount_usd, currency, status,
            created_at, expires_at, finalized_at, budget_rule_ids_json, limit_window_spends_json,
            allocation_spends_json
        )
        SELECT * FROM UNNEST(
            $1::text[], $2::text[], $3::double precision[], $4::double precision[],
            $5::double precision[], $6::text[], $7::text[], $8::text[], $9::text[], $10::text[],
            $11::text[], $12::text[], $13::text[]
        )
        ",
        &[
            &ids,
            &decision_ids,
            &amounts,
            &estimated_amounts,
            &actual_amounts,
            &currencies,
            &statuses,
            &created_at,
            &expires_at,
            &finalized_at,
            &budget_rule_ids,
            &limit_window_spends,
            &allocation_spends,
        ],
    )?;
    Ok(())
}

fn insert_simulation_reservation_scopes_postgres(
    tx: &mut PostgresTransaction<'_>,
    rows: &[SimulationReservationScopeRow],
) -> Result<(), NoetError> {
    if rows.is_empty() {
        return Ok(());
    }
    let reservation_ids = rows
        .iter()
        .map(|row| row.reservation_id.clone())
        .collect::<Vec<_>>();
    let rule_ids = rows
        .iter()
        .map(|row| row.rule_id.clone())
        .collect::<Vec<_>>();
    let limit_ids = rows
        .iter()
        .map(|row| row.limit_id.clone())
        .collect::<Vec<_>>();
    let scope_keys = rows
        .iter()
        .map(|row| row.scope_key.clone())
        .collect::<Vec<_>>();
    let amounts = rows.iter().map(|row| row.amount_usd).collect::<Vec<_>>();
    let created_at = rows
        .iter()
        .map(|row| row.created_at.clone())
        .collect::<Vec<_>>();
    tx.execute(
        "
        INSERT INTO reservation_limit_scopes (
            reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at
        )
        SELECT * FROM UNNEST(
            $1::text[], $2::text[], $3::text[], $4::text[], $5::double precision[], $6::text[]
        )
        ",
        &[
            &reservation_ids,
            &rule_ids,
            &limit_ids,
            &scope_keys,
            &amounts,
            &created_at,
        ],
    )?;
    Ok(())
}

fn insert_simulation_usage_observations_postgres(
    tx: &mut PostgresTransaction<'_>,
    rows: &[SimulationUsageObservationRow],
) -> Result<(), NoetError> {
    if rows.is_empty() {
        return Ok(());
    }
    let ids = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
    let reservation_ids = rows
        .iter()
        .map(|row| row.reservation_id.clone())
        .collect::<Vec<_>>();
    let trace_ids = rows
        .iter()
        .map(|row| row.trace_id.clone())
        .collect::<Vec<_>>();
    let providers = rows
        .iter()
        .map(|row| row.provider.clone())
        .collect::<Vec<_>>();
    let models = rows.iter().map(|row| row.model.clone()).collect::<Vec<_>>();
    let input_tokens = rows.iter().map(|row| row.input_tokens).collect::<Vec<_>>();
    let output_tokens = rows.iter().map(|row| row.output_tokens).collect::<Vec<_>>();
    let total_tokens = rows.iter().map(|row| row.total_tokens).collect::<Vec<_>>();
    let costs = rows.iter().map(|row| row.cost_usd).collect::<Vec<_>>();
    let latency = rows.iter().map(|row| row.latency_ms).collect::<Vec<_>>();
    let stop_reasons = rows
        .iter()
        .map(|row| row.stop_reason.clone())
        .collect::<Vec<_>>();
    let sources = rows
        .iter()
        .map(|row| row.source.to_owned())
        .collect::<Vec<_>>();
    let metadata = rows
        .iter()
        .map(|row| row.metadata_json.clone())
        .collect::<Vec<_>>();
    let created_at = rows
        .iter()
        .map(|row| row.created_at.clone())
        .collect::<Vec<_>>();
    tx.execute(
        "
        INSERT INTO usage_observations (
            id, reservation_id, trace_id, provider, model, input_tokens, output_tokens,
            total_tokens, cost_usd, latency_ms, stop_reason, source, metadata_json, created_at
        )
        SELECT * FROM UNNEST(
            $1::text[], $2::text[], $3::text[], $4::text[], $5::text[], $6::bigint[],
            $7::bigint[], $8::bigint[], $9::double precision[], $10::bigint[], $11::text[],
            $12::text[], $13::text[], $14::text[]
        )
        ",
        &[
            &ids,
            &reservation_ids,
            &trace_ids,
            &providers,
            &models,
            &input_tokens,
            &output_tokens,
            &total_tokens,
            &costs,
            &latency,
            &stop_reasons,
            &sources,
            &metadata,
            &created_at,
        ],
    )?;
    Ok(())
}

fn upsert_simulation_limit_windows_postgres(
    tx: &mut PostgresTransaction<'_>,
    windows: &HashMap<(String, String, String), WindowState>,
) -> Result<(), NoetError> {
    if windows.is_empty() {
        return Ok(());
    }
    let mut rule_ids = Vec::with_capacity(windows.len());
    let mut limit_ids = Vec::with_capacity(windows.len());
    let mut scope_keys = Vec::with_capacity(windows.len());
    let mut started_at = Vec::with_capacity(windows.len());
    let mut used_usd = Vec::with_capacity(windows.len());
    for ((rule_id, limit_id, scope_key), window) in windows {
        rule_ids.push(rule_id.clone());
        limit_ids.push(limit_id.clone());
        scope_keys.push(scope_key.clone());
        started_at.push(window.started_at.to_rfc3339());
        used_usd.push(window.used_usd);
    }
    tx.execute(
        "
        INSERT INTO limit_window_states (rule_id, limit_id, scope_key, started_at, used_usd)
        SELECT * FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[], $5::double precision[])
        ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
            started_at = EXCLUDED.started_at,
            used_usd = EXCLUDED.used_usd
        ",
        &[&rule_ids, &limit_ids, &scope_keys, &started_at, &used_usd],
    )?;
    Ok(())
}

fn upsert_simulation_rolling_spend_buckets_postgres(
    tx: &mut PostgresTransaction<'_>,
    buckets: &HashMap<(String, String, String, DateTime<Utc>), f64>,
) -> Result<(), NoetError> {
    if buckets.is_empty() {
        return Ok(());
    }
    let mut rule_ids = Vec::with_capacity(buckets.len());
    let mut limit_ids = Vec::with_capacity(buckets.len());
    let mut scope_keys = Vec::with_capacity(buckets.len());
    let mut bucket_starts = Vec::with_capacity(buckets.len());
    let mut amounts = Vec::with_capacity(buckets.len());
    for ((rule_id, limit_id, scope_key, bucket_start), amount) in buckets {
        rule_ids.push(rule_id.clone());
        limit_ids.push(limit_id.clone());
        scope_keys.push(scope_key.clone());
        bucket_starts.push(bucket_start.to_rfc3339());
        amounts.push(*amount);
    }
    tx.execute(
        "
        INSERT INTO rolling_spend_buckets (rule_id, limit_id, scope_key, bucket_start, amount_usd)
        SELECT * FROM UNNEST(
            $1::text[], $2::text[], $3::text[], $4::text[], $5::double precision[]
        )
        ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
            amount_usd = EXCLUDED.amount_usd
        ",
        &[&rule_ids, &limit_ids, &scope_keys, &bucket_starts, &amounts],
    )?;
    Ok(())
}

fn upsert_simulation_allocation_buckets_postgres(
    tx: &mut PostgresTransaction<'_>,
    buckets: &HashMap<(String, String), AllocationBucketState>,
) -> Result<(), NoetError> {
    if buckets.is_empty() {
        return Ok(());
    }
    let mut rule_ids = Vec::with_capacity(buckets.len());
    let mut entity_keys = Vec::with_capacity(buckets.len());
    let mut started_at = Vec::with_capacity(buckets.len());
    let mut protected_amounts = Vec::with_capacity(buckets.len());
    let mut current_grants = Vec::with_capacity(buckets.len());
    let mut carryovers = Vec::with_capacity(buckets.len());
    for ((rule_id, entity_key), bucket) in buckets {
        rule_ids.push(rule_id.clone());
        entity_keys.push(entity_key.clone());
        started_at.push(bucket.started_at.to_rfc3339());
        protected_amounts.push(bucket.protected_amount_usd);
        current_grants.push(bucket.current_grant_usd);
        carryovers.push(bucket.carryover_usd);
    }
    tx.execute(
        "
        INSERT INTO budget_allocation_buckets (
            rule_id, entity_key, started_at, protected_amount_usd, current_grant_usd, carryover_usd
        )
        SELECT * FROM UNNEST(
            $1::text[], $2::text[], $3::text[], $4::double precision[], $5::double precision[],
            $6::double precision[]
        )
        ON CONFLICT(rule_id, entity_key) DO UPDATE SET
            started_at = EXCLUDED.started_at,
            protected_amount_usd = EXCLUDED.protected_amount_usd,
            current_grant_usd = EXCLUDED.current_grant_usd,
            carryover_usd = EXCLUDED.carryover_usd
        ",
        &[
            &rule_ids,
            &entity_keys,
            &started_at,
            &protected_amounts,
            &current_grants,
            &carryovers,
        ],
    )?;
    Ok(())
}

impl LedgerStateSnapshot {
    fn routing_persistence_fields(
        &self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        decision: &AuthorizeDecision,
    ) -> RoutingPersistenceFields {
        let selected_budget_id = decision
            .reservation
            .as_ref()
            .and_then(|reservation| self.reservations.get(&reservation.id))
            .and_then(|stored| stored.budget_rule_ids.first())
            .cloned()
            .or_else(|| self.selected_budget_id.clone());

        let mut fields = RoutingPersistenceFields {
            selected_budget_id: selected_budget_id.clone(),
            ..RoutingPersistenceFields::default()
        };

        if let (Some(policy), Some(selected_budget_id)) = (policy, selected_budget_id.as_deref()) {
            if let Some(rule) = policy
                .budgets
                .iter()
                .find(|rule| rule.id == selected_budget_id)
            {
                fields.matched_entity =
                    matched_entity_and_rank(rule, request, &specificity_order(policy)).0;
                let ledger = BudgetLedger {
                    advisory_config: AdvisoryConfig::default(),
                    limit_windows: self.limit_windows.clone(),
                    allocation_buckets: self.allocation_buckets.clone(),
                    advisory_cadence: HashMap::new(),
                    pending_advisory_cadence: HashMap::new(),
                    rolling_spend_buckets: self.rolling_spend_buckets.clone(),
                    reservations: self.reservations.clone(),
                    last_selected_budget_id: self.selected_budget_id.clone(),
                    last_limit_hits: self.limit_hits.clone(),
                    events_count: 0,
                    store: LedgerStore::InMemory,
                };
                if let Some(projection) = biggest_spend_window_projection(
                    &ledger,
                    rule,
                    request,
                    0.0,
                    decision.created_at,
                ) {
                    fields.budget_window_remaining_usd =
                        Some((projection.max_usd - projection.projected_spend_usd).max(0.0));
                    fields.budget_window_mode = Some(match projection.limit_mode {
                        SpendWindowMode::Rolling => "rolling".to_owned(),
                        SpendWindowMode::Tumbling => "tumbling".to_owned(),
                    });
                    fields.budget_window_started_at = projection.window_started_at;
                    fields.budget_window_ends_at = projection.window_ends_at;
                }
                fields.tool_calls = rule.limits.tool_calls;
                fields.agent_steps = rule.limits.agent_steps;
                fields.retries = rule.limits.retries;
            }
            fields.selection_reason = decision
                .explanations
                .iter()
                .find(|explanation| explanation.rule_id == selected_budget_id)
                .map(|explanation| explanation.reason.clone());
        }

        if let Some(requested_budget_id) = request.budget_id.as_deref()
            && selected_budget_id.as_deref() != Some(requested_budget_id)
        {
            fields.rejected_budget_id = Some(requested_budget_id.to_owned());
            fields.rejected_budget_reason = decision
                .explanations
                .iter()
                .find(|explanation| explanation.rule_id == requested_budget_id)
                .map(|explanation| explanation.reason.clone());
        }

        fields.model_check = routing_model_check(decision, selected_budget_id.as_deref());
        fields
    }
}

struct SyncPostgresClient(StdMutex<PostgresClient>);

unsafe impl Send for SyncPostgresClient {}
unsafe impl Sync for SyncPostgresClient {}

#[derive(Clone, Debug)]
struct WindowState {
    started_at: DateTime<Utc>,
    used_usd: f64,
}

#[derive(Clone, Debug)]
struct AllocationBucketState {
    started_at: DateTime<Utc>,
    protected_amount_usd: f64,
    current_grant_usd: f64,
    carryover_usd: f64,
}

#[derive(Clone, Debug)]
struct StoredReservation {
    reservation: Reservation,
    estimated_cost_usd: f64,
    budget_rule_ids: Vec<String>,
    limit_window_spends: Vec<LimitWindowReservationSpend>,
    allocation_spends: Vec<AllocationReservationSpend>,
}

#[derive(Clone, Debug)]
struct BudgetCandidate {
    id: String,
    matched_entity: Option<String>,
    specificity_rank: usize,
    priority: i64,
    pressure_micros: u64,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize)]
struct AllocationReservationSpend {
    rule_id: String,
    entity_key: String,
    carryover_usd: f64,
    current_grant_usd: f64,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize)]
struct LimitWindowReservationSpend {
    rule_id: String,
    limit_id: String,
    scope_key: String,
}

#[derive(Clone, Debug)]
struct SpendWindowProjection {
    rule_id: String,
    limit_id: String,
    window_label: String,
    action: PolicyAction,
    limit_mode: SpendWindowMode,
    window_started_at: Option<DateTime<Utc>>,
    window_ends_at: Option<DateTime<Utc>>,
    current_spend_usd: f64,
    projected_spend_usd: f64,
    max_usd: f64,
    warn_at_fractions: Vec<f64>,
    warning_cadence: Option<String>,
    scope_key: String,
    window_seconds: Duration,
}

#[derive(Clone, Debug, Serialize)]
struct AuthorizeMessageHint {
    kind: String,
    rule_id: String,
    severity: DecisionSeverity,
    recommendation: MessageHintRecommendation,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_ends_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projected_spend_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    threshold_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    threshold_percent: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum MessageHintRecommendation {
    Show,
    Hide,
}

#[derive(Clone, Debug, Serialize)]
struct AuthorizeNotification {
    kind: String,
    key: String,
    severity: DecisionSeverity,
    title: String,
    body: String,
}

#[derive(Default)]
struct RoutingPersistenceFields {
    selected_budget_id: Option<String>,
    matched_entity: Option<String>,
    selection_reason: Option<String>,
    rejected_budget_id: Option<String>,
    rejected_budget_reason: Option<String>,
    model_check: Option<String>,
    budget_window_remaining_usd: Option<f64>,
    budget_window_mode: Option<String>,
    budget_window_started_at: Option<DateTime<Utc>>,
    budget_window_ends_at: Option<DateTime<Utc>>,
    tool_calls: Option<u64>,
    agent_steps: Option<u64>,
    retries: Option<u64>,
}

pub use crate::ledger_reports::*;

#[derive(Clone)]
pub struct AsyncPostgresLedger {
    pool: Arc<AsyncPostgresPool>,
    ledger: Arc<tokio::sync::Mutex<BudgetLedger>>,
    finalize_tx: Option<mpsc::Sender<AsyncFinalizeWrite>>,
    pending_async_finalizations: Arc<tokio::sync::Mutex<HashSet<String>>>,
    async_finalize_failures: Arc<AtomicU64>,
    options: AsyncPostgresLedgerOptions,
    stage_timing: bool,
}

struct AsyncPostgresStatements {
    finalize_with_usage_fast: Statement,
    finalize_without_usage_fast: Statement,
}

struct AsyncPostgresConnection {
    client: AsyncPostgresClient,
    statements: Arc<AsyncPostgresStatements>,
}

struct AsyncPostgresPool {
    connections: Vec<Arc<tokio::sync::Mutex<AsyncPostgresConnection>>>,
    next: AtomicUsize,
}

impl AsyncPostgresPool {
    async fn lock_connection(
        &self,
        timeout_ms: u64,
    ) -> Result<tokio::sync::MutexGuard<'_, AsyncPostgresConnection>, NoetError> {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.connections.len();
        tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            self.lock_any_connection(index),
        )
        .await
        .map_err(|_| {
            NoetError::GatewayTimeout(format!(
                "postgres connection acquisition exceeded {timeout_ms}ms"
            ))
        })
    }

    async fn lock_any_connection(
        &self,
        start_index: usize,
    ) -> tokio::sync::MutexGuard<'_, AsyncPostgresConnection> {
        loop {
            for offset in 0..self.connections.len() {
                let index = (start_index + offset) % self.connections.len();
                if let Ok(connection) = self.connections[index].try_lock() {
                    return connection;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }
}

#[derive(Clone, Debug)]
pub struct AsyncPostgresLedgerOptions {
    pub pool_size: usize,
    pub async_finalize: bool,
    pub finalize_queue_capacity: usize,
    pub synchronous_commit: Option<String>,
    pub acquire_timeout_ms: u64,
    pub statement_timeout_ms: u64,
    pub idle_transaction_timeout_ms: u64,
    pub lock_timeout_ms: u64,
    pub stage_timing: bool,
}

impl Default for AsyncPostgresLedgerOptions {
    fn default() -> Self {
        let mut options = std::env::var("NOET_POSTGRES_PROFILE")
            .ok()
            .and_then(|profile| Self::from_profile(&profile).ok())
            .unwrap_or_else(Self::strict);
        if let Some(pool_size) = std::env::var("NOET_POSTGRES_POOL_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
        {
            options.pool_size = pool_size.max(1);
        }
        if let Some(async_finalize) = parse_env_bool_option("NOET_POSTGRES_ASYNC_FINALIZE") {
            options.async_finalize = async_finalize;
        }
        if let Some(finalize_queue_capacity) =
            std::env::var("NOET_POSTGRES_FINALIZE_QUEUE_CAPACITY")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
        {
            options.finalize_queue_capacity = finalize_queue_capacity.max(1);
        }
        if let Some(synchronous_commit) = std::env::var("NOET_POSTGRES_SYNCHRONOUS_COMMIT")
            .ok()
            .filter(|value| !value.trim().is_empty())
        {
            options.synchronous_commit = Some(synchronous_commit);
        }
        if let Some(acquire_timeout_ms) = parse_env_u64_option("NOET_POSTGRES_ACQUIRE_TIMEOUT_MS") {
            options.acquire_timeout_ms = acquire_timeout_ms.max(1);
        }
        if let Some(statement_timeout_ms) =
            parse_env_u64_option("NOET_POSTGRES_STATEMENT_TIMEOUT_MS")
        {
            options.statement_timeout_ms = statement_timeout_ms;
        }
        if let Some(idle_transaction_timeout_ms) =
            parse_env_u64_option("NOET_POSTGRES_IDLE_TX_TIMEOUT_MS")
        {
            options.idle_transaction_timeout_ms = idle_transaction_timeout_ms;
        }
        if let Some(lock_timeout_ms) = parse_env_u64_option("NOET_POSTGRES_LOCK_TIMEOUT_MS") {
            options.lock_timeout_ms = lock_timeout_ms;
        }
        if let Some(stage_timing) = parse_env_bool_option("NOET_POSTGRES_STAGE_TIMING") {
            options.stage_timing = stage_timing;
        }
        options
    }
}

impl AsyncPostgresLedgerOptions {
    pub fn strict() -> Self {
        Self {
            pool_size: 4,
            async_finalize: false,
            finalize_queue_capacity: 1024,
            synchronous_commit: None,
            acquire_timeout_ms: 5_000,
            statement_timeout_ms: 30_000,
            idle_transaction_timeout_ms: 30_000,
            lock_timeout_ms: 0,
            stage_timing: false,
        }
    }

    pub fn performance() -> Self {
        Self {
            async_finalize: true,
            synchronous_commit: Some("off".to_owned()),
            ..Self::strict()
        }
    }

    pub fn from_profile(profile: &str) -> Result<Self, NoetError> {
        match profile.trim().to_ascii_lowercase().as_str() {
            "strict" => Ok(Self::strict()),
            "performance" => Ok(Self::performance()),
            other => Err(NoetError::InvalidConfig(format!(
                "invalid Postgres profile {other:?}; expected strict or performance"
            ))),
        }
    }
}

struct AsyncFinalizeWrite {
    reservation: Reservation,
    payload: FinalizeReservation,
    snapshot: LedgerStateSnapshot,
}

const ASYNC_AUTHORIZE_FAST_SQL: &str = "
WITH upsert_window AS (
    INSERT INTO limit_window_states (
        rule_id, limit_id, scope_key, started_at, used_usd
    ) VALUES ($30, $31, $32, $33, $34)
    ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
        started_at = EXCLUDED.started_at,
        used_usd = EXCLUDED.used_usd
    RETURNING 1
), ins_decision AS (
    INSERT INTO decisions (
        decision_id, trace_id, session_id, request_id, subject, project,
        provider, model, estimated_tokens, estimated_cost_usd, outcome,
        action, explanations_json, metadata_json, entities_json,
        selected_budget_id, matched_entity, selection_reason,
        rejected_budget_id, rejected_budget_reason, model_check,
        budget_window_remaining_usd, routing_json, limit_hits_json,
        max_tool_calls, max_agent_steps, max_retries, app_run_key, created_at
    ) VALUES (
        $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
        $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26,
        $27, $28, $29
    )
    RETURNING decision_id
), ins_reservation AS (
    INSERT INTO reservations (
        id, decision_id, amount_usd, estimated_amount_usd, currency, status,
        created_at, expires_at, budget_rule_ids_json,
        limit_window_spends_json, allocation_spends_json
    ) VALUES ($35, $1, $36, $36, $37, $38, $39, $40, $41, $42, $43)
    RETURNING id
), ins_scope AS (
    INSERT INTO reservation_limit_scopes (
        reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at
    ) VALUES ($35, $30, $31, $32, $36, $39)
    RETURNING 1
), upsert_bucket AS (
    INSERT INTO rolling_spend_buckets (
        rule_id, limit_id, scope_key, bucket_start, amount_usd
    ) VALUES ($30, $31, $32, $44, $36)
    ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
        amount_usd = rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd
    RETURNING 1
)
SELECT 1
FROM upsert_window, ins_decision, ins_reservation, ins_scope, upsert_bucket
";

const ASYNC_FINALIZE_WITH_USAGE_FAST_SQL: &str = "
WITH updated AS (
    UPDATE reservations
    SET amount_usd = $2, actual_amount_usd = $2, status = $3, finalized_at = $4
    WHERE id = $1
    RETURNING decision_id
), decision_trace AS (
    SELECT d.trace_id
    FROM decisions d
    JOIN updated u ON u.decision_id = d.decision_id
), inserted_usage AS (
    INSERT INTO usage_observations (
        id, reservation_id, trace_id, provider, model, input_tokens,
        output_tokens, total_tokens, cost_usd, latency_ms, stop_reason,
        source, metadata_json, created_at
    )
    SELECT $5, $1, COALESCE((SELECT trace_id FROM decision_trace), $6),
           $7, $8, $9, $10, $11, $12, $13, $14,
           'reservation.finalize', $15, $4
    FROM updated
    RETURNING 1
), upsert_window AS (
    INSERT INTO limit_window_states (
        rule_id, limit_id, scope_key, started_at, used_usd
    ) VALUES ($16, $17, $18, $19, $20)
    ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
        used_usd = CASE
            WHEN limit_window_states.started_at = EXCLUDED.started_at
            THEN GREATEST(limit_window_states.used_usd + EXCLUDED.used_usd, 0)
            ELSE limit_window_states.used_usd
        END
    RETURNING 1
)
SELECT 1 FROM updated, inserted_usage, upsert_window
";

const ASYNC_FINALIZE_WITHOUT_USAGE_FAST_SQL: &str = "
WITH updated AS (
    UPDATE reservations
    SET amount_usd = $2, actual_amount_usd = $2, status = $3, finalized_at = $4
    WHERE id = $1
    RETURNING 1
), upsert_window AS (
    INSERT INTO limit_window_states (
        rule_id, limit_id, scope_key, started_at, used_usd
    ) VALUES ($5, $6, $7, $8, $9)
    ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
        used_usd = CASE
            WHEN limit_window_states.started_at = EXCLUDED.started_at
            THEN GREATEST(limit_window_states.used_usd + EXCLUDED.used_usd, 0)
            ELSE limit_window_states.used_usd
        END
    RETURNING 1
)
SELECT 1 FROM updated, upsert_window
";

fn parse_env_bool_option(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn parse_env_u64_option(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn sqlite_wal_autocheckpoint() -> u64 {
    parse_env_u64_option("NOET_SQLITE_WAL_AUTOCHECKPOINT").unwrap_or(1_000)
}

fn finish_sqlite_transaction<T>(
    conn: &Connection,
    result: Result<T, NoetError>,
) -> Result<T, NoetError> {
    match result {
        Ok(value) => {
            conn.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

async fn prepare_async_postgres_statements(
    client: &AsyncPostgresClient,
) -> Result<AsyncPostgresStatements, NoetError> {
    Ok(AsyncPostgresStatements {
        finalize_with_usage_fast: client.prepare(ASYNC_FINALIZE_WITH_USAGE_FAST_SQL).await?,
        finalize_without_usage_fast: client
            .prepare(ASYNC_FINALIZE_WITHOUT_USAGE_FAST_SQL)
            .await?,
    })
}

async fn apply_postgres_connection_options(
    client: &AsyncPostgresClient,
    options: &AsyncPostgresLedgerOptions,
) -> Result<(), NoetError> {
    let mut statements = Vec::new();
    if let Some(value) = options.synchronous_commit.as_deref() {
        let value = normalized_synchronous_commit(value)?;
        statements.push(format!("SET synchronous_commit TO {value};"));
    }
    statements.push(format!(
        "SET statement_timeout TO '{}ms';",
        options.statement_timeout_ms
    ));
    statements.push(format!(
        "SET idle_in_transaction_session_timeout TO '{}ms';",
        options.idle_transaction_timeout_ms
    ));
    statements.push(format!(
        "SET lock_timeout TO '{}ms';",
        options.lock_timeout_ms
    ));
    client.batch_execute(&statements.join("\n")).await?;
    Ok(())
}

async fn lock_postgres_connection(
    connection: &Arc<tokio::sync::Mutex<AsyncPostgresConnection>>,
    timeout_ms: u64,
) -> Result<tokio::sync::MutexGuard<'_, AsyncPostgresConnection>, NoetError> {
    tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        connection.lock(),
    )
    .await
    .map_err(|_| {
        NoetError::GatewayTimeout(format!(
            "postgres connection acquisition exceeded {timeout_ms}ms"
        ))
    })
}

fn postgres_url_requires_tls(database_url: &str) -> bool {
    url::Url::parse(database_url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key.eq_ignore_ascii_case("sslmode"))
                .map(|(_, value)| value.to_ascii_lowercase())
        })
        .map(|sslmode| matches!(sslmode.as_str(), "require" | "verify-ca" | "verify-full"))
        .unwrap_or(false)
}

pub(crate) async fn connect_async_postgres_client(
    database_url: &str,
) -> Result<AsyncPostgresClient, NoetError> {
    if postgres_url_requires_tls(database_url) {
        let connector = TlsConnector::new()?;
        let connector = PostgresTls::new(connector);
        let (client, connection) = tokio_postgres::connect(database_url, connector).await?;
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::error!(error = %error, "postgres connection failed");
            }
        });
        return Ok(client);
    }

    let (client, connection) = tokio_postgres::connect(database_url, AsyncNoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(error = %error, "postgres connection failed");
        }
    });
    Ok(client)
}

async fn connect_async_postgres_connection(
    database_url: &str,
    options: &AsyncPostgresLedgerOptions,
    initialize_schema: bool,
) -> Result<Arc<tokio::sync::Mutex<AsyncPostgresConnection>>, NoetError> {
    let client = connect_async_postgres_client(database_url).await?;
    apply_postgres_connection_options(&client, options).await?;
    if initialize_schema {
        init_postgres_schema_async(&client).await?;
    }
    let statements = prepare_async_postgres_statements(&client).await?;
    Ok(Arc::new(tokio::sync::Mutex::new(AsyncPostgresConnection {
        client,
        statements: Arc::new(statements),
    })))
}

fn normalized_synchronous_commit(value: &str) -> Result<&'static str, NoetError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" => Ok("on"),
        "off" => Ok("off"),
        "local" => Ok("local"),
        "remote_write" => Ok("remote_write"),
        "remote_apply" => Ok("remote_apply"),
        other => Err(NoetError::InvalidConfig(format!(
            "invalid NOET_POSTGRES_SYNCHRONOUS_COMMIT value {other:?}; expected on, off, local, remote_write, or remote_apply"
        ))),
    }
}

async fn run_async_postgres_finalize_worker(
    connection: Arc<tokio::sync::Mutex<AsyncPostgresConnection>>,
    pending_finalizations: Arc<tokio::sync::Mutex<HashSet<String>>>,
    failures: Arc<AtomicU64>,
    options: AsyncPostgresLedgerOptions,
    mut rx: mpsc::Receiver<AsyncFinalizeWrite>,
) {
    while let Some(write) = rx.recv().await {
        let mut attempt = 0_u64;
        loop {
            attempt += 1;
            let Ok(mut connection) =
                lock_postgres_connection(&connection, options.acquire_timeout_ms).await
            else {
                failures.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            };
            let statements = connection.statements.clone();
            let result = async {
                let tx = connection.client.transaction().await?;
                tx.batch_execute("SELECT pg_advisory_xact_lock(1984111137)")
                    .await?;
                persist_finalization_write_async(&tx, &statements, &write).await?;
                tx.commit().await?;
                Ok::<_, NoetError>(())
            }
            .await;
            drop(connection);
            match result {
                Ok(()) => {
                    pending_finalizations
                        .lock()
                        .await
                        .remove(&write.reservation.id);
                    break;
                }
                Err(error) => {
                    failures.fetch_add(1, Ordering::Relaxed);
                    let retry_delay_ms = (25 * attempt).min(1_000);
                    tracing::warn!(
                        error = %error,
                        attempt,
                        retry_delay_ms,
                        "postgres async finalize persistence attempt failed; retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(retry_delay_ms)).await;
                }
            }
        }
    }
}

impl AsyncPostgresLedger {
    pub async fn set_advisory_config(&self, config: AdvisoryConfig) {
        self.ledger.lock().await.set_advisory_config(config);
    }

    pub async fn connect(database_url: &str) -> Result<Self, NoetError> {
        Self::connect_with_options(database_url, AsyncPostgresLedgerOptions::default()).await
    }

    pub async fn connect_with_options(
        database_url: &str,
        options: AsyncPostgresLedgerOptions,
    ) -> Result<Self, NoetError> {
        let pool_size = options.pool_size.max(1);
        let mut connections = Vec::with_capacity(pool_size);
        let client = connect_async_postgres_client(database_url).await?;
        apply_postgres_connection_options(&client, &options).await?;
        init_postgres_schema_async(&client).await?;
        let statements = prepare_async_postgres_statements(&client).await?;
        let mut ledger = BudgetLedger::default();
        load_async_authorization_state(&client, &mut ledger, None, Utc::now(), None).await?;
        connections.push(Arc::new(tokio::sync::Mutex::new(AsyncPostgresConnection {
            client,
            statements: Arc::new(statements),
        })));
        for _ in 1..pool_size {
            connections
                .push(connect_async_postgres_connection(database_url, &options, false).await?);
        }
        let pool = Arc::new(AsyncPostgresPool {
            connections,
            next: AtomicUsize::new(0),
        });
        let pending_async_finalizations = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
        let async_finalize_failures = Arc::new(AtomicU64::new(0));
        let finalize_tx = if options.async_finalize {
            let finalize_connection =
                connect_async_postgres_connection(database_url, &options, false).await?;
            let (tx, rx) = mpsc::channel(options.finalize_queue_capacity);
            tokio::spawn(run_async_postgres_finalize_worker(
                finalize_connection,
                pending_async_finalizations.clone(),
                async_finalize_failures.clone(),
                options.clone(),
                rx,
            ));
            Some(tx)
        } else {
            None
        };
        Ok(Self {
            pool,
            ledger: Arc::new(tokio::sync::Mutex::new(ledger)),
            finalize_tx,
            pending_async_finalizations,
            async_finalize_failures,
            options: options.clone(),
            stage_timing: options.stage_timing,
        })
    }

    pub fn async_finalize_failures(&self) -> u64 {
        self.async_finalize_failures.load(Ordering::Relaxed)
    }

    pub async fn try_authorize(
        &self,
        policy: Option<Arc<PolicyFile>>,
        request: AuthorizeRequest,
    ) -> Result<AuthorizeDecision, NoetError> {
        self.try_authorize_at(policy, request, Utc::now()).await
    }

    pub async fn try_authorize_at(
        &self,
        policy: Option<Arc<PolicyFile>>,
        request: AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> Result<AuthorizeDecision, NoetError> {
        let started = Instant::now();
        let mut ledger = self.ledger.lock().await;
        let mut connection = self
            .pool
            .lock_connection(self.options.acquire_timeout_ms)
            .await?;
        let tx = connection.client.transaction().await?;
        tx.batch_execute("SELECT pg_advisory_xact_lock(1984111137)")
            .await?;
        let pending_finalizations = if self.finalize_tx.is_some() {
            Some(self.pending_async_finalizations.lock().await.clone())
        } else {
            None
        };
        load_async_authorization_state(
            &tx,
            &mut ledger,
            policy.as_deref(),
            now,
            pending_finalizations.as_ref(),
        )
        .await?;
        let decision = ledger.try_authorize_at(policy.as_deref(), &request, now)?;
        let snapshot = ledger.state_snapshot();
        let decision_elapsed = started.elapsed();
        let db_started = Instant::now();
        persist_decision_async(&tx, &snapshot, policy.as_deref(), &request, &decision).await?;
        tx.commit().await?;
        ledger.pending_advisory_cadence.clear();
        if self.stage_timing {
            tracing::debug!(
                decision_ms = decision_elapsed.as_secs_f64() * 1000.0,
                db_ms = db_started.elapsed().as_secs_f64() * 1000.0,
                total_ms = started.elapsed().as_secs_f64() * 1000.0,
                "postgres authorize stages"
            );
        }
        Ok(decision)
    }

    pub async fn finalize(
        &self,
        reservation_id: String,
        payload: FinalizeReservation,
    ) -> Result<Reservation, NoetError> {
        let started = Instant::now();
        let finalize_tx = self.finalize_tx.as_ref().cloned();
        let (reservation, write, decision_elapsed) = {
            let mut ledger = self.ledger.lock().await;
            let mut connection = self
                .pool
                .lock_connection(self.options.acquire_timeout_ms)
                .await?;
            let statements = connection.statements.clone();
            let tx = connection.client.transaction().await?;
            tx.batch_execute("SELECT pg_advisory_xact_lock(1984111137)")
                .await?;
            let pending_finalizations = if finalize_tx.is_some() {
                Some(self.pending_async_finalizations.lock().await.clone())
            } else {
                None
            };
            load_async_finalization_state(&tx, &mut ledger, pending_finalizations.as_ref()).await?;
            if !ledger.reservations.contains_key(&reservation_id) {
                load_reservation_async(&tx, &mut ledger, &reservation_id).await?;
            }
            let already_finalized = ledger
                .reservations
                .get(&reservation_id)
                .map(|stored| stored.reservation.status == ReservationStatus::Finalized)
                .unwrap_or(false);
            let reservation = ledger.finalize(&reservation_id, &payload)?;
            if already_finalized {
                tx.commit().await?;
                return Ok(reservation);
            }
            let snapshot = ledger.state_snapshot();
            let decision_elapsed = started.elapsed();
            let write = AsyncFinalizeWrite {
                reservation: reservation.clone(),
                payload,
                snapshot,
            };
            if finalize_tx.is_none() {
                let db_started = Instant::now();
                persist_finalization_write_async(&tx, &statements, &write).await?;
                tx.commit().await?;
                if self.stage_timing {
                    tracing::debug!(
                        decision_ms = decision_elapsed.as_secs_f64() * 1000.0,
                        db_ms = db_started.elapsed().as_secs_f64() * 1000.0,
                        total_ms = started.elapsed().as_secs_f64() * 1000.0,
                        "postgres finalize stages"
                    );
                }
                return Ok(reservation);
            }
            tx.commit().await?;
            (reservation, write, decision_elapsed)
        };
        if let Some(finalize_tx) = finalize_tx {
            self.pending_async_finalizations
                .lock()
                .await
                .insert(reservation.id.clone());
            match finalize_tx.try_send(write) {
                Ok(()) => {
                    if self.stage_timing {
                        tracing::debug!(
                            decision_ms = decision_elapsed.as_secs_f64() * 1000.0,
                            total_ms = started.elapsed().as_secs_f64() * 1000.0,
                            "postgres finalize queued"
                        );
                    }
                    return Ok(reservation);
                }
                Err(mpsc::error::TrySendError::Full(write)) => {
                    self.pending_async_finalizations
                        .lock()
                        .await
                        .remove(&reservation.id);
                    tracing::warn!("postgres async finalize queue full; persisting synchronously");
                    return self
                        .persist_finalization_write(write, reservation, started, decision_elapsed)
                        .await;
                }
                Err(mpsc::error::TrySendError::Closed(write)) => {
                    self.pending_async_finalizations
                        .lock()
                        .await
                        .remove(&reservation.id);
                    tracing::warn!(
                        "postgres async finalize queue closed; persisting synchronously"
                    );
                    return self
                        .persist_finalization_write(write, reservation, started, decision_elapsed)
                        .await;
                }
            }
        }
        self.persist_finalization_write(write, reservation, started, decision_elapsed)
            .await
    }

    async fn persist_finalization_write(
        &self,
        write: AsyncFinalizeWrite,
        reservation: Reservation,
        started: Instant,
        decision_elapsed: std::time::Duration,
    ) -> Result<Reservation, NoetError> {
        let db_started = Instant::now();
        let mut connection = self
            .pool
            .lock_connection(self.options.acquire_timeout_ms)
            .await?;
        let statements = connection.statements.clone();
        let tx = connection.client.transaction().await?;
        tx.batch_execute("SELECT pg_advisory_xact_lock(1984111137)")
            .await?;
        persist_finalization_write_async(&tx, &statements, &write).await?;
        tx.commit().await?;
        if self.stage_timing {
            tracing::debug!(
                decision_ms = decision_elapsed.as_secs_f64() * 1000.0,
                db_ms = db_started.elapsed().as_secs_f64() * 1000.0,
                total_ms = started.elapsed().as_secs_f64() * 1000.0,
                "postgres finalize stages"
            );
        }
        Ok(reservation)
    }

    pub async fn record_event(&self, event: TraceEvent) -> Result<(), NoetError> {
        {
            let mut ledger = self.ledger.lock().await;
            ledger.record_event(event.clone())?;
        }
        let connection = self
            .pool
            .lock_connection(self.options.acquire_timeout_ms)
            .await?;
        persist_event_async(&connection.client, &event).await
    }
}

impl ApprovalAuditStore for BudgetLedger {
    fn approval_audit_events(&self) -> Result<Vec<ApprovalAuditEvent>, NoetError> {
        self.approval_audit_events_from_ledger()
    }
}

impl BudgetLedger {
    pub fn set_advisory_config(&mut self, config: AdvisoryConfig) {
        self.advisory_config = config;
    }

    fn state_snapshot(&self) -> LedgerStateSnapshot {
        LedgerStateSnapshot {
            limit_windows: self.limit_windows.clone(),
            allocation_buckets: self.allocation_buckets.clone(),
            rolling_spend_buckets: self.rolling_spend_buckets.clone(),
            pending_advisory_cadence: self.pending_advisory_cadence.clone(),
            reservations: self.reservations.clone(),
            selected_budget_id: self.last_selected_budget_id.clone(),
            limit_hits: self.last_limit_hits.clone(),
        }
    }

    fn load_store_state(&mut self) -> Result<(), NoetError> {
        self.load_limit_windows()?;
        self.load_allocation_buckets()?;
        self.load_active_reservations()?;
        self.load_advisory_cadence_for_store()
    }

    fn load_advisory_cadence_for_store(&mut self) -> Result<(), NoetError> {
        match &self.store {
            LedgerStore::Postgres(_) => self.load_advisory_cadence_postgres(),
            LedgerStore::Sqlite(_) => self.load_advisory_cadence_sqlite(),
            LedgerStore::InMemory => Ok(()),
        }
    }

    fn persist_authorization_state(&self, has_reservation: bool) -> Result<(), NoetError> {
        if has_reservation {
            self.persist_limit_windows()?;
            self.persist_allocation_buckets()?;
        }
        Ok(())
    }

    pub fn open_sqlite(path: &Path) -> Result<Self, NoetError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(
            None,
            "wal_autocheckpoint",
            sqlite_wal_autocheckpoint() as i64,
        )?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let schema_result = init_schema(&conn);
        finish_sqlite_transaction(&conn, schema_result)?;
        let mut ledger = Self {
            store: LedgerStore::Sqlite(conn),
            ..Self::default()
        };
        ledger.load_store_state()?;
        Ok(ledger)
    }

    pub fn open_postgres(database_url: &str) -> Result<Self, NoetError> {
        let mut pg_conn = if postgres_url_requires_tls(database_url) {
            let connector = TlsConnector::new()?;
            let connector = PostgresTls::new(connector);
            PostgresClient::connect(database_url, connector)?
        } else {
            PostgresClient::connect(database_url, NoTls)?
        };
        init_postgres_schema(&mut pg_conn)?;
        let mut ledger = Self {
            store: LedgerStore::Postgres(Arc::new(SyncPostgresClient(StdMutex::new(pg_conn)))),
            ..Self::default()
        };
        ledger.load_store_state()?;
        Ok(ledger)
    }

    fn begin_sqlite_transaction(&self) -> Result<bool, NoetError> {
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(false);
        };
        conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(true)
    }

    fn finish_sqlite_transaction<T>(
        &self,
        started: bool,
        result: Result<T, NoetError>,
    ) -> Result<T, NoetError> {
        if !started {
            return result;
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return result;
        };
        finish_sqlite_transaction(conn, result)
    }

    pub fn authorize(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
    ) -> AuthorizeDecision {
        self.try_authorize(policy, request)
            .expect("authorize decision persistence")
    }

    fn recommend_message_hint(
        &mut self,
        request: &AuthorizeRequest,
        advisory_key: &str,
        scope_key: &str,
        severity: DecisionSeverity,
        now: DateTime<Utc>,
        configured_cadence: Option<&str>,
    ) -> MessageHintRecommendation {
        if severity != DecisionSeverity::Warn {
            return MessageHintRecommendation::Show;
        }
        if self.recommend_advisory(
            &request_user_key(request),
            advisory_key,
            scope_key,
            now,
            warning_advisory_cooldown(&self.advisory_config, configured_cadence),
        ) {
            MessageHintRecommendation::Show
        } else {
            MessageHintRecommendation::Hide
        }
    }

    fn recommend_advisory(
        &mut self,
        user_key: &str,
        advisory_key: &str,
        scope_key: &str,
        now: DateTime<Utc>,
        cooldown: Duration,
    ) -> bool {
        let key = (
            user_key.to_owned(),
            advisory_key.to_owned(),
            scope_key.to_owned(),
        );
        if let Some(last_shown_at) = self.advisory_cadence.get(&key)
            && *last_shown_at + cooldown > now
        {
            return false;
        }
        self.advisory_cadence.insert(key.clone(), now);
        self.pending_advisory_cadence.insert(key, now);
        true
    }

    fn enablement_notifications(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> Vec<AuthorizeNotification> {
        let Some(policy) = policy else {
            return Vec::new();
        };
        let Some(reason) = enablement_headroom(policy, self, request, now) else {
            return Vec::new();
        };
        let user_key = request_user_key(request);
        if !self.recommend_advisory(
            &user_key,
            "notification.enablement.headroom",
            &reason.scope_key,
            now,
            ENABLEMENT_NOTIFICATION_COOLDOWN,
        ) {
            return Vec::new();
        }
        let Some(tip) = enablement_tip(&self.advisory_config, &user_key, now) else {
            return Vec::new();
        };
        vec![AuthorizeNotification {
            kind: "enablement_tip".to_owned(),
            key: tip.key,
            severity: DecisionSeverity::Info,
            title: "AI budget headroom available".to_owned(),
            body: tip.body,
        }]
    }

    pub fn try_authorize(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
    ) -> Result<AuthorizeDecision, NoetError> {
        self.try_authorize_at(policy, request, Utc::now())
    }

    pub fn try_authorize_at(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> Result<AuthorizeDecision, NoetError> {
        self.load_advisory_cadence_for_store()?;
        let outcome = AuthorizationEngine::new(self).evaluate(policy, request, now);

        let reservation = if outcome.action.halts_request() {
            None
        } else {
            Some(self.create_reservation(
                policy,
                request,
                now,
                outcome.selected_budget_id.as_deref(),
            ))
        };
        self.last_selected_budget_id = outcome.selected_budget_id.clone();
        self.last_limit_hits = outcome.limit_hits.clone();
        let has_reservation = reservation.is_some();
        let decision = AuthorizeDecision {
            decision_id: Uuid::new_v4().to_string(),
            outcome: outcome.action.decision_outcome(),
            action: outcome.action,
            reservation,
            explanations: outcome.explanations,
            metadata: authorize_metadata(&outcome.message_hints, &outcome.notifications),
            created_at: now,
        };
        let sqlite_tx = self.begin_sqlite_transaction()?;
        let persist_result = (|| {
            self.persist_authorization_state(has_reservation)?;
            self.persist_decision(
                policy,
                request,
                &decision,
                outcome.selected_budget_id.as_deref(),
                &outcome.limit_hits,
            )
        })();
        self.finish_sqlite_transaction(sqlite_tx, persist_result)?;
        Ok(decision)
    }

    pub fn try_authorize_replay_at(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> Result<AuthorizeDecision, NoetError> {
        let outcome = AuthorizationEngine::new(self).evaluate_replay(policy, request, now);

        let reservation = if outcome.action.halts_request() {
            None
        } else {
            Some(self.create_replay_reservation(
                policy,
                request,
                now,
                outcome.selected_budget_id.as_deref(),
            ))
        };

        Ok(AuthorizeDecision {
            decision_id: format!("replay-decision-{}", self.reservations.len()),
            outcome: outcome.action.decision_outcome(),
            action: outcome.action,
            reservation,
            explanations: outcome.explanations,
            metadata: message_hints_metadata(&outcome.message_hints),
            created_at: now,
        })
    }

    pub fn seed_replay_spend(&mut self, seed: ReplaySpendSeed) {
        if seed.amount_usd <= 0.0 {
            return;
        }
        match seed.mode {
            SpendWindowMode::Tumbling => {
                let key = (seed.rule_id, seed.limit_id, seed.scope_key);
                let entry = self.limit_windows.entry(key).or_insert(WindowState {
                    started_at: seed.window_started_at,
                    used_usd: 0.0,
                });
                entry.started_at = seed.window_started_at;
                entry.used_usd += seed.amount_usd;
            }
            SpendWindowMode::Rolling => {
                let bucket_key = (
                    seed.rule_id.clone(),
                    seed.limit_id.clone(),
                    seed.scope_key.clone(),
                    rolling_bucket_start(seed.seeded_at),
                );
                *self.rolling_spend_buckets.entry(bucket_key).or_insert(0.0) += seed.amount_usd;
                let id = format!(
                    "replay-seed-{}-{}-{}-{}",
                    seed.rule_id,
                    seed.limit_id,
                    seed.scope_key,
                    self.reservations.len() + 1
                );
                self.reservations.insert(
                    id.clone(),
                    StoredReservation {
                        reservation: Reservation {
                            id,
                            amount_usd: seed.amount_usd,
                            currency: "USD".to_owned(),
                            status: ReservationStatus::Finalized,
                            created_at: seed.seeded_at,
                            expires_at: seed.seeded_at,
                        },
                        estimated_cost_usd: seed.amount_usd,
                        budget_rule_ids: vec![seed.rule_id.clone()],
                        limit_window_spends: vec![LimitWindowReservationSpend {
                            rule_id: seed.rule_id,
                            limit_id: seed.limit_id,
                            scope_key: seed.scope_key,
                        }],
                        allocation_spends: Vec::new(),
                    },
                );
            }
        }
    }

    pub fn finalize(
        &mut self,
        reservation_id: &str,
        payload: &FinalizeReservation,
    ) -> Result<Reservation, NoetError> {
        payload
            .validate_accounting()
            .map_err(NoetError::InvalidConfig)?;

        let stored = self
            .reservations
            .get_mut(reservation_id)
            .ok_or_else(|| NoetError::NotFound(format!("reservation {reservation_id}")))?;

        if stored.reservation.status == ReservationStatus::Finalized {
            return Ok(stored.reservation.clone());
        }

        let actual_cost = payload
            .actual_cost_usd
            .or_else(|| payload.usage.as_ref().and_then(|usage| usage.cost_usd));
        if let Some(actual_cost) = actual_cost {
            let delta = actual_cost - stored.estimated_cost_usd;
            for spend in &stored.limit_window_spends {
                let key = (
                    spend.rule_id.clone(),
                    spend.limit_id.clone(),
                    spend.scope_key.clone(),
                );
                if let Some(window) = self.limit_windows.get_mut(&key) {
                    window.used_usd = (window.used_usd + delta).max(0.0);
                }
            }
            stored.reservation.amount_usd = actual_cost;
        }

        stored.reservation.status = ReservationStatus::Finalized;
        let reservation = stored.reservation.clone();
        let sqlite_tx = self.begin_sqlite_transaction()?;
        let persist_result = (|| {
            self.persist_finalization(&reservation, payload)?;
            self.persist_windows()?;
            self.persist_limit_windows()
        })();
        self.finish_sqlite_transaction(sqlite_tx, persist_result)?;
        Ok(reservation)
    }

    pub fn capture_simulation_decision(
        &self,
        batch: &mut SimulationLedgerBatch,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        decision: &AuthorizeDecision,
    ) -> Result<(), NoetError> {
        let trace_id = string_metadata(request, "trace_id");
        let session_id = string_metadata(request, "session_id");
        let request_id = string_metadata(request, "request_id");
        let routing = self.routing_persistence_fields(
            policy,
            request,
            decision,
            self.last_selected_budget_id.as_deref(),
        );
        let outcome = outcome_text(decision.outcome);
        let app_run_key = decision_app_run_key(
            trace_id.as_deref(),
            request.provider.as_deref(),
            request.model.as_deref(),
            outcome,
            routing.selected_budget_id.as_deref(),
            &request.metadata,
            decision.created_at,
        );
        let routing_report = decision_routing_report(&routing);
        batch.decisions.push(SimulationDecisionRow {
            decision_id: decision.decision_id.clone(),
            trace_id: trace_id.clone(),
            session_id,
            request_id,
            subject: request.subject.clone(),
            project: request.project.clone(),
            provider: request.provider.clone(),
            model: request.model.clone(),
            estimated_tokens: request.estimated_tokens.map(|value| value as i64),
            estimated_cost_usd: request.estimated_cost_usd,
            outcome,
            action: action_text(decision.action),
            explanations_json: serde_json::to_string(&decision.explanations)?,
            metadata_json: serde_json::to_string(&request.metadata)?,
            entities_json: serde_json::to_string(&request.entities)?,
            selected_budget_id: routing.selected_budget_id,
            matched_entity: routing.matched_entity,
            selection_reason: routing.selection_reason,
            rejected_budget_id: routing.rejected_budget_id,
            rejected_budget_reason: routing.rejected_budget_reason,
            model_check: routing.model_check,
            budget_window_remaining_usd: routing.budget_window_remaining_usd,
            routing_json: serde_json::to_string(&routing_report)?,
            limit_hits_json: serde_json::to_string(&self.last_limit_hits)?,
            max_tool_calls: routing.tool_calls.map(|value| value as i64),
            max_agent_steps: routing.agent_steps.map(|value| value as i64),
            max_retries: routing.retries.map(|value| value as i64),
            app_run_key,
            created_at: decision.created_at.to_rfc3339(),
        });

        if let Some(reservation) = &decision.reservation
            && let Some(stored) = self.reservations.get(&reservation.id)
        {
            batch
                .reservation_index
                .insert(reservation.id.clone(), batch.reservations.len());
            batch.reservations.push(SimulationReservationRow {
                id: reservation.id.clone(),
                decision_id: decision.decision_id.clone(),
                trace_id,
                amount_usd: reservation.amount_usd,
                estimated_amount_usd: stored.estimated_cost_usd,
                actual_amount_usd: None,
                currency: reservation.currency.clone(),
                status: reservation_status_text(reservation.status),
                created_at: reservation.created_at.to_rfc3339(),
                expires_at: reservation.expires_at.to_rfc3339(),
                finalized_at: None,
                budget_rule_ids_json: serde_json::to_string(&stored.budget_rule_ids)?,
                limit_window_spends_json: serde_json::to_string(&stored.limit_window_spends)?,
                allocation_spends_json: serde_json::to_string(&stored.allocation_spends)?,
            });
            for spend in &stored.limit_window_spends {
                batch
                    .reservation_scopes
                    .push(SimulationReservationScopeRow {
                        reservation_id: reservation.id.clone(),
                        rule_id: spend.rule_id.clone(),
                        limit_id: spend.limit_id.clone(),
                        scope_key: spend.scope_key.clone(),
                        amount_usd: reservation.amount_usd,
                        created_at: reservation.created_at.to_rfc3339(),
                    });
            }
        }

        Ok(())
    }

    pub fn capture_simulation_finalization(
        &self,
        batch: &mut SimulationLedgerBatch,
        reservation: &Reservation,
        payload: &FinalizeReservation,
    ) -> Result<(), NoetError> {
        let now = Utc::now().to_rfc3339();
        let trace_id = batch
            .reservation_index
            .get(&reservation.id)
            .and_then(|index| batch.reservations.get_mut(*index))
            .and_then(|row| {
                row.amount_usd = reservation.amount_usd;
                row.actual_amount_usd = Some(reservation.amount_usd);
                row.status = reservation_status_text(reservation.status);
                row.finalized_at = Some(now.clone());
                row.trace_id.clone()
            })
            .or_else(|| string_value(&payload.metadata, "trace_id"));

        if let Some(usage) = &payload.usage {
            batch
                .usage_observations
                .push(SimulationUsageObservationRow {
                    id: Uuid::new_v4().to_string(),
                    reservation_id: reservation.id.clone(),
                    trace_id,
                    provider: usage.provider.clone(),
                    model: usage.model.clone(),
                    input_tokens: usage.input_tokens.map(|value| value as i64),
                    output_tokens: usage.output_tokens.map(|value| value as i64),
                    total_tokens: usage.total_tokens.map(|value| value as i64),
                    cost_usd: usage.cost_usd.or(Some(reservation.amount_usd)),
                    latency_ms: usage.latency_ms.map(|value| value as i64),
                    stop_reason: usage.stop_reason.clone(),
                    source: "reservation.finalize",
                    metadata_json: serde_json::to_string(&payload.metadata)?,
                    created_at: now,
                });
        }
        Ok(())
    }

    pub fn persist_simulation_batch_to_postgres(
        &self,
        database_url: &str,
        batch: &SimulationLedgerBatch,
    ) -> Result<(), NoetError> {
        self.persist_simulation_batch_to_postgres_with_options(
            database_url,
            batch,
            &AsyncPostgresLedgerOptions::strict(),
        )
    }

    pub fn persist_simulation_batch_to_postgres_with_options(
        &self,
        database_url: &str,
        batch: &SimulationLedgerBatch,
        options: &AsyncPostgresLedgerOptions,
    ) -> Result<(), NoetError> {
        let target = Self::open_postgres(database_url)?;
        let LedgerStore::Postgres(pg_conn) = &target.store else {
            return Ok(());
        };
        let mut pg = pg_conn.0.lock().expect("postgres mutex");
        let mut tx = pg.transaction()?;
        if let Some(value) = options.synchronous_commit.as_deref() {
            let value = normalized_synchronous_commit(value)?;
            tx.batch_execute(&format!("SET LOCAL synchronous_commit TO {value}"))?;
        }

        insert_simulation_decisions_postgres(&mut tx, &batch.decisions)?;
        insert_simulation_reservations_postgres(&mut tx, &batch.reservations)?;
        insert_simulation_reservation_scopes_postgres(&mut tx, &batch.reservation_scopes)?;
        insert_simulation_usage_observations_postgres(&mut tx, &batch.usage_observations)?;
        upsert_simulation_limit_windows_postgres(&mut tx, &self.limit_windows)?;
        upsert_simulation_rolling_spend_buckets_postgres(&mut tx, &self.rolling_spend_buckets)?;
        upsert_simulation_allocation_buckets_postgres(&mut tx, &self.allocation_buckets)?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_event(&mut self, event: TraceEvent) -> Result<(), NoetError> {
        validate_event_payload(&event)?;
        self.persist_event(&event)?;
        self.events_count = self.events_count.saturating_add(1);
        Ok(())
    }

    pub fn event_count(&self) -> usize {
        self.events_count as usize
    }

    fn approval_audit_events_from_ledger(&self) -> Result<Vec<ApprovalAuditEvent>, NoetError> {
        match &self.store {
            LedgerStore::Postgres(pg_conn) => {
                let rows = pg_conn.0.lock().expect("postgres mutex").query(
                    "
                    SELECT id, trace_id, kind, occurred_at, payload_json
                    FROM events
                    WHERE kind IN ('approval.self.approved', 'approval.self.rejected', 'pi.authorize')
                    ORDER BY occurred_at DESC
                    ",
                    &[],
                )?;
                let events = rows
                    .into_iter()
                    .map(trace_event_from_postgres_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(events
                    .into_iter()
                    .filter_map(|event| ApprovalAuditEvent::from_trace_event(&event))
                    .collect())
            }
            LedgerStore::Sqlite(conn) => {
                let mut stmt = conn.prepare(
                    "
                    SELECT id, trace_id, kind, occurred_at, payload_json
                    FROM events
                    WHERE kind IN ('approval.self.approved', 'approval.self.rejected', 'pi.authorize')
                    ORDER BY occurred_at DESC
                    ",
                )?;
                let events = stmt
                    .query_map([], trace_event_from_sqlite_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(events
                    .into_iter()
                    .filter_map(|event| ApprovalAuditEvent::from_trace_event(&event))
                    .collect())
            }
            LedgerStore::InMemory => Ok(Vec::new()),
        }
    }

    pub fn usage_report(&self) -> Result<UsageReport, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let rows = pg_conn.0.lock().expect("postgres mutex").query(
                "
                SELECT d.subject, d.project, COALESCE(u.provider, d.provider), COALESCE(u.model, d.model),
                       COALESCE(SUM(u.input_tokens), 0)::BIGINT, COALESCE(SUM(u.output_tokens), 0)::BIGINT,
                       COALESCE(SUM(CASE
                           WHEN COALESCE((u.metadata_json::jsonb #>> '{usage_details,cache_read_tokens}') ~ '^-?[0-9]+$', false)
                           THEN (u.metadata_json::jsonb #>> '{usage_details,cache_read_tokens}')::BIGINT
                           ELSE 0
                       END), 0)::BIGINT,
                       COALESCE(SUM(CASE
                           WHEN COALESCE((u.metadata_json::jsonb #>> '{usage_details,cache_write_tokens}') ~ '^-?[0-9]+$', false)
                           THEN (u.metadata_json::jsonb #>> '{usage_details,cache_write_tokens}')::BIGINT
                           ELSE 0
                       END), 0)::BIGINT,
                       COALESCE(SUM(u.total_tokens), 0)::BIGINT,
                       COALESCE(SUM(CASE
                           WHEN COALESCE((u.metadata_json::jsonb #>> '{usage_details,cache_read_cost_usd}') ~ '^-?([0-9]+(\\.[0-9]*)?|\\.[0-9]+)([eE][+-]?[0-9]+)?$', false)
                           THEN (u.metadata_json::jsonb #>> '{usage_details,cache_read_cost_usd}')::DOUBLE PRECISION
                           ELSE 0
                       END), 0)::DOUBLE PRECISION,
                       COALESCE(SUM(CASE
                           WHEN COALESCE((u.metadata_json::jsonb #>> '{usage_details,cache_write_cost_usd}') ~ '^-?([0-9]+(\\.[0-9]*)?|\\.[0-9]+)([eE][+-]?[0-9]+)?$', false)
                           THEN (u.metadata_json::jsonb #>> '{usage_details,cache_write_cost_usd}')::DOUBLE PRECISION
                           ELSE 0
                       END), 0)::DOUBLE PRECISION,
                       COALESCE(SUM(r.amount_usd), 0),
                       COUNT(r.id)::BIGINT,
                       COALESCE(SUM(CASE WHEN r.status = 'active' THEN 1 ELSE 0 END), 0)::BIGINT,
                       COALESCE(SUM(CASE WHEN r.status = 'finalized' THEN 1 ELSE 0 END), 0)::BIGINT
                FROM reservations r
                JOIN decisions d ON d.decision_id = r.decision_id
                LEFT JOIN usage_observations u ON u.reservation_id = r.id
                GROUP BY d.subject, d.project, COALESCE(u.provider, d.provider), COALESCE(u.model, d.model)
                ORDER BY COALESCE(SUM(r.amount_usd), 0) DESC
                ",
                &[],
            )?;
            let rows = rows
                .into_iter()
                .map(|row| UsageReportRow {
                    subject: row.get(0),
                    project: row.get(1),
                    provider: row.get(2),
                    model: row.get(3),
                    input_tokens: row.get::<_, i64>(4).max(0) as u64,
                    output_tokens: row.get::<_, i64>(5).max(0) as u64,
                    cache_read_tokens: row.get::<_, i64>(6).max(0) as u64,
                    cache_write_tokens: row.get::<_, i64>(7).max(0) as u64,
                    total_tokens: row.get::<_, i64>(8).max(0) as u64,
                    cache_read_cost_usd: row.get(9),
                    cache_write_cost_usd: row.get(10),
                    total_cost_usd: row.get(11),
                    reservations: row.get::<_, i64>(12).max(0) as u64,
                    active_reservations: row.get::<_, i64>(13).max(0) as u64,
                    finalized_reservations: row.get::<_, i64>(14).max(0) as u64,
                })
                .collect::<Vec<_>>();
            return Ok(UsageReport {
                total_cost_usd: rows.iter().map(|row| row.total_cost_usd).sum(),
                rows,
                protected_adoption: protected_adoption_report_postgres(pg_conn)?,
            });
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(UsageReport {
                total_cost_usd: 0.0,
                rows: Vec::new(),
                protected_adoption: None,
            });
        };
        let mut stmt = conn.prepare(
            "
            SELECT d.subject, d.project, COALESCE(u.provider, d.provider), COALESCE(u.model, d.model),
                   COALESCE(SUM(u.input_tokens), 0), COALESCE(SUM(u.output_tokens), 0),
                   COALESCE(SUM(CAST(json_extract(u.metadata_json, '$.usage_details.cache_read_tokens') AS INTEGER)), 0),
                   COALESCE(SUM(CAST(json_extract(u.metadata_json, '$.usage_details.cache_write_tokens') AS INTEGER)), 0),
                   COALESCE(SUM(u.total_tokens), 0),
                   COALESCE(SUM(CAST(json_extract(u.metadata_json, '$.usage_details.cache_read_cost_usd') AS REAL)), 0),
                   COALESCE(SUM(CAST(json_extract(u.metadata_json, '$.usage_details.cache_write_cost_usd') AS REAL)), 0),
                   COALESCE(SUM(r.amount_usd), 0),
                   COUNT(r.id),
                   COALESCE(SUM(CASE WHEN r.status = 'active' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN r.status = 'finalized' THEN 1 ELSE 0 END), 0)
            FROM reservations r
            JOIN decisions d ON d.decision_id = r.decision_id
            LEFT JOIN usage_observations u ON u.reservation_id = r.id
            GROUP BY d.subject, d.project, COALESCE(u.provider, d.provider), COALESCE(u.model, d.model)
            ORDER BY COALESCE(SUM(r.amount_usd), 0) DESC
            ",
        )?;
        let rows: Vec<UsageReportRow> = stmt
            .query_map([], |row| {
                Ok(UsageReportRow {
                    subject: row.get(0)?,
                    project: row.get(1)?,
                    provider: row.get(2)?,
                    model: row.get(3)?,
                    input_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    output_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                    cache_read_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                    cache_write_tokens: row.get::<_, i64>(7)?.max(0) as u64,
                    total_tokens: row.get::<_, i64>(8)?.max(0) as u64,
                    cache_read_cost_usd: row.get(9)?,
                    cache_write_cost_usd: row.get(10)?,
                    total_cost_usd: row.get(11)?,
                    reservations: row.get::<_, i64>(12)?.max(0) as u64,
                    active_reservations: row.get::<_, i64>(13)?.max(0) as u64,
                    finalized_reservations: row.get::<_, i64>(14)?.max(0) as u64,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(UsageReport {
            total_cost_usd: rows.iter().map(|row| row.total_cost_usd).sum(),
            rows,
            protected_adoption: protected_adoption_report(conn)?,
        })
    }

    pub fn decisions_report(&self) -> Result<Vec<TraceReportItem>, NoetError> {
        self.decisions_report_since(None)
    }

    pub fn decisions_report_for_run_page(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<TraceReportItem>, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let rows = pg_conn.0.lock().expect("postgres mutex").query(
                "
                WITH run_page AS (
                    SELECT app_run_key, MAX(created_at) AS latest_at
                    FROM decisions
                    GROUP BY app_run_key
                    ORDER BY latest_at DESC
                    LIMIT $1 OFFSET $2
                )
                SELECT d.created_at, d.outcome, d.decision_id, d.trace_id, d.request_id, d.provider, d.model,
                       d.action,
                       d.estimated_tokens, d.estimated_cost_usd, d.explanations_json, d.metadata_json, d.entities_json,
                       d.selected_budget_id, d.matched_entity, d.selection_reason, d.rejected_budget_id, d.rejected_budget_reason,
                       d.model_check, d.budget_window_remaining_usd, d.routing_json, d.limit_hits_json,
                       d.metadata_json::jsonb ->> 'agent_run_id'
                FROM decisions d
                JOIN run_page p ON d.app_run_key = p.app_run_key
                ORDER BY p.latest_at DESC, d.created_at DESC
                ",
                &[&(limit as i64), &(offset as i64)],
            )?;
            return Ok(rows
                .iter()
                .map(decision_report_item_from_postgres_row)
                .collect());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "
            WITH run_page AS (
                SELECT app_run_key, MAX(created_at) AS latest_at
                FROM decisions
                GROUP BY app_run_key
                ORDER BY latest_at DESC
                LIMIT ?1 OFFSET ?2
            )
            SELECT d.created_at, d.outcome, d.decision_id, d.trace_id, d.request_id, d.provider, d.model,
                   d.action,
                   d.estimated_tokens, d.estimated_cost_usd, d.explanations_json, d.metadata_json, d.entities_json,
                   d.selected_budget_id, d.matched_entity, d.selection_reason, d.rejected_budget_id, d.rejected_budget_reason,
                   d.model_check, d.budget_window_remaining_usd, d.routing_json, d.limit_hits_json,
                   json_extract(d.metadata_json, '$.agent_run_id')
            FROM decisions d
            JOIN run_page p ON d.app_run_key = p.app_run_key
            ORDER BY p.latest_at DESC, d.created_at DESC
            ",
        )?;
        stmt.query_map(
            params![limit as i64, offset as i64],
            decision_report_item_from_row,
        )?
        .collect::<Result<_, _>>()
        .map_err(NoetError::from)
    }

    pub fn run_totals_report(&self) -> Result<RunTotalsReport, NoetError> {
        self.run_totals_report_since(None)
    }

    pub fn run_totals_report_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<RunTotalsReport, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let mut pg = pg_conn.0.lock().expect("postgres mutex");
            let since = since.map(|since| since.to_rfc3339());
            let mut totals = RunTotalsReport {
                runs: if let Some(since) = since.as_deref() {
                    pg.query_one(
                        "SELECT COUNT(DISTINCT app_run_key)::BIGINT FROM decisions WHERE created_at >= $1",
                        &[&since],
                    )?
                    .get::<_, i64>(0)
                    .max(0) as u64
                } else {
                    pg.query_one(
                        "SELECT COUNT(DISTINCT app_run_key)::BIGINT FROM decisions",
                        &[],
                    )?
                    .get::<_, i64>(0)
                    .max(0) as u64
                },
                ..RunTotalsReport::default()
            };
            let outcome_rows = if let Some(since) = since.as_deref() {
                pg.query(
                    "SELECT outcome, COUNT(*)::BIGINT FROM decisions WHERE created_at >= $1 GROUP BY outcome",
                    &[&since],
                )?
            } else {
                pg.query(
                    "SELECT outcome, COUNT(*)::BIGINT FROM decisions GROUP BY outcome",
                    &[],
                )?
            };
            for row in outcome_rows {
                let outcome: String = row.get(0);
                let count = row.get::<_, i64>(1).max(0) as u64;
                match outcome.as_str() {
                    "allow" => totals.allow += count,
                    "warn" => totals.warn += count,
                    "deny" => totals.deny += count,
                    "ask" => totals.ask += count,
                    _ => {}
                }
            }
            totals.limit_hits = if let Some(since) = since.as_deref() {
                pg.query_one(
                    "
                    SELECT COALESCE(SUM(COALESCE(jsonb_array_length(limit_hits_json::jsonb), 0)), 0)::BIGINT
                    FROM decisions
                    WHERE created_at >= $1
                    ",
                    &[&since],
                )?
                .get::<_, i64>(0)
                .max(0) as u64
            } else {
                pg.query_one(
                    "
                    SELECT COALESCE(SUM(COALESCE(jsonb_array_length(limit_hits_json::jsonb), 0)), 0)::BIGINT
                    FROM decisions
                    ",
                    &[],
                )?
                .get::<_, i64>(0)
                .max(0) as u64
            };
            let row = if let Some(since) = since.as_deref() {
                pg.query_one(
                    "
                    SELECT COALESCE(SUM(total_tokens), 0)::BIGINT, COALESCE(SUM(cost_usd), 0)::DOUBLE PRECISION
                    FROM usage_observations
                    WHERE created_at >= $1
                    ",
                    &[&since],
                )?
            } else {
                pg.query_one(
                    "
                    SELECT COALESCE(SUM(total_tokens), 0)::BIGINT, COALESCE(SUM(cost_usd), 0)::DOUBLE PRECISION
                    FROM usage_observations
                    ",
                    &[],
                )?
            };
            totals.tokens = row.get::<_, i64>(0).max(0) as u64;
            totals.spend_usd = row.get(1);
            return Ok(totals);
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(RunTotalsReport::default());
        };
        let since = since.map(|since| since.to_rfc3339());
        let mut totals = RunTotalsReport {
            runs: if let Some(since) = since.as_deref() {
                conn.query_row(
                    "
                    SELECT COUNT(DISTINCT app_run_key)
                    FROM decisions
                    WHERE created_at >= ?1
                    ",
                    [since],
                    |row| Ok(row.get::<_, i64>(0)?.max(0) as u64),
                )?
            } else {
                conn.query_row(
                    "
                SELECT COUNT(DISTINCT app_run_key)
                FROM decisions
                ",
                    [],
                    |row| Ok(row.get::<_, i64>(0)?.max(0) as u64),
                )?
            },
            ..RunTotalsReport::default()
        };
        let mut outcome_sql = "
                SELECT outcome, COUNT(*)
                FROM decisions"
            .to_owned();
        if since.is_some() {
            outcome_sql.push_str(" WHERE created_at >= ?1");
        }
        outcome_sql.push_str(" GROUP BY outcome");
        let mut stmt = conn.prepare(&outcome_sql)?;
        let rows = if let Some(since) = since.as_deref() {
            stmt.query_map([since], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        for (outcome, count) in rows {
            match outcome.as_str() {
                "allow" => totals.allow += count,
                "warn" => totals.warn += count,
                "deny" => totals.deny += count,
                "ask" => totals.ask += count,
                _ => {}
            }
        }
        totals.limit_hits = if let Some(since) = since.as_deref() {
            conn.query_row(
                "
                SELECT COALESCE(SUM(COALESCE(json_array_length(limit_hits_json), 0)), 0)
                FROM decisions
                WHERE created_at >= ?1
                ",
                [since],
                |row| Ok(row.get::<_, i64>(0)?.max(0) as u64),
            )?
        } else {
            conn.query_row(
                "
            SELECT COALESCE(SUM(COALESCE(json_array_length(limit_hits_json), 0)), 0)
            FROM decisions
            ",
                [],
                |row| Ok(row.get::<_, i64>(0)?.max(0) as u64),
            )?
        };
        let (tokens, spend): (i64, f64) = if let Some(since) = since.as_deref() {
            conn.query_row(
                "
                SELECT COALESCE(SUM(total_tokens), 0), COALESCE(SUM(cost_usd), 0)
                FROM usage_observations
                WHERE created_at >= ?1
                ",
                [since],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
        } else {
            conn.query_row(
                "
            SELECT COALESCE(SUM(total_tokens), 0), COALESCE(SUM(cost_usd), 0)
            FROM usage_observations
            ",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
        };
        totals.tokens = tokens.max(0) as u64;
        totals.spend_usd = spend;
        Ok(totals)
    }

    pub fn rule_stats_report(&self) -> Result<Vec<RuleStatsReport>, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let mut stats = HashMap::<String, RuleStatsReport>::new();
            let mut pg = pg_conn.0.lock().expect("postgres mutex");
            let rows = pg.query(
                "
                SELECT COALESCE(selected_budget_id, explanations_json::jsonb #>> '{0,rule_id}', 'unattributed'),
                       outcome,
                       COUNT(*)::BIGINT,
                       COALESCE(SUM(COALESCE(jsonb_array_length(limit_hits_json::jsonb), 0)), 0)::BIGINT
                FROM decisions
                GROUP BY 1, 2
                ",
                &[],
            )?;
            for row in rows {
                let rule: String = row.get(0);
                let outcome: String = row.get(1);
                let count = row.get::<_, i64>(2).max(0) as u64;
                let limit_hits = row.get::<_, i64>(3).max(0) as u64;
                let stat = stats
                    .entry(rule.clone())
                    .or_insert_with(|| RuleStatsReport {
                        rule,
                        ..RuleStatsReport::default()
                    });
                match outcome.as_str() {
                    "allow" => stat.allow += count,
                    "warn" => stat.warn += count,
                    "deny" => stat.deny += count,
                    "ask" => stat.ask += count,
                    _ => {}
                }
                stat.limit_hits += limit_hits;
            }

            let mut reasons = HashMap::<String, HashMap<String, u64>>::new();
            let mut models = HashMap::<String, HashMap<String, u64>>::new();
            let rows = pg.query(
                "
                SELECT COALESCE(selected_budget_id, explanations_json::jsonb #>> '{0,rule_id}', 'unattributed'),
                       explanations_json,
                       provider,
                       model,
                       limit_hits_json
                FROM decisions
                WHERE outcome = 'deny'
                   OR COALESCE(jsonb_array_length(limit_hits_json::jsonb), 0) > 0
                ",
                &[],
            )?;
            for row in rows {
                let rule: String = row.get(0);
                let explanations_json: String = row.get(1);
                let provider: Option<String> = row.get(2);
                let model: Option<String> = row.get(3);
                let limit_hits_json: Option<String> = row.get(4);
                if let Some(reason) =
                    rule_stat_reason(&explanations_json, limit_hits_json.as_deref())
                {
                    *reasons
                        .entry(rule.clone())
                        .or_default()
                        .entry(reason)
                        .or_default() += 1;
                }
                if let Some(model) = model {
                    let model_ref = provider
                        .map(|provider| format!("{provider}/{model}"))
                        .unwrap_or(model);
                    *models
                        .entry(rule)
                        .or_default()
                        .entry(model_ref)
                        .or_default() += 1;
                }
            }

            let mut stats = stats.into_values().collect::<Vec<_>>();
            for stat in &mut stats {
                stat.top_reason = reasons.get(&stat.rule).and_then(most_common_count);
                stat.top_model = models.get(&stat.rule).and_then(most_common_count);
            }
            stats.sort_by(|left, right| left.rule.cmp(&right.rule));
            return Ok(stats);
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(Vec::new());
        };
        let mut stats = HashMap::<String, RuleStatsReport>::new();
        let mut stmt = conn.prepare(
            "
            SELECT COALESCE(selected_budget_id, json_extract(explanations_json, '$[0].rule_id'), 'unattributed'),
                   outcome,
                   COUNT(*),
                   COALESCE(SUM(COALESCE(json_array_length(limit_hits_json), 0)), 0)
            FROM decisions
            GROUP BY 1, 2
            ",
        )?;
        for row in stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?.max(0) as u64,
                row.get::<_, i64>(3)?.max(0) as u64,
            ))
        })? {
            let (rule, outcome, count, limit_hits) = row?;
            let stat = stats
                .entry(rule.clone())
                .or_insert_with(|| RuleStatsReport {
                    rule,
                    ..RuleStatsReport::default()
                });
            match outcome.as_str() {
                "allow" => stat.allow += count,
                "warn" => stat.warn += count,
                "deny" => stat.deny += count,
                "ask" => stat.ask += count,
                _ => {}
            }
            stat.limit_hits += limit_hits;
        }

        let mut reasons = HashMap::<String, HashMap<String, u64>>::new();
        let mut models = HashMap::<String, HashMap<String, u64>>::new();
        let mut stmt = conn.prepare(
            "
            SELECT COALESCE(selected_budget_id, json_extract(explanations_json, '$[0].rule_id'), 'unattributed'),
                   explanations_json,
                   provider,
                   model,
                   limit_hits_json
            FROM decisions
            WHERE outcome = 'deny'
               OR COALESCE(json_array_length(limit_hits_json), 0) > 0
            ",
        )?;
        for row in stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })? {
            let (rule, explanations_json, provider, model, limit_hits_json) = row?;
            if let Some(reason) = rule_stat_reason(&explanations_json, limit_hits_json.as_deref()) {
                *reasons
                    .entry(rule.clone())
                    .or_default()
                    .entry(reason)
                    .or_default() += 1;
            }
            if let Some(model) = model {
                let model_ref = provider
                    .map(|provider| format!("{provider}/{model}"))
                    .unwrap_or(model);
                *models
                    .entry(rule)
                    .or_default()
                    .entry(model_ref)
                    .or_default() += 1;
            }
        }

        let mut stats = stats.into_values().collect::<Vec<_>>();
        for stat in &mut stats {
            stat.top_reason = reasons.get(&stat.rule).and_then(most_common_count);
            stat.top_model = models.get(&stat.rule).and_then(most_common_count);
        }
        stats.sort_by(|left, right| left.rule.cmp(&right.rule));
        Ok(stats)
    }

    pub fn decisions_report_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<TraceReportItem>, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let mut sql = "
                SELECT created_at, outcome, decision_id, trace_id, request_id, provider, model,
                       action,
                       estimated_tokens, estimated_cost_usd, explanations_json, metadata_json, entities_json,
                       selected_budget_id, matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                       model_check, budget_window_remaining_usd, routing_json, limit_hits_json,
                       metadata_json::jsonb ->> 'agent_run_id'
                FROM decisions
            "
            .to_owned();
            if since.is_some() {
                sql.push_str(" WHERE created_at >= $1");
            }
            sql.push_str(" ORDER BY created_at DESC");
            let rows = if let Some(since) = since {
                pg_conn
                    .0
                    .lock()
                    .expect("postgres mutex")
                    .query(&sql, &[&since.to_rfc3339()])?
            } else {
                pg_conn.0.lock().expect("postgres mutex").query(&sql, &[])?
            };
            return Ok(rows
                .iter()
                .map(decision_report_item_from_postgres_row)
                .collect());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(Vec::new());
        };
        let mut sql = "
            SELECT created_at, outcome, decision_id, trace_id, request_id, provider, model,
                   action,
                   estimated_tokens, estimated_cost_usd, explanations_json, metadata_json, entities_json,
                   selected_budget_id, matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                   model_check, budget_window_remaining_usd, routing_json, limit_hits_json,
                   json_extract(metadata_json, '$.agent_run_id')
            FROM decisions
        "
        .to_owned();
        if since.is_some() {
            sql.push_str(" WHERE created_at >= ?");
        }
        sql.push_str(" ORDER BY created_at DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(since) = since {
            stmt.query_map([since.to_rfc3339()], decision_report_item_from_row)?
                .collect::<Result<_, _>>()
        } else {
            stmt.query_map([], decision_report_item_from_row)?
                .collect::<Result<_, _>>()
        };
        rows.map_err(NoetError::from)
    }

    pub fn historical_authorize_requests(
        &self,
    ) -> Result<Vec<HistoricalAuthorizeRequest>, NoetError> {
        self.historical_authorize_requests_since(None)
    }

    pub fn historical_authorize_requests_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<HistoricalAuthorizeRequest>, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let mut sql = "
                SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                       provider, model, estimated_tokens, estimated_cost_usd, metadata_json
                FROM decisions
            "
            .to_owned();
            if since.is_some() {
                sql.push_str(" WHERE created_at >= $1");
            }
            sql.push_str(" ORDER BY created_at ASC");
            let rows = if let Some(since) = since {
                pg_conn
                    .0
                    .lock()
                    .expect("postgres mutex")
                    .query(&sql, &[&since.to_rfc3339()])?
            } else {
                pg_conn.0.lock().expect("postgres mutex").query(&sql, &[])?
            };
            return Ok(rows
                .iter()
                .map(historical_authorize_request_from_postgres_row)
                .collect());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(Vec::new());
        };
        let mut sql = "
            SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                   provider, model, estimated_tokens, estimated_cost_usd, metadata_json
            FROM decisions
        "
        .to_owned();
        if since.is_some() {
            sql.push_str(" WHERE created_at >= ?");
        }
        sql.push_str(" ORDER BY created_at ASC");
        let mut stmt = conn.prepare(&sql)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            let entities_json: String = row.get(4)?;
            let metadata_json: String = row.get(11)?;
            Ok(HistoricalAuthorizeRequest {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                decision_id: row.get(1)?,
                baseline_outcome: parse_decision_outcome(row.get::<_, String>(2)?.as_str()),
                request: AuthorizeRequest {
                    budget_id: serde_json::from_str::<Value>(&metadata_json).ok().and_then(
                        |value| {
                            value
                                .get("budget_id")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        },
                    ),
                    entities: parse_entities_json(entities_json),
                    subject: row.get(5)?,
                    project: row.get(6)?,
                    provider: row.get(7)?,
                    model: row.get(8)?,
                    estimated_tokens: row
                        .get::<_, Option<i64>>(9)?
                        .map(|value| value.max(0) as u64),
                    estimated_cost_usd: row.get(10)?,
                    metadata: serde_json::from_str(&metadata_json).unwrap_or_default(),
                },
            })
        };
        let rows = if let Some(since) = since {
            stmt.query_map([since.to_rfc3339()], map_row)?
                .collect::<Result<_, _>>()
        } else {
            stmt.query_map([], map_row)?.collect::<Result<_, _>>()
        };
        rows.map_err(NoetError::from)
    }

    pub fn historical_authorize_request_count_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<usize, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let count: i64 = if let Some(since) = since {
                pg_conn.0.lock().expect("postgres mutex").query_one(
                    "SELECT COUNT(*)::BIGINT FROM decisions WHERE created_at >= $1",
                    &[&since.to_rfc3339()],
                )?
            } else {
                pg_conn
                    .0
                    .lock()
                    .expect("postgres mutex")
                    .query_one("SELECT COUNT(*)::BIGINT FROM decisions", &[])?
            }
            .get(0);
            return Ok(count.max(0) as usize);
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(0);
        };
        let count = if let Some(since) = since {
            conn.query_row(
                "SELECT COUNT(*) FROM decisions WHERE created_at >= ?1",
                [since.to_rfc3339()],
                |row| row.get::<_, i64>(0),
            )?
        } else {
            conn.query_row("SELECT COUNT(*) FROM decisions", [], |row| {
                row.get::<_, i64>(0)
            })?
        };
        Ok(count.max(0) as usize)
    }

    pub fn latest_historical_authorize_requests_since(
        &self,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<HistoricalAuthorizeRequest>, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            if limit == 0 {
                return Ok(Vec::new());
            }
            let mut sql = "
                SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                       provider, model, estimated_tokens, estimated_cost_usd, metadata_json
                FROM (
                    SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                           provider, model, estimated_tokens, estimated_cost_usd
                    FROM decisions
            "
            .to_owned();
            if since.is_some() {
                sql.push_str(" WHERE created_at >= $1");
            }
            let limit_param = if since.is_some() { "$2" } else { "$1" };
            sql.push_str(&format!(
                " ORDER BY created_at DESC LIMIT {limit_param}) recent ORDER BY created_at ASC"
            ));
            let rows = if let Some(since) = since {
                pg_conn
                    .0
                    .lock()
                    .expect("postgres mutex")
                    .query(&sql, &[&since.to_rfc3339(), &(limit as i64)])?
            } else {
                pg_conn
                    .0
                    .lock()
                    .expect("postgres mutex")
                    .query(&sql, &[&(limit as i64)])?
            };
            return Ok(rows
                .iter()
                .map(historical_authorize_request_from_postgres_row)
                .collect());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(Vec::new());
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut sql = "
            SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                   provider, model, estimated_tokens, estimated_cost_usd, metadata_json
            FROM (
                SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                       provider, model, estimated_tokens, estimated_cost_usd
                FROM decisions
        "
        .to_owned();
        if since.is_some() {
            sql.push_str(" WHERE created_at >= ?1");
        }
        let limit_param = if since.is_some() { "?2" } else { "?1" };
        sql.push_str(&format!(
            " ORDER BY created_at DESC LIMIT {limit_param}) ORDER BY created_at ASC"
        ));
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(since) = since {
            stmt.query_map(params![since.to_rfc3339(), limit as i64], |row| {
                historical_authorize_request_from_row(row)
            })?
            .collect::<Result<_, _>>()
        } else {
            stmt.query_map([limit as i64], |row| {
                historical_authorize_request_from_row(row)
            })?
            .collect::<Result<_, _>>()
        };
        rows.map_err(NoetError::from)
    }

    pub fn spend_scope_totals(
        &self,
        rule_id: &str,
        limit_id: &str,
        since: DateTime<Utc>,
        before: DateTime<Utc>,
    ) -> Result<Vec<SpendScopeTotal>, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let rows = pg_conn.0.lock().expect("postgres mutex").query(
                "
                SELECT scope_key, COALESCE(SUM(amount_usd), 0)::DOUBLE PRECISION
                FROM reservation_limit_scopes
                WHERE rule_id = $1
                  AND limit_id = $2
                  AND created_at >= $3
                  AND created_at < $4
                GROUP BY scope_key
                HAVING COALESCE(SUM(amount_usd), 0) > 0
                ",
                &[
                    &rule_id,
                    &limit_id,
                    &since.to_rfc3339(),
                    &before.to_rfc3339(),
                ],
            )?;
            return Ok(rows
                .into_iter()
                .map(|row| SpendScopeTotal {
                    scope_key: row.get(0),
                    amount_usd: row.get(1),
                })
                .collect());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "
            SELECT scope_key, COALESCE(SUM(amount_usd), 0)
            FROM reservation_limit_scopes
            WHERE rule_id = ?1
              AND limit_id = ?2
              AND created_at >= ?3
              AND created_at < ?4
            GROUP BY scope_key
            HAVING COALESCE(SUM(amount_usd), 0) > 0
            ",
        )?;
        stmt.query_map(
            params![rule_id, limit_id, since.to_rfc3339(), before.to_rfc3339()],
            |row| {
                Ok(SpendScopeTotal {
                    scope_key: row.get(0)?,
                    amount_usd: row.get(1)?,
                })
            },
        )?
        .collect::<Result<_, _>>()
        .map_err(NoetError::from)
    }

    pub fn usage_activity_report(&self) -> Result<Vec<UsageActivityRecord>, NoetError> {
        self.usage_activity_report_since(None)
    }

    pub fn usage_activity_report_for_agent_runs(
        &self,
        agent_run_ids: &[String],
    ) -> Result<Vec<UsageActivityRecord>, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            if agent_run_ids.is_empty() {
                return Ok(Vec::new());
            }
            let placeholders = (1..=agent_run_ids.len())
                .map(|index| format!("${index}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "
                SELECT u.created_at, COALESCE(u.trace_id, d.trace_id), d.subject, d.project,
                       COALESCE(u.provider, d.provider), COALESCE(u.model, d.model),
                       d.selected_budget_id, d.matched_entity, d.entities_json,
                       COALESCE(u.input_tokens, 0)::BIGINT, COALESCE(u.output_tokens, 0)::BIGINT,
                       CASE
                           WHEN COALESCE((u.metadata_json::jsonb #>> '{{usage_details,cache_read_tokens}}') ~ '^-?[0-9]+$', false)
                           THEN (u.metadata_json::jsonb #>> '{{usage_details,cache_read_tokens}}')::BIGINT
                           ELSE 0
                       END::BIGINT,
                       CASE
                           WHEN COALESCE((u.metadata_json::jsonb #>> '{{usage_details,cache_write_tokens}}') ~ '^-?[0-9]+$', false)
                           THEN (u.metadata_json::jsonb #>> '{{usage_details,cache_write_tokens}}')::BIGINT
                           ELSE 0
                       END::BIGINT,
                       COALESCE(u.total_tokens, 0)::BIGINT,
                       COALESCE(u.cost_usd, r.actual_amount_usd, r.amount_usd, 0)::DOUBLE PRECISION,
                       COALESCE(u.metadata_json::jsonb ->> 'agent_run_id', d.metadata_json::jsonb ->> 'agent_run_id'),
                       COALESCE(u.metadata_json::jsonb ->> 'request_id', d.request_id)
                FROM usage_observations u
                LEFT JOIN reservations r ON r.id = u.reservation_id
                LEFT JOIN decisions d ON d.decision_id = r.decision_id
                WHERE COALESCE(u.metadata_json::jsonb ->> 'agent_run_id', d.metadata_json::jsonb ->> 'agent_run_id') IN ({placeholders})
                ORDER BY u.created_at DESC
                "
            );
            let params = agent_run_ids
                .iter()
                .map(|id| id as &(dyn PostgresToSql + Sync))
                .collect::<Vec<_>>();
            let rows = pg_conn
                .0
                .lock()
                .expect("postgres mutex")
                .query(&sql, &params)?;
            return Ok(rows
                .iter()
                .map(usage_activity_record_from_postgres_row)
                .collect());
        }
        if !matches!(&self.store, LedgerStore::Sqlite(_)) {
            return Ok(Vec::new());
        }
        if agent_run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", agent_run_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "
            SELECT u.created_at, COALESCE(u.trace_id, d.trace_id), d.subject, d.project,
                   COALESCE(u.provider, d.provider), COALESCE(u.model, d.model),
                   d.selected_budget_id, d.matched_entity, d.entities_json,
                   COALESCE(u.input_tokens, 0), COALESCE(u.output_tokens, 0),
                   COALESCE(CAST(json_extract(u.metadata_json, '$.usage_details.cache_read_tokens') AS INTEGER), 0),
                   COALESCE(CAST(json_extract(u.metadata_json, '$.usage_details.cache_write_tokens') AS INTEGER), 0),
                   COALESCE(u.total_tokens, 0),
                   COALESCE(u.cost_usd, r.actual_amount_usd, r.amount_usd, 0),
                   COALESCE(json_extract(u.metadata_json, '$.agent_run_id'), json_extract(d.metadata_json, '$.agent_run_id')),
                   COALESCE(json_extract(u.metadata_json, '$.request_id'), d.request_id)
            FROM usage_observations u
            LEFT JOIN reservations r ON r.id = u.reservation_id
            LEFT JOIN decisions d ON d.decision_id = r.decision_id
            WHERE COALESCE(json_extract(u.metadata_json, '$.agent_run_id'), json_extract(d.metadata_json, '$.agent_run_id')) IN ({placeholders})
            ORDER BY u.created_at DESC
            "
        );
        let params = agent_run_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect::<Vec<_>>();
        self.query_usage_activity_rows(&sql, params.as_slice())
    }

    pub fn usage_activity_report_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<UsageActivityRecord>, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let mut sql = "
                SELECT u.created_at, COALESCE(u.trace_id, d.trace_id), d.subject, d.project,
                       COALESCE(u.provider, d.provider), COALESCE(u.model, d.model),
                       d.selected_budget_id, d.matched_entity, d.entities_json,
                       COALESCE(u.input_tokens, 0)::BIGINT, COALESCE(u.output_tokens, 0)::BIGINT,
                       CASE
                           WHEN COALESCE((u.metadata_json::jsonb #>> '{usage_details,cache_read_tokens}') ~ '^-?[0-9]+$', false)
                           THEN (u.metadata_json::jsonb #>> '{usage_details,cache_read_tokens}')::BIGINT
                           ELSE 0
                       END::BIGINT,
                       CASE
                           WHEN COALESCE((u.metadata_json::jsonb #>> '{usage_details,cache_write_tokens}') ~ '^-?[0-9]+$', false)
                           THEN (u.metadata_json::jsonb #>> '{usage_details,cache_write_tokens}')::BIGINT
                           ELSE 0
                       END::BIGINT,
                       COALESCE(u.total_tokens, 0)::BIGINT,
                       COALESCE(u.cost_usd, r.actual_amount_usd, r.amount_usd, 0)::DOUBLE PRECISION,
                       COALESCE(u.metadata_json::jsonb ->> 'agent_run_id', d.metadata_json::jsonb ->> 'agent_run_id'),
                       COALESCE(u.metadata_json::jsonb ->> 'request_id', d.request_id)
                FROM usage_observations u
                LEFT JOIN reservations r ON r.id = u.reservation_id
                LEFT JOIN decisions d ON d.decision_id = r.decision_id
            "
            .to_owned();
            if since.is_some() {
                sql.push_str(" WHERE u.created_at >= $1");
            }
            sql.push_str(" ORDER BY u.created_at DESC");
            let rows = if let Some(since) = since {
                pg_conn
                    .0
                    .lock()
                    .expect("postgres mutex")
                    .query(&sql, &[&since.to_rfc3339()])?
            } else {
                pg_conn.0.lock().expect("postgres mutex").query(&sql, &[])?
            };
            return Ok(rows
                .iter()
                .map(usage_activity_record_from_postgres_row)
                .collect());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(Vec::new());
        };
        let mut sql = "
            SELECT u.created_at, COALESCE(u.trace_id, d.trace_id), d.subject, d.project,
                   COALESCE(u.provider, d.provider), COALESCE(u.model, d.model),
                   d.selected_budget_id, d.matched_entity, d.entities_json,
                   COALESCE(u.input_tokens, 0), COALESCE(u.output_tokens, 0),
                   COALESCE(CAST(json_extract(u.metadata_json, '$.usage_details.cache_read_tokens') AS INTEGER), 0),
                   COALESCE(CAST(json_extract(u.metadata_json, '$.usage_details.cache_write_tokens') AS INTEGER), 0),
                   COALESCE(u.total_tokens, 0),
                   COALESCE(u.cost_usd, r.actual_amount_usd, r.amount_usd, 0),
                   COALESCE(json_extract(u.metadata_json, '$.agent_run_id'), json_extract(d.metadata_json, '$.agent_run_id')),
                   COALESCE(json_extract(u.metadata_json, '$.request_id'), d.request_id)
            FROM usage_observations u
            LEFT JOIN reservations r ON r.id = u.reservation_id
            LEFT JOIN decisions d ON d.decision_id = r.decision_id
        "
        .to_owned();
        if since.is_some() {
            sql.push_str(" WHERE u.created_at >= ?");
        }
        sql.push_str(" ORDER BY u.created_at DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(since) = since {
            stmt.query_map([since.to_rfc3339()], usage_activity_record_from_row)?
                .collect::<Result<_, _>>()
        } else {
            stmt.query_map([], usage_activity_record_from_row)?
                .collect::<Result<_, _>>()
        };
        rows.map_err(NoetError::from)
    }

    fn query_usage_activity_rows(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<UsageActivityRecord>, NoetError> {
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(sql)?;
        stmt.query_map(params, usage_activity_record_from_row)?
            .collect::<Result<_, _>>()
            .map_err(NoetError::from)
    }

    pub fn observations_report(
        &self,
        kind_prefix: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<Vec<TraceReportItem>, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let mut sql = "SELECT occurred_at, kind, payload_json, trace_id FROM events".to_owned();
            let mut clauses = Vec::new();
            if kind_prefix.is_some() {
                clauses.push("kind LIKE $1");
            }
            if trace_id.is_some() {
                clauses.push(if kind_prefix.is_some() {
                    "trace_id = $2"
                } else {
                    "trace_id = $1"
                });
            }
            if !clauses.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&clauses.join(" AND "));
            }
            sql.push_str(" ORDER BY occurred_at DESC");
            let prefix = kind_prefix.map(|prefix| format!("{prefix}%"));
            let rows = match (prefix.as_ref(), trace_id) {
                (Some(prefix), Some(trace_id)) => pg_conn
                    .0
                    .lock()
                    .expect("postgres mutex")
                    .query(&sql, &[prefix, &trace_id])?,
                (Some(prefix), None) => pg_conn
                    .0
                    .lock()
                    .expect("postgres mutex")
                    .query(&sql, &[prefix])?,
                (None, Some(trace_id)) => pg_conn
                    .0
                    .lock()
                    .expect("postgres mutex")
                    .query(&sql, &[&trace_id])?,
                (None, None) => pg_conn.0.lock().expect("postgres mutex").query(&sql, &[])?,
            };
            return Ok(rows
                .into_iter()
                .map(event_report_item_from_postgres_row)
                .collect());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(Vec::new());
        };
        let mut sql = "SELECT occurred_at, kind, payload_json, trace_id FROM events".to_owned();
        let mut clauses = Vec::new();
        if kind_prefix.is_some() {
            clauses.push("kind LIKE ?");
        }
        if trace_id.is_some() {
            clauses.push("trace_id = ?");
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY occurred_at DESC");
        let mut stmt = conn.prepare(&sql)?;
        let prefix = kind_prefix.map(|prefix| format!("{prefix}%"));
        let mapper = |row: &rusqlite::Row<'_>| {
            let kind: String = row.get(1)?;
            let payload_json: String = row.get(2)?;
            Ok(TraceReportItem {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                summary: summarize_event_payload(&kind, &payload_json),
                kind,
                trace_id: row.get(3)?,
                agent_run_id: None,
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            })
        };
        match (prefix, trace_id) {
            (Some(prefix), Some(trace_id)) => stmt
                .query_map(params![prefix, trace_id], mapper)?
                .collect::<Result<_, _>>()
                .map_err(NoetError::from),
            (Some(prefix), None) => stmt
                .query_map(params![prefix], mapper)?
                .collect::<Result<_, _>>()
                .map_err(NoetError::from),
            (None, Some(trace_id)) => stmt
                .query_map(params![trace_id], mapper)?
                .collect::<Result<_, _>>()
                .map_err(NoetError::from),
            (None, None) => stmt
                .query_map([], mapper)?
                .collect::<Result<_, _>>()
                .map_err(NoetError::from),
        }
    }

    pub fn trace_report(&self, trace_id: &str) -> Result<TraceReport, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let mut pg = pg_conn.0.lock().expect("postgres mutex");
            let mut items = Vec::new();
            let decisions = pg.query(
                "
                SELECT created_at, outcome, decision_id, trace_id, request_id, provider, model,
                       action,
                       estimated_tokens, estimated_cost_usd, explanations_json, metadata_json, entities_json,
                       selected_budget_id, matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                       model_check, budget_window_remaining_usd, routing_json, limit_hits_json,
                       metadata_json::jsonb ->> 'agent_run_id'
                FROM decisions
                WHERE trace_id = $1
                ORDER BY created_at
                ",
                &[&trace_id],
            )?;
            items.extend(decisions.iter().map(decision_report_item_from_postgres_row));

            let usage = pg.query(
                "
                SELECT created_at, provider, model, input_tokens, output_tokens, total_tokens, cost_usd,
                       stop_reason, metadata_json
                FROM usage_observations
                WHERE trace_id = $1
                ORDER BY created_at
                ",
                &[&trace_id],
            )?;
            for row in usage {
                let provider: Option<String> = row.get(1);
                let model: Option<String> = row.get(2);
                let input_tokens: Option<i64> = row.get(3);
                let output_tokens: Option<i64> = row.get(4);
                let tokens: Option<i64> = row.get(5);
                let cost: Option<f64> = row.get(6);
                let stop_reason: Option<String> = row.get(7);
                let metadata_json: String = row.get(8);
                items.push(TraceReportItem {
                    occurred_at: parse_time(row.get::<_, String>(0)),
                    kind: "usage.finalized".to_owned(),
                    summary: summarize_finalized_usage(FinalizedUsageSummary {
                        provider: provider.as_deref(),
                        model: model.as_deref(),
                        input_tokens,
                        output_tokens,
                        total_tokens: tokens,
                        cost,
                        stop_reason: stop_reason.as_deref(),
                        metadata_json: &metadata_json,
                    }),
                    trace_id: Some(trace_id.to_owned()),
                    agent_run_id: agent_run_id_from_metadata_json(&metadata_json),
                    entities: Vec::new(),
                    routing: None,
                    limit_hits: None,
                    binding_limit: None,
                });
            }

            let events = pg.query(
                "
                SELECT occurred_at, kind, payload_json
                FROM events
                WHERE trace_id = $1
                ORDER BY occurred_at
                ",
                &[&trace_id],
            )?;
            for row in events {
                let kind: String = row.get(1);
                let payload_json: String = row.get(2);
                items.push(TraceReportItem {
                    occurred_at: parse_time(row.get::<_, String>(0)),
                    summary: summarize_event_payload(&kind, &payload_json),
                    kind,
                    trace_id: Some(trace_id.to_owned()),
                    agent_run_id: agent_run_id_from_metadata_json(&payload_json),
                    entities: Vec::new(),
                    routing: None,
                    limit_hits: None,
                    binding_limit: None,
                });
            }
            drop(pg);

            if let Some(limit_items) = self.lifecycle_limit_report_items(trace_id)? {
                items.extend(limit_items);
            }

            items.sort_by_key(|item| item.occurred_at);
            return Ok(TraceReport {
                trace_id: trace_id.to_owned(),
                items,
            });
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(TraceReport {
                trace_id: trace_id.to_owned(),
                items: Vec::new(),
            });
        };
        let mut items = Vec::new();

        let mut decisions = conn.prepare(
            "
            SELECT created_at, outcome, decision_id, trace_id, request_id, provider, model,
                   action,
                   estimated_tokens, estimated_cost_usd, explanations_json, metadata_json, entities_json,
                   selected_budget_id, matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                   model_check, budget_window_remaining_usd, routing_json, limit_hits_json,
                   json_extract(metadata_json, '$.agent_run_id')
            FROM decisions
            WHERE trace_id = ?1
            ORDER BY created_at
            ",
        )?;
        for row in decisions.query_map([trace_id], decision_report_item_from_row)? {
            items.push(row?);
        }

        let mut usage = conn.prepare(
            "
            SELECT created_at, provider, model, input_tokens, output_tokens, total_tokens, cost_usd,
                   stop_reason, metadata_json
            FROM usage_observations
            WHERE trace_id = ?1
            ORDER BY created_at
            ",
        )?;
        for row in usage.query_map([trace_id], |row| {
            let provider: Option<String> = row.get(1)?;
            let model: Option<String> = row.get(2)?;
            let input_tokens: Option<i64> = row.get(3)?;
            let output_tokens: Option<i64> = row.get(4)?;
            let tokens: Option<i64> = row.get(5)?;
            let cost: Option<f64> = row.get(6)?;
            let stop_reason: Option<String> = row.get(7)?;
            let metadata_json: String = row.get(8)?;
            Ok(TraceReportItem {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                kind: "usage.finalized".to_owned(),
                summary: summarize_finalized_usage(FinalizedUsageSummary {
                    provider: provider.as_deref(),
                    model: model.as_deref(),
                    input_tokens,
                    output_tokens,
                    total_tokens: tokens,
                    cost,
                    stop_reason: stop_reason.as_deref(),
                    metadata_json: &metadata_json,
                }),
                trace_id: Some(trace_id.to_owned()),
                agent_run_id: agent_run_id_from_metadata_json(&metadata_json),
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            })
        })? {
            items.push(row?);
        }

        let mut events = conn.prepare(
            "
            SELECT occurred_at, kind, payload_json
            FROM events
            WHERE trace_id = ?1
            ORDER BY occurred_at
            ",
        )?;
        for row in events.query_map([trace_id], |row| {
            let kind: String = row.get(1)?;
            let payload_json: String = row.get(2)?;
            Ok(TraceReportItem {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                summary: summarize_event_payload(&kind, &payload_json),
                kind,
                trace_id: Some(trace_id.to_owned()),
                agent_run_id: agent_run_id_from_metadata_json(&payload_json),
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            })
        })? {
            items.push(row?);
        }

        if let Some(limit_items) = self.lifecycle_limit_report_items(trace_id)? {
            items.extend(limit_items);
        }

        items.sort_by_key(|item| item.occurred_at);
        Ok(TraceReport {
            trace_id: trace_id.to_owned(),
            items,
        })
    }

    fn evaluate_budget_rules(
        &mut self,
        policy: &PolicyFile,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
        outputs: &mut BudgetEvaluationOutputs<'_>,
    ) -> Option<String> {
        let estimated_cost = request.estimated_cost();
        let candidate = self.select_budget_rule(policy, request, now, outputs.explanations);

        let Some(candidate) = candidate else {
            let exhausted_rules = self.exhausted_budget_rules(policy, request, now);
            if !exhausted_rules.is_empty() {
                *outputs.action = merge_policy_action(*outputs.action, PolicyAction::Block);
                for hit in exhausted_rules {
                    outputs.explanations.push(DecisionExplanation {
                        rule_id: hit.rule_id.clone(),
                        reason: hit.reason.clone(),
                        severity: hit.severity,
                    });
                    outputs
                        .message_hints
                        .push(message_hint_from_limit_hit("spend_limit", &hit));
                    outputs.limit_hits.push(hit);
                }
                return None;
            }
            let scoped_rules: Vec<&BudgetRule> = policy
                .budgets
                .iter()
                .filter(|rule| budget_scope_matches(rule, request))
                .collect();
            if scoped_rules
                .iter()
                .any(|rule| !budget_model_allowed(rule, request))
            {
                *outputs.action = merge_policy_action(*outputs.action, PolicyAction::Block);
                for rule in scoped_rules
                    .into_iter()
                    .filter(|rule| !budget_model_allowed(rule, request))
                {
                    outputs.explanations.push(DecisionExplanation {
                        rule_id: rule.id.clone(),
                        reason: "requested provider/model is not allowed by budget".to_owned(),
                        severity: DecisionSeverity::Deny,
                    });
                }
                return None;
            }
            if outputs
                .explanations
                .iter()
                .any(|explanation| explanation.rule_id == "no_fallback_budget")
            {
                *outputs.action = merge_policy_action(*outputs.action, PolicyAction::Block);
                return None;
            }
            outputs.explanations.push(DecisionExplanation {
                rule_id: "no_budget_match".to_owned(),
                reason: "no matching budget rule; request allowed".to_owned(),
                severity: DecisionSeverity::Info,
            });
            return None;
        };

        let rule = policy.budgets.iter().find(|rule| rule.id == candidate.id)?;
        if apply_budget_limits(BudgetLimitEvaluation {
            ledger: self,
            rule,
            request,
            estimated_cost,
            now,
            action: outputs.action,
            explanations: outputs.explanations,
            limit_hits: outputs.limit_hits,
            message_hints: outputs.message_hints,
        }) {
            return Some(rule.id.clone());
        }
        Some(rule.id.clone())
    }

    fn select_budget_rule(
        &mut self,
        policy: &PolicyFile,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
        explanations: &mut Vec<DecisionExplanation>,
    ) -> Option<BudgetCandidate> {
        if let Some(requested_budget_id) = request.budget_id.as_deref() {
            if let Some(rule) = policy
                .budgets
                .iter()
                .find(|rule| rule.id == requested_budget_id)
            {
                if let Some(candidate) = self.valid_budget_candidate(policy, rule, request, now) {
                    explanations.push(DecisionExplanation {
                        rule_id: rule.id.clone(),
                        reason: "selected requested budget".to_owned(),
                        severity: DecisionSeverity::Info,
                    });
                    return Some(candidate);
                }
                explanations.push(DecisionExplanation {
                    rule_id: rule.id.clone(),
                    reason: self.budget_rejection_reason(policy, rule, request, now),
                    severity: DecisionSeverity::Info,
                });
            } else {
                explanations.push(DecisionExplanation {
                    rule_id: requested_budget_id.to_owned(),
                    reason: "requested budget does not exist".to_owned(),
                    severity: DecisionSeverity::Info,
                });
            }
        }

        let mut candidates: Vec<BudgetCandidate> = policy
            .budgets
            .iter()
            .filter_map(|rule| self.valid_budget_candidate(policy, rule, request, now))
            .collect();
        candidates.sort_by(|left, right| {
            left.specificity_rank
                .cmp(&right.specificity_rank)
                .then_with(|| right.priority.cmp(&left.priority))
                .then_with(|| left.pressure_micros.cmp(&right.pressure_micros))
                .then_with(|| left.id.cmp(&right.id))
        });
        let candidate = candidates.into_iter().next();
        if let Some(candidate) = &candidate {
            explanations.push(DecisionExplanation {
                rule_id: candidate.id.clone(),
                reason: match candidate.matched_entity.as_deref() {
                    Some(entity) => format!("selected fallback budget for {entity}"),
                    None => "selected fallback budget".to_owned(),
                },
                severity: DecisionSeverity::Info,
            });
        } else if request.budget_id.is_some() {
            explanations.push(DecisionExplanation {
                rule_id: "no_fallback_budget".to_owned(),
                reason: "no fallback budget can satisfy the request".to_owned(),
                severity: DecisionSeverity::Deny,
            });
        }
        candidate
    }

    fn valid_budget_candidate(
        &mut self,
        policy: &PolicyFile,
        rule: &BudgetRule,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> Option<BudgetCandidate> {
        if !budget_rule_matches(rule, request) {
            return None;
        }
        let estimated_cost = request.estimated_cost();
        if allocation_bucket_available_usd(self, rule, request, now)
            .is_some_and(|available_usd| estimated_cost > available_usd)
        {
            return None;
        }
        let (matched_entity, specificity_rank) =
            matched_entity_and_rank(rule, request, &specificity_order(policy));
        let projections =
            spend_window_projections(self, rule, request, estimated_cost, now).ok()?;
        Some(BudgetCandidate {
            id: rule.id.clone(),
            matched_entity,
            specificity_rank,
            priority: rule.priority,
            pressure_micros: projections
                .iter()
                .map(|projection| {
                    ((projection.projected_spend_usd / projection.max_usd) * 1_000_000.0).round()
                        as u64
                })
                .max()
                .unwrap_or(0),
        })
    }

    fn budget_rejection_reason(
        &mut self,
        _policy: &PolicyFile,
        rule: &BudgetRule,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> String {
        if !budget_scope_matches(rule, request) {
            return "requested budget does not match the request".to_owned();
        }
        if !budget_model_allowed(rule, request) {
            return "requested provider/model is not allowed by requested budget".to_owned();
        }
        if let Some(available_usd) = allocation_bucket_available_usd(self, rule, request, now)
            && request.estimated_cost() > available_usd
        {
            return allocation_limit_hit(rule, request, request.estimated_cost(), available_usd)
                .reason;
        }
        if let Some(hit) =
            spend_window_projections(self, rule, request, request.estimated_cost(), now)
                .ok()
                .into_iter()
                .flatten()
                .filter(|projection| {
                    projection.projected_spend_usd > projection.max_usd
                        && matches!(projection.action, PolicyAction::Ask | PolicyAction::Block)
                })
                .max_by_key(|projection| {
                    ((projection.projected_spend_usd / projection.max_usd) * 1_000_000.0).round()
                        as u64
                })
                .map(|projection| spend_limit_hit(&projection))
        {
            return hit.reason;
        }
        if let Err(reason) =
            spend_window_projections(self, rule, request, request.estimated_cost(), now)
        {
            return reason;
        }
        "requested budget is not valid for the request".to_owned()
    }

    fn exhausted_budget_rules(
        &mut self,
        policy: &PolicyFile,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> Vec<DecisionLimitHitReport> {
        let estimated_cost = request.estimated_cost();
        let mut hits = Vec::new();
        for rule in policy
            .budgets
            .iter()
            .filter(|rule| budget_rule_matches(rule, request))
        {
            if let Some(available_usd) = allocation_bucket_available_usd(self, rule, request, now)
                && estimated_cost > available_usd
            {
                hits.push(allocation_limit_hit(
                    rule,
                    request,
                    estimated_cost,
                    available_usd,
                ));
            }
            hits.extend(
                spend_window_projections(self, rule, request, estimated_cost, now)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|projection| {
                        projection.projected_spend_usd > projection.max_usd
                            && matches!(projection.action, PolicyAction::Ask | PolicyAction::Block)
                    })
                    .map(|projection| spend_limit_hit(&projection)),
            );
        }
        hits
    }

    fn create_reservation(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
        selected_budget_id: Option<&str>,
    ) -> Reservation {
        let amount_usd = request.estimated_cost();
        let matching_rules: Vec<&BudgetRule> = policy
            .map(|policy| {
                policy
                    .budgets
                    .iter()
                    .filter(|rule| {
                        selected_budget_id
                            .map(|selected_budget_id| rule.id == selected_budget_id)
                            .unwrap_or_else(|| budget_rule_matches(rule, request))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let budget_rule_ids: Vec<String> =
            matching_rules.iter().map(|rule| rule.id.clone()).collect();
        let mut allocation_spends = Vec::new();
        let mut limit_window_spends = Vec::new();
        let expires_at = matching_rules
            .iter()
            .flat_map(|rule| {
                spend_window_projections(self, rule, request, amount_usd, now)
                    .expect("selected budget has valid spend window scopes")
                    .into_iter()
                    .map(|projection| {
                        projection
                            .window_ends_at
                            .unwrap_or(now + projection.window_seconds)
                    })
            })
            .min()
            .unwrap_or_else(|| now + Duration::hours(1));

        for rule in matching_rules {
            for limit in &rule.limits.spend {
                let limit_id = spend_limit_identifier(limit).to_owned();
                let scope_key = spend_limit_scope_key(limit.by, request)
                    .expect("selected budget has valid spend window scopes");
                if matches!(limit.mode, Some(SpendWindowMode::Tumbling)) {
                    let Some(window) = crate::policy::parse_limit_window(&limit.window) else {
                        continue;
                    };
                    self.limit_window(rule, &limit_id, window, &scope_key, now)
                        .used_usd += amount_usd;
                } else if matches!(limit.mode, Some(SpendWindowMode::Rolling)) {
                    let bucket_key = (
                        rule.id.clone(),
                        limit_id.clone(),
                        scope_key.clone(),
                        rolling_bucket_start(now),
                    );
                    *self.rolling_spend_buckets.entry(bucket_key).or_insert(0.0) += amount_usd;
                }
                limit_window_spends.push(LimitWindowReservationSpend {
                    rule_id: rule.id.clone(),
                    limit_id,
                    scope_key,
                });
            }
            if let Some(spend) = consume_allocation_bucket(self, rule, request, amount_usd, now) {
                allocation_spends.push(spend);
            }
        }

        let reservation = Reservation {
            id: Uuid::new_v4().to_string(),
            amount_usd,
            currency: "USD".to_owned(),
            status: ReservationStatus::Active,
            created_at: now,
            expires_at,
        };
        self.reservations.insert(
            reservation.id.clone(),
            StoredReservation {
                reservation: reservation.clone(),
                estimated_cost_usd: amount_usd,
                budget_rule_ids,
                limit_window_spends,
                allocation_spends,
            },
        );
        reservation
    }

    fn create_replay_reservation(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
        selected_budget_id: Option<&str>,
    ) -> Reservation {
        let amount_usd = request.estimated_cost();
        let matching_rules: Vec<&BudgetRule> = policy
            .map(|policy| {
                policy
                    .budgets
                    .iter()
                    .filter(|rule| {
                        selected_budget_id
                            .map(|selected_budget_id| rule.id == selected_budget_id)
                            .unwrap_or_else(|| budget_rule_matches(rule, request))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let budget_rule_ids: Vec<String> =
            matching_rules.iter().map(|rule| rule.id.clone()).collect();
        let mut limit_window_spends = Vec::new();

        for rule in matching_rules {
            for limit in &rule.limits.spend {
                let limit_id = spend_limit_identifier(limit).to_owned();
                let scope_key = spend_limit_scope_key(limit.by, request)
                    .expect("selected budget has valid spend window scopes");
                if matches!(limit.mode, Some(SpendWindowMode::Tumbling)) {
                    let Some(window) = crate::policy::parse_limit_window(&limit.window) else {
                        continue;
                    };
                    self.limit_window(rule, &limit_id, window, &scope_key, now)
                        .used_usd += amount_usd;
                } else if matches!(limit.mode, Some(SpendWindowMode::Rolling)) {
                    let bucket_key = (
                        rule.id.clone(),
                        limit_id.clone(),
                        scope_key.clone(),
                        rolling_bucket_start(now),
                    );
                    *self.rolling_spend_buckets.entry(bucket_key).or_insert(0.0) += amount_usd;
                }
                limit_window_spends.push(LimitWindowReservationSpend {
                    rule_id: rule.id.clone(),
                    limit_id,
                    scope_key,
                });
            }
        }

        let reservation = Reservation {
            id: format!("replay-reservation-{}", self.reservations.len() + 1),
            amount_usd,
            currency: "USD".to_owned(),
            status: ReservationStatus::Active,
            created_at: now,
            expires_at: now + Duration::hours(1),
        };
        self.reservations.insert(
            reservation.id.clone(),
            StoredReservation {
                reservation: reservation.clone(),
                estimated_cost_usd: amount_usd,
                budget_rule_ids,
                limit_window_spends,
                allocation_spends: Vec::new(),
            },
        );
        reservation
    }

    fn limit_window(
        &mut self,
        rule: &BudgetRule,
        limit_id: &str,
        window_seconds: Duration,
        scope_key: &str,
        now: DateTime<Utc>,
    ) -> &mut WindowState {
        let key = (rule.id.clone(), limit_id.to_owned(), scope_key.to_owned());
        let window = self.limit_windows.entry(key).or_insert(WindowState {
            started_at: now,
            used_usd: 0.0,
        });

        if now - window.started_at >= window_seconds {
            window.started_at =
                advance_tumbling_window_start(window.started_at, window_seconds, now);
            window.used_usd = 0.0;
        }

        window
    }

    fn limit_window_used_usd(
        &self,
        rule: &BudgetRule,
        limit_id: &str,
        window_seconds: Duration,
        scope_key: &str,
        now: DateTime<Utc>,
    ) -> f64 {
        let key = (rule.id.clone(), limit_id.to_owned(), scope_key.to_owned());
        let Some(window) = self.limit_windows.get(&key) else {
            return 0.0;
        };
        if now - window.started_at >= window_seconds {
            0.0
        } else {
            window.used_usd
        }
    }

    fn persist_decision(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        decision: &AuthorizeDecision,
        selected_budget_id: Option<&str>,
        limit_hits: &[DecisionLimitHitReport],
    ) -> Result<(), NoetError> {
        match &self.store {
            LedgerStore::Postgres(_) => {
                let result = self.persist_decision_postgres(
                    policy,
                    request,
                    decision,
                    selected_budget_id,
                    limit_hits,
                );
                if result.is_ok() {
                    self.pending_advisory_cadence.clear();
                }
                return result;
            }
            LedgerStore::Sqlite(_) => {}
            LedgerStore::InMemory => return Ok(()),
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(());
        };
        let trace_id = string_metadata(request, "trace_id");
        let session_id = string_metadata(request, "session_id");
        let request_id = string_metadata(request, "request_id");
        let routing =
            self.routing_persistence_fields(policy, request, decision, selected_budget_id);
        let outcome = outcome_text(decision.outcome);
        let app_run_key = decision_app_run_key(
            trace_id.as_deref(),
            request.provider.as_deref(),
            request.model.as_deref(),
            outcome,
            routing.selected_budget_id.as_deref(),
            &request.metadata,
            decision.created_at,
        );
        let routing_report = decision_routing_report(&routing);
        conn.execute(
            "
            INSERT INTO decisions (
                decision_id, trace_id, session_id, request_id, subject, project, provider, model,
                estimated_tokens, estimated_cost_usd, outcome, action, explanations_json, metadata_json,
                entities_json, selected_budget_id, matched_entity, selection_reason, rejected_budget_id,
                rejected_budget_reason, model_check, budget_window_remaining_usd, routing_json,
                limit_hits_json, max_tool_calls, max_agent_steps, max_retries, app_run_key, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29
            )
            ",
            params![
                decision.decision_id.as_str(),
                trace_id.as_deref(),
                session_id.as_deref(),
                request_id.as_deref(),
                request.subject.as_deref(),
                request.project.as_deref(),
                request.provider.as_deref(),
                request.model.as_deref(),
                request.estimated_tokens.map(|value| value as i64),
                request.estimated_cost_usd,
                outcome,
                action_text(decision.action),
                serde_json::to_string(&decision.explanations)?,
                serde_json::to_string(&request.metadata)?,
                serde_json::to_string(&request.entities)?,
                routing.selected_budget_id.as_deref(),
                routing.matched_entity.as_deref(),
                routing.selection_reason.as_deref(),
                routing.rejected_budget_id.as_deref(),
                routing.rejected_budget_reason.as_deref(),
                routing.model_check.as_deref(),
                routing.budget_window_remaining_usd,
                serde_json::to_string(&routing_report)?,
                serde_json::to_string(limit_hits)?,
                routing.tool_calls.map(|value| value as i64),
                routing.agent_steps.map(|value| value as i64),
                routing.retries.map(|value| value as i64),
                app_run_key,
                decision.created_at.to_rfc3339(),
            ],
        )?;
        if let Some(reservation) = &decision.reservation {
            let limit_window_spends = self
                .reservations
                .get(&reservation.id)
                .map(|stored| stored.limit_window_spends.as_slice())
                .unwrap_or_default();
            let budget_rule_ids = self
                .reservations
                .get(&reservation.id)
                .map(|stored| stored.budget_rule_ids.as_slice())
                .unwrap_or_default();
            conn.execute(
                "
                INSERT INTO reservations (
                    id, decision_id, amount_usd, estimated_amount_usd, currency, status,
                    created_at, expires_at, budget_rule_ids_json, limit_window_spends_json,
                    allocation_spends_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ",
                params![
                    reservation.id.as_str(),
                    decision.decision_id.as_str(),
                    reservation.amount_usd,
                    reservation.amount_usd,
                    reservation.currency.as_str(),
                    reservation_status_text(reservation.status),
                    reservation.created_at.to_rfc3339(),
                    reservation.expires_at.to_rfc3339(),
                    serde_json::to_string(budget_rule_ids)?,
                    serde_json::to_string(&limit_window_spends)?,
                    serde_json::to_string(
                        &self
                            .reservations
                            .get(&reservation.id)
                            .map(|stored| stored.allocation_spends.as_slice())
                            .unwrap_or_default(),
                    )?,
                ],
            )?;
            for spend in limit_window_spends {
                conn.execute(
                    "
                    INSERT INTO reservation_limit_scopes (
                        reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    ",
                    params![
                        reservation.id.as_str(),
                        spend.rule_id.as_str(),
                        spend.limit_id.as_str(),
                        spend.scope_key.as_str(),
                        reservation.amount_usd,
                        reservation.created_at.to_rfc3339()
                    ],
                )?;
                conn.execute(
                    "
                    INSERT INTO rolling_spend_buckets (
                        rule_id, limit_id, scope_key, bucket_start, amount_usd
                    ) VALUES (?1, ?2, ?3, ?4, ?5)
                    ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
                        amount_usd = amount_usd + excluded.amount_usd
                    ",
                    params![
                        spend.rule_id.as_str(),
                        spend.limit_id.as_str(),
                        spend.scope_key.as_str(),
                        rolling_bucket_start(reservation.created_at).to_rfc3339(),
                        reservation.amount_usd
                    ],
                )?;
            }
        }
        persist_advisory_cadence_sqlite(conn, &self.pending_advisory_cadence)?;
        self.pending_advisory_cadence.clear();
        Ok(())
    }

    fn persist_decision_postgres(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        decision: &AuthorizeDecision,
        selected_budget_id: Option<&str>,
        limit_hits: &[DecisionLimitHitReport],
    ) -> Result<(), NoetError> {
        let LedgerStore::Postgres(pg_conn) = &self.store else {
            return Ok(());
        };
        let trace_id = string_metadata(request, "trace_id");
        let session_id = string_metadata(request, "session_id");
        let request_id = string_metadata(request, "request_id");
        let routing =
            self.routing_persistence_fields(policy, request, decision, selected_budget_id);
        let outcome = outcome_text(decision.outcome);
        let app_run_key = decision_app_run_key(
            trace_id.as_deref(),
            request.provider.as_deref(),
            request.model.as_deref(),
            outcome,
            routing.selected_budget_id.as_deref(),
            &request.metadata,
            decision.created_at,
        );
        let routing_report = decision_routing_report(&routing);
        let estimated_tokens = request.estimated_tokens.map(|value| value as i64);
        let explanations_json = serde_json::to_string(&decision.explanations)?;
        let metadata_json = serde_json::to_string(&request.metadata)?;
        let entities_json = serde_json::to_string(&request.entities)?;
        let routing_json = serde_json::to_string(&routing_report)?;
        let limit_hits_json = serde_json::to_string(limit_hits)?;
        let max_tool_calls = routing.tool_calls.map(|value| value as i64);
        let max_agent_steps = routing.agent_steps.map(|value| value as i64);
        let max_retries = routing.retries.map(|value| value as i64);
        let created_at = decision.created_at.to_rfc3339();

        let mut pg = pg_conn.0.lock().expect("postgres mutex");
        let mut tx = pg.transaction()?;
        tx.execute(
            "
            INSERT INTO decisions (
                decision_id, trace_id, session_id, request_id, subject, project, provider, model,
                estimated_tokens, estimated_cost_usd, outcome, action, explanations_json, metadata_json,
                entities_json, selected_budget_id, matched_entity, selection_reason, rejected_budget_id,
                rejected_budget_reason, model_check, budget_window_remaining_usd, routing_json,
                limit_hits_json, max_tool_calls, max_agent_steps, max_retries, app_run_key, created_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
                $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29
            )
            ",
            &[
                &decision.decision_id.as_str(),
                &trace_id.as_deref(),
                &session_id.as_deref(),
                &request_id.as_deref(),
                &request.subject.as_deref(),
                &request.project.as_deref(),
                &request.provider.as_deref(),
                &request.model.as_deref(),
                &estimated_tokens,
                &request.estimated_cost_usd,
                &outcome,
                &action_text(decision.action),
                &explanations_json,
                &metadata_json,
                &entities_json,
                &routing.selected_budget_id.as_deref(),
                &routing.matched_entity.as_deref(),
                &routing.selection_reason.as_deref(),
                &routing.rejected_budget_id.as_deref(),
                &routing.rejected_budget_reason.as_deref(),
                &routing.model_check.as_deref(),
                &routing.budget_window_remaining_usd,
                &routing_json,
                &limit_hits_json,
                &max_tool_calls,
                &max_agent_steps,
                &max_retries,
                &app_run_key,
                &created_at,
            ],
        )?;
        if let Some(reservation) = &decision.reservation {
            let limit_window_spends = self
                .reservations
                .get(&reservation.id)
                .map(|stored| stored.limit_window_spends.as_slice())
                .unwrap_or_default();
            let budget_rule_ids = self
                .reservations
                .get(&reservation.id)
                .map(|stored| stored.budget_rule_ids.as_slice())
                .unwrap_or_default();
            let budget_rule_ids_json = serde_json::to_string(budget_rule_ids)?;
            let limit_window_spends_json = serde_json::to_string(&limit_window_spends)?;
            let allocation_spends_json = serde_json::to_string(
                &self
                    .reservations
                    .get(&reservation.id)
                    .map(|stored| stored.allocation_spends.as_slice())
                    .unwrap_or_default(),
            )?;
            let reservation_created_at = reservation.created_at.to_rfc3339();
            let reservation_expires_at = reservation.expires_at.to_rfc3339();
            tx.execute(
                "
                INSERT INTO reservations (
                    id, decision_id, amount_usd, estimated_amount_usd, currency, status,
                    created_at, expires_at, budget_rule_ids_json, limit_window_spends_json,
                    allocation_spends_json
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                ",
                &[
                    &reservation.id.as_str(),
                    &decision.decision_id.as_str(),
                    &reservation.amount_usd,
                    &reservation.amount_usd,
                    &reservation.currency.as_str(),
                    &reservation_status_text(reservation.status),
                    &reservation_created_at,
                    &reservation_expires_at,
                    &budget_rule_ids_json,
                    &limit_window_spends_json,
                    &allocation_spends_json,
                ],
            )?;
            for spend in limit_window_spends {
                tx.execute(
                    "
                    INSERT INTO reservation_limit_scopes (
                        reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at
                    ) VALUES ($1, $2, $3, $4, $5, $6)
                    ",
                    &[
                        &reservation.id.as_str(),
                        &spend.rule_id.as_str(),
                        &spend.limit_id.as_str(),
                        &spend.scope_key.as_str(),
                        &reservation.amount_usd,
                        &reservation_created_at,
                    ],
                )?;
                let bucket_start = rolling_bucket_start(reservation.created_at).to_rfc3339();
                tx.execute(
                    "
                    INSERT INTO rolling_spend_buckets (
                        rule_id, limit_id, scope_key, bucket_start, amount_usd
                    ) VALUES ($1, $2, $3, $4, $5)
                    ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
                        amount_usd = rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd
                    ",
                    &[
                        &spend.rule_id.as_str(),
                        &spend.limit_id.as_str(),
                        &spend.scope_key.as_str(),
                        &bucket_start,
                        &reservation.amount_usd,
                    ],
                )?;
            }
        }
        persist_advisory_cadence_postgres(&mut tx, &self.pending_advisory_cadence)?;
        tx.commit()?;
        Ok(())
    }

    fn routing_persistence_fields(
        &self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        decision: &AuthorizeDecision,
        selected_budget_id: Option<&str>,
    ) -> RoutingPersistenceFields {
        let selected_budget_id = decision
            .reservation
            .as_ref()
            .and_then(|reservation| self.reservations.get(&reservation.id))
            .and_then(|stored| stored.budget_rule_ids.first())
            .cloned()
            .or_else(|| selected_budget_id.map(ToOwned::to_owned));

        let mut fields = RoutingPersistenceFields {
            selected_budget_id: selected_budget_id.clone(),
            ..RoutingPersistenceFields::default()
        };

        if let (Some(policy), Some(selected_budget_id)) = (policy, selected_budget_id.as_deref()) {
            if let Some(rule) = policy
                .budgets
                .iter()
                .find(|rule| rule.id == selected_budget_id)
            {
                fields.matched_entity =
                    matched_entity_and_rank(rule, request, &specificity_order(policy)).0;
                if let Some(projection) =
                    biggest_spend_window_projection(self, rule, request, 0.0, decision.created_at)
                {
                    fields.budget_window_remaining_usd =
                        Some((projection.max_usd - projection.projected_spend_usd).max(0.0));
                    fields.budget_window_mode = Some(match projection.limit_mode {
                        SpendWindowMode::Rolling => "rolling".to_owned(),
                        SpendWindowMode::Tumbling => "tumbling".to_owned(),
                    });
                    fields.budget_window_started_at = projection.window_started_at;
                    fields.budget_window_ends_at = projection.window_ends_at;
                }
                fields.tool_calls = rule.limits.tool_calls;
                fields.agent_steps = rule.limits.agent_steps;
                fields.retries = rule.limits.retries;
            }
            fields.selection_reason = decision
                .explanations
                .iter()
                .find(|explanation| explanation.rule_id == selected_budget_id)
                .map(|explanation| explanation.reason.clone());
        }

        if let Some(requested_budget_id) = request.budget_id.as_deref()
            && selected_budget_id.as_deref() != Some(requested_budget_id)
        {
            fields.rejected_budget_id = Some(requested_budget_id.to_owned());
            fields.rejected_budget_reason = decision
                .explanations
                .iter()
                .find(|explanation| explanation.rule_id == requested_budget_id)
                .map(|explanation| explanation.reason.clone());
        }

        fields.model_check = routing_model_check(decision, selected_budget_id.as_deref());
        fields
    }

    fn lifecycle_limit_report_items(
        &self,
        trace_id: &str,
    ) -> Result<Option<Vec<TraceReportItem>>, NoetError> {
        let config = if let LedgerStore::Postgres(pg_conn) = &self.store {
            pg_conn
                .0
                .lock()
                .expect("postgres mutex")
                .query_opt(
                    "
                    SELECT created_at, max_tool_calls, max_agent_steps, max_retries
                    FROM decisions
                    WHERE trace_id = $1
                    ORDER BY created_at DESC
                    LIMIT 1
                    ",
                    &[&trace_id],
                )?
                .map(|row| {
                    (
                        parse_time(row.get::<_, String>(0)),
                        row.get::<_, Option<i64>>(1)
                            .map(|value| value.max(0) as u64),
                        row.get::<_, Option<i64>>(2)
                            .map(|value| value.max(0) as u64),
                        row.get::<_, Option<i64>>(3)
                            .map(|value| value.max(0) as u64),
                    )
                })
        } else {
            let LedgerStore::Sqlite(conn) = &self.store else {
                return Ok(None);
            };
            conn.query_row(
                "
                    SELECT created_at, max_tool_calls, max_agent_steps, max_retries
                    FROM decisions
                    WHERE trace_id = ?1
                    ORDER BY created_at DESC
                    LIMIT 1
                    ",
                [trace_id],
                |row| {
                    Ok((
                        parse_time(row.get::<_, String>(0)?),
                        row.get::<_, Option<i64>>(1)?
                            .map(|value| value.max(0) as u64),
                        row.get::<_, Option<i64>>(2)?
                            .map(|value| value.max(0) as u64),
                        row.get::<_, Option<i64>>(3)?
                            .map(|value| value.max(0) as u64),
                    ))
                },
            )
            .optional()?
        };
        let Some((occurred_at, max_tool_calls, max_agent_steps, max_retries)) = config else {
            return Ok(None);
        };

        let tool_calls = self.event_count_for_trace(trace_id, "pi.tool_call")?;
        let agent_steps = self.event_count_for_trace(trace_id, "pi.turn_end")?;
        let provider_calls = self.event_count_for_trace(trace_id, "pi.provider_call.started")?;
        let retries = provider_calls.saturating_sub(agent_steps);

        let mut items = Vec::new();
        if let Some(limit) = max_tool_calls
            && tool_calls > limit
        {
            items.push(TraceReportItem {
                occurred_at,
                kind: "limit.report_only.tool_calls".to_owned(),
                summary: format!(
                    "tool_calls={tool_calls} max_tool_calls={limit} reporting_only=true source=pi.tool_call"
                ),
                trace_id: Some(trace_id.to_owned()),
                agent_run_id: None,
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            });
        }
        if let Some(limit) = max_agent_steps
            && agent_steps > limit
        {
            items.push(TraceReportItem {
                occurred_at,
                kind: "limit.report_only.agent_steps".to_owned(),
                summary: format!(
                    "agent_steps={agent_steps} max_agent_steps={limit} reporting_only=true source=pi.turn_end"
                ),
                trace_id: Some(trace_id.to_owned()),
                agent_run_id: None,
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            });
        }
        if let Some(limit) = max_retries
            && retries > limit
        {
            items.push(TraceReportItem {
                occurred_at,
                kind: "limit.report_only.retries".to_owned(),
                summary: format!(
                    "retries={retries} provider_calls={provider_calls} turns={agent_steps} max_retries={limit} reporting_only=true source=pi.provider_call.started,pi.turn_end"
                ),
                trace_id: Some(trace_id.to_owned()),
                agent_run_id: None,
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            });
        }
        Ok((!items.is_empty()).then_some(items))
    }

    fn event_count_for_trace(&self, trace_id: &str, kind: &str) -> Result<u64, NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let count: i64 = pg_conn
                .0
                .lock()
                .expect("postgres mutex")
                .query_one(
                    "
                    SELECT COUNT(*)
                    FROM events
                    WHERE trace_id = $1 AND kind = $2
                    ",
                    &[&trace_id, &kind],
                )?
                .get(0);
            return Ok(count.max(0) as u64);
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(0);
        };
        conn.query_row(
            "
            SELECT COUNT(*)
            FROM events
            WHERE trace_id = ?1 AND kind = ?2
            ",
            params![trace_id, kind],
            |row| Ok(row.get::<_, i64>(0)?.max(0) as u64),
        )
        .map_err(NoetError::from)
    }

    fn persist_finalization(
        &self,
        reservation: &Reservation,
        payload: &FinalizeReservation,
    ) -> Result<(), NoetError> {
        match &self.store {
            LedgerStore::Postgres(_) => {
                return self.persist_finalization_postgres(reservation, payload);
            }
            LedgerStore::Sqlite(_) => {}
            LedgerStore::InMemory => return Ok(()),
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(());
        };
        let now = Utc::now();
        conn.execute(
            "
            UPDATE reservations
            SET amount_usd = ?2, actual_amount_usd = ?2, status = ?3, finalized_at = ?4
            WHERE id = ?1
            ",
            params![
                reservation.id.as_str(),
                reservation.amount_usd,
                reservation_status_text(reservation.status),
                now.to_rfc3339(),
            ],
        )?;
        if let Some(usage) = &payload.usage {
            let decision_trace_id: Option<String> = conn
                .query_row(
                    "
                    SELECT d.trace_id
                    FROM reservations r
                    JOIN decisions d ON d.decision_id = r.decision_id
                    WHERE r.id = ?1
                    ",
                    [reservation.id.as_str()],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let trace_id =
                decision_trace_id.or_else(|| string_value(&payload.metadata, "trace_id"));
            conn.execute(
                "
                INSERT INTO usage_observations (
                    id, reservation_id, trace_id, provider, model, input_tokens, output_tokens,
                    total_tokens, cost_usd, latency_ms, stop_reason, source, metadata_json,
                    created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                ",
                params![
                    Uuid::new_v4().to_string(),
                    reservation.id.as_str(),
                    trace_id.as_deref(),
                    usage.provider.as_deref(),
                    usage.model.as_deref(),
                    usage.input_tokens.map(|value| value as i64),
                    usage.output_tokens.map(|value| value as i64),
                    usage.total_tokens.map(|value| value as i64),
                    usage.cost_usd.or(Some(reservation.amount_usd)),
                    usage.latency_ms.map(|value| value as i64),
                    usage.stop_reason.as_deref(),
                    "reservation.finalize",
                    serde_json::to_string(&payload.metadata)?,
                    now.to_rfc3339(),
                ],
            )?;
        }
        Ok(())
    }

    fn persist_finalization_postgres(
        &self,
        reservation: &Reservation,
        payload: &FinalizeReservation,
    ) -> Result<(), NoetError> {
        let LedgerStore::Postgres(pg_conn) = &self.store else {
            return Ok(());
        };
        let now = Utc::now().to_rfc3339();
        let mut pg = pg_conn.0.lock().expect("postgres mutex");
        let mut tx = pg.transaction()?;
        tx.execute(
            "
            UPDATE reservations
            SET amount_usd = $2, actual_amount_usd = $2, status = $3, finalized_at = $4
            WHERE id = $1
            ",
            &[
                &reservation.id.as_str(),
                &reservation.amount_usd,
                &reservation_status_text(reservation.status),
                &now,
            ],
        )?;
        if let Some(usage) = &payload.usage {
            let decision_trace_id = tx
                .query_opt(
                    "
                    SELECT d.trace_id
                    FROM reservations r
                    JOIN decisions d ON d.decision_id = r.decision_id
                    WHERE r.id = $1
                    ",
                    &[&reservation.id.as_str()],
                )?
                .and_then(|row| row.get::<_, Option<String>>(0));
            let trace_id =
                decision_trace_id.or_else(|| string_value(&payload.metadata, "trace_id"));
            let input_tokens = usage.input_tokens.map(|value| value as i64);
            let output_tokens = usage.output_tokens.map(|value| value as i64);
            let total_tokens = usage.total_tokens.map(|value| value as i64);
            let latency_ms = usage.latency_ms.map(|value| value as i64);
            let metadata_json = serde_json::to_string(&payload.metadata)?;
            tx.execute(
                "
                INSERT INTO usage_observations (
                    id, reservation_id, trace_id, provider, model, input_tokens, output_tokens,
                    total_tokens, cost_usd, latency_ms, stop_reason, source, metadata_json,
                    created_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                ",
                &[
                    &Uuid::new_v4().to_string(),
                    &reservation.id.as_str(),
                    &trace_id.as_deref(),
                    &usage.provider.as_deref(),
                    &usage.model.as_deref(),
                    &input_tokens,
                    &output_tokens,
                    &total_tokens,
                    &usage.cost_usd.or(Some(reservation.amount_usd)),
                    &latency_ms,
                    &usage.stop_reason.as_deref(),
                    &"reservation.finalize",
                    &metadata_json,
                    &now,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn persist_event(&self, event: &TraceEvent) -> Result<(), NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let occurred_at = event.occurred_at.unwrap_or_else(Utc::now);
            let source = event
                .payload
                .as_object()
                .and_then(|payload| payload.get("source"))
                .and_then(|value| value.as_str());
            let id = event
                .id
                .as_deref()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let payload_json = serde_json::to_string(&event.payload)?;
            pg_conn.0.lock().expect("postgres mutex").execute(
                "
                INSERT INTO events (id, trace_id, kind, occurred_at, source, payload_json)
                VALUES ($1, $2, $3, $4, $5, $6)
                ",
                &[
                    &id,
                    &event.trace_id.as_deref(),
                    &event.kind.as_str(),
                    &occurred_at.to_rfc3339(),
                    &source,
                    &payload_json,
                ],
            )?;
            return Ok(());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(());
        };
        let occurred_at = event.occurred_at.unwrap_or_else(Utc::now);
        let source = event
            .payload
            .as_object()
            .and_then(|payload| payload.get("source"))
            .and_then(|value| value.as_str());
        conn.execute(
            "
            INSERT INTO events (id, trace_id, kind, occurred_at, source, payload_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                event
                    .id
                    .as_deref()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                event.trace_id.as_deref(),
                event.kind.as_str(),
                occurred_at.to_rfc3339(),
                source,
                serde_json::to_string(&event.payload)?,
            ],
        )?;
        Ok(())
    }

    fn persist_windows(&self) -> Result<(), NoetError> {
        Ok(())
    }

    fn persist_limit_windows(&self) -> Result<(), NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let mut pg = pg_conn.0.lock().expect("postgres mutex");
            for ((rule_id, limit_id, scope_key), window) in &self.limit_windows {
                pg.execute(
                    "
                    INSERT INTO limit_window_states (rule_id, limit_id, scope_key, started_at, used_usd)
                    VALUES ($1, $2, $3, $4, $5)
                    ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
                        started_at = EXCLUDED.started_at,
                        used_usd = EXCLUDED.used_usd
                    ",
                    &[
                        &rule_id,
                        &limit_id,
                        &scope_key,
                        &window.started_at.to_rfc3339(),
                        &window.used_usd,
                    ],
                )?;
            }
            return Ok(());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(());
        };
        for ((rule_id, limit_id, scope_key), window) in &self.limit_windows {
            conn.execute(
                "
                INSERT INTO limit_window_states (rule_id, limit_id, scope_key, started_at, used_usd)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
                    started_at = excluded.started_at,
                    used_usd = excluded.used_usd
                ",
                params![
                    rule_id,
                    limit_id,
                    scope_key,
                    window.started_at.to_rfc3339(),
                    window.used_usd
                ],
            )?;
        }
        Ok(())
    }

    fn persist_allocation_buckets(&self) -> Result<(), NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let mut pg = pg_conn.0.lock().expect("postgres mutex");
            for ((rule_id, entity_key), bucket) in &self.allocation_buckets {
                pg.execute(
                    "
                    INSERT INTO budget_allocation_buckets (
                        rule_id, entity_key, started_at, protected_amount_usd, current_grant_usd, carryover_usd
                    ) VALUES ($1, $2, $3, $4, $5, $6)
                    ON CONFLICT(rule_id, entity_key) DO UPDATE SET
                        started_at = EXCLUDED.started_at,
                        protected_amount_usd = EXCLUDED.protected_amount_usd,
                        current_grant_usd = EXCLUDED.current_grant_usd,
                        carryover_usd = EXCLUDED.carryover_usd
                    ",
                    &[
                        &rule_id,
                        &entity_key,
                        &bucket.started_at.to_rfc3339(),
                        &bucket.protected_amount_usd,
                        &bucket.current_grant_usd,
                        &bucket.carryover_usd,
                    ],
                )?;
            }
            return Ok(());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(());
        };
        for ((rule_id, entity_key), bucket) in &self.allocation_buckets {
            conn.execute(
                "
                INSERT INTO budget_allocation_buckets (
                    rule_id, entity_key, started_at, protected_amount_usd, current_grant_usd, carryover_usd
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(rule_id, entity_key) DO UPDATE SET
                    started_at = excluded.started_at,
                    protected_amount_usd = excluded.protected_amount_usd,
                    current_grant_usd = excluded.current_grant_usd,
                    carryover_usd = excluded.carryover_usd
                ",
                params![
                    rule_id,
                    entity_key,
                    bucket.started_at.to_rfc3339(),
                    bucket.protected_amount_usd,
                    bucket.current_grant_usd,
                    bucket.carryover_usd
                ],
            )?;
        }
        Ok(())
    }

    fn load_limit_windows(&mut self) -> Result<(), NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let rows = pg_conn.0.lock().expect("postgres mutex").query(
                "
                SELECT rule_id, limit_id, scope_key, started_at, used_usd
                FROM limit_window_states
                ",
                &[],
            )?;
            self.limit_windows = rows
                .into_iter()
                .map(|row| {
                    (
                        (row.get(0), row.get(1), row.get(2)),
                        WindowState {
                            started_at: parse_time(row.get::<_, String>(3)),
                            used_usd: row.get(4),
                        },
                    )
                })
                .collect();
            return Ok(());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(());
        };
        let mut stmt = conn.prepare(
            "
            SELECT rule_id, limit_id, scope_key, started_at, used_usd
            FROM limit_window_states
            ",
        )?;
        let limit_windows: Vec<((String, String, String), WindowState)> = stmt
            .query_map([], |row| {
                Ok((
                    (row.get(0)?, row.get(1)?, row.get(2)?),
                    WindowState {
                        started_at: parse_time(row.get::<_, String>(3)?),
                        used_usd: row.get(4)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        self.limit_windows = limit_windows.into_iter().collect();
        Ok(())
    }

    fn load_allocation_buckets(&mut self) -> Result<(), NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let rows = pg_conn.0.lock().expect("postgres mutex").query(
                "
                SELECT rule_id, entity_key, started_at, protected_amount_usd, current_grant_usd, carryover_usd
                FROM budget_allocation_buckets
                ",
                &[],
            )?;
            self.allocation_buckets = rows
                .into_iter()
                .map(|row| {
                    let rule_id: String = row.get(0);
                    let entity_key: String = row.get(1);
                    let started_at: Option<String> = row.get(2);
                    (
                        (rule_id, entity_key),
                        AllocationBucketState {
                            started_at: started_at.map(parse_time).unwrap_or_else(Utc::now),
                            protected_amount_usd: row.get(3),
                            current_grant_usd: row.get(4),
                            carryover_usd: row.get(5),
                        },
                    )
                })
                .collect();
            return Ok(());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(());
        };
        let mut stmt = conn.prepare(
            "
            SELECT rule_id, entity_key, started_at, protected_amount_usd, current_grant_usd, carryover_usd
            FROM budget_allocation_buckets
            ",
        )?;
        let buckets: Vec<((String, String), AllocationBucketState)> = stmt
            .query_map([], |row| {
                let rule_id: String = row.get(0)?;
                let entity_key: String = row.get(1)?;
                let started_at: Option<String> = row.get(2)?;
                Ok((
                    (rule_id, entity_key),
                    AllocationBucketState {
                        started_at: started_at.map(parse_time).unwrap_or_else(Utc::now),
                        protected_amount_usd: row.get(3)?,
                        current_grant_usd: row.get(4)?,
                        carryover_usd: row.get(5)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        self.allocation_buckets = buckets.into_iter().collect();
        Ok(())
    }

    fn load_advisory_cadence_sqlite(&mut self) -> Result<(), NoetError> {
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(());
        };
        let mut stmt = conn.prepare(
            "
            SELECT user_key, advisory_key, scope_key, last_shown_at
            FROM advisory_delivery_state
            ",
        )?;
        let rows: Vec<((String, String, String), DateTime<Utc>)> = stmt
            .query_map([], |row| {
                Ok((
                    (row.get(0)?, row.get(1)?, row.get(2)?),
                    parse_time(row.get::<_, String>(3)?),
                ))
            })?
            .collect::<Result<_, _>>()?;
        self.advisory_cadence = rows.into_iter().collect();
        Ok(())
    }

    fn load_advisory_cadence_postgres(&mut self) -> Result<(), NoetError> {
        let LedgerStore::Postgres(pg_conn) = &self.store else {
            return Ok(());
        };
        let rows = pg_conn.0.lock().expect("postgres mutex").query(
            "
            SELECT user_key, advisory_key, scope_key, last_shown_at
            FROM advisory_delivery_state
            ",
            &[],
        )?;
        self.advisory_cadence = rows
            .into_iter()
            .map(|row| {
                (
                    (row.get(0), row.get(1), row.get(2)),
                    parse_time(row.get::<_, String>(3)),
                )
            })
            .collect();
        Ok(())
    }

    fn load_active_reservations(&mut self) -> Result<(), NoetError> {
        if let LedgerStore::Postgres(pg_conn) = &self.store {
            let rows = pg_conn.0.lock().expect("postgres mutex").query(
                "
                SELECT id, amount_usd, estimated_amount_usd, currency, status, created_at, expires_at,
                       budget_rule_ids_json, limit_window_spends_json, allocation_spends_json
                FROM reservations
                WHERE status = 'active'
                ",
                &[],
            )?;
            self.reservations = rows
                .into_iter()
                .map(|row| {
                    let id: String = row.get(0);
                    let budget_rule_ids_json: String = row.get(7);
                    let limit_window_spends_json: String = row.get(8);
                    let allocation_spends_json: String = row.get(9);
                    (
                        id.clone(),
                        StoredReservation {
                            reservation: Reservation {
                                id,
                                amount_usd: row.get(1),
                                currency: row.get(3),
                                status: ReservationStatus::Active,
                                created_at: parse_time(row.get::<_, String>(5)),
                                expires_at: parse_time(row.get::<_, String>(6)),
                            },
                            estimated_cost_usd: row.get(2),
                            budget_rule_ids: serde_json::from_str(&budget_rule_ids_json)
                                .unwrap_or_default(),
                            limit_window_spends: serde_json::from_str(&limit_window_spends_json)
                                .unwrap_or_default(),
                            allocation_spends: serde_json::from_str(&allocation_spends_json)
                                .unwrap_or_default(),
                        },
                    )
                })
                .collect();
            return Ok(());
        }
        let LedgerStore::Sqlite(conn) = &self.store else {
            return Ok(());
        };
        let mut stmt = conn.prepare(
            "
            SELECT id, amount_usd, estimated_amount_usd, currency, status, created_at, expires_at,
                   budget_rule_ids_json, limit_window_spends_json, allocation_spends_json
            FROM reservations
            WHERE status = 'active'
            ",
        )?;
        let reservations: Vec<(String, StoredReservation)> = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let budget_rule_ids_json: String = row.get(7)?;
                let budget_rule_ids =
                    serde_json::from_str(&budget_rule_ids_json).unwrap_or_default();
                let limit_window_spends_json: String = row.get(8)?;
                let limit_window_spends =
                    serde_json::from_str(&limit_window_spends_json).unwrap_or_default();
                let allocation_spends_json: String = row.get(9)?;
                let allocation_spends =
                    serde_json::from_str(&allocation_spends_json).unwrap_or_default();
                Ok((
                    id.clone(),
                    StoredReservation {
                        reservation: Reservation {
                            id,
                            amount_usd: row.get(1)?,
                            currency: row.get(3)?,
                            status: ReservationStatus::Active,
                            created_at: parse_time(row.get::<_, String>(5)?),
                            expires_at: parse_time(row.get::<_, String>(6)?),
                        },
                        estimated_cost_usd: row.get(2)?,
                        budget_rule_ids,
                        limit_window_spends,
                        allocation_spends,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        self.reservations = reservations.into_iter().collect();
        Ok(())
    }
}

fn merge_policy_action(current: PolicyAction, next: PolicyAction) -> PolicyAction {
    use PolicyAction::{Allow, Ask, Block, Warn};

    match (current, next) {
        (Block, _) | (_, Block) => Block,
        (Ask, _) | (_, Ask) => Ask,
        (Warn, _) | (_, Warn) => Warn,
        _ => Allow,
    }
}

fn init_schema(conn: &Connection) -> Result<(), NoetError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );
        INSERT OR IGNORE INTO schema_migrations (version, applied_at)
        VALUES (1, datetime('now'));

        CREATE TABLE IF NOT EXISTS decisions (
            decision_id TEXT PRIMARY KEY,
            trace_id TEXT,
            session_id TEXT,
            request_id TEXT,
            subject TEXT,
            project TEXT,
            provider TEXT,
            model TEXT,
            estimated_tokens INTEGER,
            estimated_cost_usd REAL,
            outcome TEXT NOT NULL,
            action TEXT NOT NULL DEFAULT 'allow',
            explanations_json TEXT NOT NULL,
            metadata_json TEXT NOT NULL,
            entities_json TEXT NOT NULL DEFAULT '[]',
            selected_budget_id TEXT,
            matched_entity TEXT,
            selection_reason TEXT,
            rejected_budget_id TEXT,
            rejected_budget_reason TEXT,
            model_check TEXT,
            budget_window_remaining_usd REAL,
            routing_json TEXT,
            limit_hits_json TEXT,
            app_run_key TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_decisions_trace ON decisions(trace_id);
        CREATE INDEX IF NOT EXISTS idx_decisions_created ON decisions(created_at);

        CREATE TABLE IF NOT EXISTS reservations (
            id TEXT PRIMARY KEY,
            decision_id TEXT NOT NULL REFERENCES decisions(decision_id),
            amount_usd REAL NOT NULL,
            estimated_amount_usd REAL NOT NULL,
            actual_amount_usd REAL,
            currency TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            finalized_at TEXT,
            budget_rule_ids_json TEXT NOT NULL DEFAULT '[]',
            limit_window_spends_json TEXT NOT NULL DEFAULT '[]',
            allocation_spends_json TEXT NOT NULL DEFAULT '[]'
        );

        CREATE TABLE IF NOT EXISTS reservation_limit_scopes (
            reservation_id TEXT NOT NULL REFERENCES reservations(id),
            rule_id TEXT NOT NULL,
            limit_id TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            amount_usd REAL NOT NULL DEFAULT 0,
            created_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_lookup
            ON reservation_limit_scopes(rule_id, limit_id, scope_key);
        CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_reservation
            ON reservation_limit_scopes(reservation_id);
        CREATE INDEX IF NOT EXISTS idx_reservations_decision
            ON reservations(decision_id);
        CREATE INDEX IF NOT EXISTS idx_reservations_active
            ON reservations(status)
            WHERE status = 'active';
        CREATE INDEX IF NOT EXISTS idx_decisions_created_decision
            ON decisions(created_at, decision_id);

        CREATE TABLE IF NOT EXISTS rolling_spend_buckets (
            rule_id TEXT NOT NULL,
            limit_id TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            bucket_start TEXT NOT NULL,
            amount_usd REAL NOT NULL,
            PRIMARY KEY (rule_id, limit_id, scope_key, bucket_start)
        );

        CREATE TABLE IF NOT EXISTS usage_observations (
            id TEXT PRIMARY KEY,
            reservation_id TEXT REFERENCES reservations(id),
            trace_id TEXT,
            provider TEXT,
            model TEXT,
            input_tokens INTEGER,
            output_tokens INTEGER,
            total_tokens INTEGER,
            cost_usd REAL,
            latency_ms INTEGER,
            stop_reason TEXT,
            source TEXT,
            metadata_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_usage_trace ON usage_observations(trace_id);

        CREATE TABLE IF NOT EXISTS events (
            id TEXT PRIMARY KEY,
            trace_id TEXT,
            kind TEXT NOT NULL,
            occurred_at TEXT NOT NULL,
            source TEXT,
            payload_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_events_trace ON events(trace_id);
        CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);

        CREATE TABLE IF NOT EXISTS budget_windows (
            rule_id TEXT PRIMARY KEY,
            started_at TEXT NOT NULL,
            used_usd REAL NOT NULL
        );

        CREATE TABLE IF NOT EXISTS limit_window_states (
            rule_id TEXT NOT NULL,
            limit_id TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            started_at TEXT NOT NULL,
            used_usd REAL NOT NULL,
            PRIMARY KEY (rule_id, limit_id, scope_key)
        );

        CREATE TABLE IF NOT EXISTS budget_allocation_buckets (
            rule_id TEXT NOT NULL,
            entity_key TEXT NOT NULL,
            started_at TEXT,
            protected_amount_usd REAL NOT NULL DEFAULT 0,
            current_grant_usd REAL NOT NULL,
            carryover_usd REAL NOT NULL,
            PRIMARY KEY (rule_id, entity_key)
        );

        CREATE TABLE IF NOT EXISTS advisory_delivery_state (
            user_key TEXT NOT NULL,
            advisory_key TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            last_shown_at TEXT NOT NULL,
            PRIMARY KEY (user_key, advisory_key, scope_key)
        );
        ",
    )?;
    ensure_column(
        conn,
        "decisions",
        "selected_budget_id",
        "selected_budget_id TEXT",
    )?;
    ensure_column(conn, "decisions", "matched_entity", "matched_entity TEXT")?;
    ensure_column(
        conn,
        "decisions",
        "action",
        "action TEXT NOT NULL DEFAULT 'allow'",
    )?;
    ensure_column(
        conn,
        "reservations",
        "limit_window_spends_json",
        "limit_window_spends_json TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(
        conn,
        "reservation_limit_scopes",
        "amount_usd",
        "amount_usd REAL NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "reservation_limit_scopes",
        "created_at",
        "created_at TEXT",
    )?;
    ensure_column(
        conn,
        "decisions",
        "selection_reason",
        "selection_reason TEXT",
    )?;
    ensure_column(
        conn,
        "decisions",
        "rejected_budget_id",
        "rejected_budget_id TEXT",
    )?;
    ensure_column(
        conn,
        "decisions",
        "rejected_budget_reason",
        "rejected_budget_reason TEXT",
    )?;
    ensure_column(conn, "decisions", "model_check", "model_check TEXT")?;
    ensure_column(conn, "decisions", "routing_json", "routing_json TEXT")?;
    ensure_column(conn, "decisions", "limit_hits_json", "limit_hits_json TEXT")?;
    ensure_column(conn, "decisions", "app_run_key", "app_run_key TEXT")?;
    ensure_column(
        conn,
        "decisions",
        "budget_window_remaining_usd",
        "budget_window_remaining_usd REAL",
    )?;
    ensure_column(
        conn,
        "decisions",
        "max_tool_calls",
        "max_tool_calls INTEGER",
    )?;
    ensure_column(
        conn,
        "decisions",
        "max_agent_steps",
        "max_agent_steps INTEGER",
    )?;
    ensure_column(conn, "decisions", "max_retries", "max_retries INTEGER")?;
    ensure_column(
        conn,
        "decisions",
        "entities_json",
        "entities_json TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(
        conn,
        "reservations",
        "allocation_spends_json",
        "allocation_spends_json TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(
        conn,
        "budget_allocation_buckets",
        "started_at",
        "started_at TEXT",
    )?;
    ensure_column(
        conn,
        "budget_allocation_buckets",
        "protected_amount_usd",
        "protected_amount_usd REAL NOT NULL DEFAULT 0",
    )?;
    backfill_decision_app_run_keys(conn)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_decisions_app_run_key_created ON decisions(app_run_key, created_at)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_reservation ON reservation_limit_scopes(reservation_id)",
        [],
    )?;
    backfill_reservation_limit_scope_rollups(conn)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_rolling ON reservation_limit_scopes(rule_id, limit_id, scope_key, created_at)",
        [],
    )?;
    backfill_rolling_spend_buckets(conn)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_reservations_decision ON reservations(decision_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_reservations_active ON reservations(status) WHERE status = 'active'",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_decisions_created_decision ON decisions(created_at, decision_id)",
        [],
    )?;
    Ok(())
}

fn init_postgres_schema(conn: &mut PostgresClient) -> Result<(), NoetError> {
    conn.batch_execute("BEGIN")?;
    let result = conn.batch_execute(
        "
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version BIGINT PRIMARY KEY,
            applied_at TEXT NOT NULL
        );
        INSERT INTO schema_migrations (version, applied_at)
        VALUES (1, now()::text)
        ON CONFLICT (version) DO NOTHING;

        CREATE TABLE IF NOT EXISTS decisions (
            decision_id TEXT PRIMARY KEY,
            trace_id TEXT,
            session_id TEXT,
            request_id TEXT,
            subject TEXT,
            project TEXT,
            provider TEXT,
            model TEXT,
            estimated_tokens BIGINT,
            estimated_cost_usd DOUBLE PRECISION,
            outcome TEXT NOT NULL,
            action TEXT NOT NULL DEFAULT 'allow',
            explanations_json TEXT NOT NULL,
            metadata_json TEXT NOT NULL,
            entities_json TEXT NOT NULL DEFAULT '[]',
            selected_budget_id TEXT,
            matched_entity TEXT,
            selection_reason TEXT,
            rejected_budget_id TEXT,
            rejected_budget_reason TEXT,
            model_check TEXT,
            budget_window_remaining_usd DOUBLE PRECISION,
            routing_json TEXT,
            limit_hits_json TEXT,
            max_tool_calls BIGINT,
            max_agent_steps BIGINT,
            max_retries BIGINT,
            app_run_key TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_decisions_trace ON decisions(trace_id);
        CREATE INDEX IF NOT EXISTS idx_decisions_created ON decisions(created_at);
        CREATE INDEX IF NOT EXISTS idx_decisions_app_run_key_created ON decisions(app_run_key, created_at);
        CREATE INDEX IF NOT EXISTS idx_decisions_created_decision ON decisions(created_at, decision_id);

        CREATE TABLE IF NOT EXISTS reservations (
            id TEXT PRIMARY KEY,
            decision_id TEXT NOT NULL REFERENCES decisions(decision_id),
            amount_usd DOUBLE PRECISION NOT NULL,
            estimated_amount_usd DOUBLE PRECISION NOT NULL,
            actual_amount_usd DOUBLE PRECISION,
            currency TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            finalized_at TEXT,
            budget_rule_ids_json TEXT NOT NULL DEFAULT '[]',
            limit_window_spends_json TEXT NOT NULL DEFAULT '[]',
            allocation_spends_json TEXT NOT NULL DEFAULT '[]'
        );
        CREATE INDEX IF NOT EXISTS idx_reservations_decision ON reservations(decision_id);
        CREATE INDEX IF NOT EXISTS idx_reservations_active
            ON reservations(status)
            WHERE status = 'active';

        CREATE TABLE IF NOT EXISTS reservation_limit_scopes (
            reservation_id TEXT NOT NULL REFERENCES reservations(id),
            rule_id TEXT NOT NULL,
            limit_id TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            amount_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
            created_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_lookup
            ON reservation_limit_scopes(rule_id, limit_id, scope_key);
        CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_reservation
            ON reservation_limit_scopes(reservation_id);
        CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_rolling
            ON reservation_limit_scopes(rule_id, limit_id, scope_key, created_at);

        CREATE TABLE IF NOT EXISTS rolling_spend_buckets (
            rule_id TEXT NOT NULL,
            limit_id TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            bucket_start TEXT NOT NULL,
            amount_usd DOUBLE PRECISION NOT NULL,
            PRIMARY KEY (rule_id, limit_id, scope_key, bucket_start)
        );

        CREATE TABLE IF NOT EXISTS usage_observations (
            id TEXT PRIMARY KEY,
            reservation_id TEXT REFERENCES reservations(id),
            trace_id TEXT,
            provider TEXT,
            model TEXT,
            input_tokens BIGINT,
            output_tokens BIGINT,
            total_tokens BIGINT,
            cost_usd DOUBLE PRECISION,
            latency_ms BIGINT,
            stop_reason TEXT,
            source TEXT,
            metadata_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_usage_trace ON usage_observations(trace_id);

        CREATE TABLE IF NOT EXISTS events (
            id TEXT PRIMARY KEY,
            trace_id TEXT,
            kind TEXT NOT NULL,
            occurred_at TEXT NOT NULL,
            source TEXT,
            payload_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_events_trace ON events(trace_id);
        CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);

        CREATE TABLE IF NOT EXISTS limit_window_states (
            rule_id TEXT NOT NULL,
            limit_id TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            started_at TEXT NOT NULL,
            used_usd DOUBLE PRECISION NOT NULL,
            PRIMARY KEY (rule_id, limit_id, scope_key)
        );

        CREATE TABLE IF NOT EXISTS budget_allocation_buckets (
            rule_id TEXT NOT NULL,
            entity_key TEXT NOT NULL,
            started_at TEXT,
            protected_amount_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
            current_grant_usd DOUBLE PRECISION NOT NULL,
            carryover_usd DOUBLE PRECISION NOT NULL,
            PRIMARY KEY (rule_id, entity_key)
        );

        CREATE TABLE IF NOT EXISTS advisory_delivery_state (
            user_key TEXT NOT NULL,
            advisory_key TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            last_shown_at TEXT NOT NULL,
            PRIMARY KEY (user_key, advisory_key, scope_key)
        );
        ",
    );
    match result {
        Ok(()) => {
            conn.batch_execute("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.batch_execute("ROLLBACK");
            Err(error.into())
        }
    }
}

async fn init_postgres_schema_async(conn: &AsyncPostgresClient) -> Result<(), NoetError> {
    conn.batch_execute(
        "
        BEGIN;

        CREATE TABLE IF NOT EXISTS schema_migrations (
            version BIGINT PRIMARY KEY,
            applied_at TEXT NOT NULL
        );
        INSERT INTO schema_migrations (version, applied_at)
        VALUES (1, now()::text)
        ON CONFLICT (version) DO NOTHING;

        CREATE TABLE IF NOT EXISTS decisions (
            decision_id TEXT PRIMARY KEY,
            trace_id TEXT,
            session_id TEXT,
            request_id TEXT,
            subject TEXT,
            project TEXT,
            provider TEXT,
            model TEXT,
            estimated_tokens BIGINT,
            estimated_cost_usd DOUBLE PRECISION,
            outcome TEXT NOT NULL,
            action TEXT NOT NULL DEFAULT 'allow',
            explanations_json TEXT NOT NULL,
            metadata_json TEXT NOT NULL,
            entities_json TEXT NOT NULL DEFAULT '[]',
            selected_budget_id TEXT,
            matched_entity TEXT,
            selection_reason TEXT,
            rejected_budget_id TEXT,
            rejected_budget_reason TEXT,
            model_check TEXT,
            budget_window_remaining_usd DOUBLE PRECISION,
            routing_json TEXT,
            limit_hits_json TEXT,
            max_tool_calls BIGINT,
            max_agent_steps BIGINT,
            max_retries BIGINT,
            app_run_key TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_decisions_trace ON decisions(trace_id);
        CREATE INDEX IF NOT EXISTS idx_decisions_created ON decisions(created_at);
        CREATE INDEX IF NOT EXISTS idx_decisions_app_run_key_created ON decisions(app_run_key, created_at);
        CREATE INDEX IF NOT EXISTS idx_decisions_created_decision ON decisions(created_at, decision_id);

        CREATE TABLE IF NOT EXISTS reservations (
            id TEXT PRIMARY KEY,
            decision_id TEXT NOT NULL REFERENCES decisions(decision_id),
            amount_usd DOUBLE PRECISION NOT NULL,
            estimated_amount_usd DOUBLE PRECISION NOT NULL,
            actual_amount_usd DOUBLE PRECISION,
            currency TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            finalized_at TEXT,
            budget_rule_ids_json TEXT NOT NULL DEFAULT '[]',
            limit_window_spends_json TEXT NOT NULL DEFAULT '[]',
            allocation_spends_json TEXT NOT NULL DEFAULT '[]'
        );
        CREATE INDEX IF NOT EXISTS idx_reservations_decision ON reservations(decision_id);
        CREATE INDEX IF NOT EXISTS idx_reservations_active
            ON reservations(status)
            WHERE status = 'active';

        CREATE TABLE IF NOT EXISTS reservation_limit_scopes (
            reservation_id TEXT NOT NULL REFERENCES reservations(id),
            rule_id TEXT NOT NULL,
            limit_id TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            amount_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
            created_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_lookup
            ON reservation_limit_scopes(rule_id, limit_id, scope_key);
        CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_reservation
            ON reservation_limit_scopes(reservation_id);
        CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_rolling
            ON reservation_limit_scopes(rule_id, limit_id, scope_key, created_at);

        CREATE TABLE IF NOT EXISTS rolling_spend_buckets (
            rule_id TEXT NOT NULL,
            limit_id TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            bucket_start TEXT NOT NULL,
            amount_usd DOUBLE PRECISION NOT NULL,
            PRIMARY KEY (rule_id, limit_id, scope_key, bucket_start)
        );

        CREATE TABLE IF NOT EXISTS usage_observations (
            id TEXT PRIMARY KEY,
            reservation_id TEXT REFERENCES reservations(id),
            trace_id TEXT,
            provider TEXT,
            model TEXT,
            input_tokens BIGINT,
            output_tokens BIGINT,
            total_tokens BIGINT,
            cost_usd DOUBLE PRECISION,
            latency_ms BIGINT,
            stop_reason TEXT,
            source TEXT,
            metadata_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_usage_trace ON usage_observations(trace_id);

        CREATE TABLE IF NOT EXISTS events (
            id TEXT PRIMARY KEY,
            trace_id TEXT,
            kind TEXT NOT NULL,
            occurred_at TEXT NOT NULL,
            source TEXT,
            payload_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_events_trace ON events(trace_id);
        CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);

        CREATE TABLE IF NOT EXISTS limit_window_states (
            rule_id TEXT NOT NULL,
            limit_id TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            started_at TEXT NOT NULL,
            used_usd DOUBLE PRECISION NOT NULL,
            PRIMARY KEY (rule_id, limit_id, scope_key)
        );

        CREATE TABLE IF NOT EXISTS budget_allocation_buckets (
            rule_id TEXT NOT NULL,
            entity_key TEXT NOT NULL,
            started_at TEXT,
            protected_amount_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
            current_grant_usd DOUBLE PRECISION NOT NULL,
            carryover_usd DOUBLE PRECISION NOT NULL,
            PRIMARY KEY (rule_id, entity_key)
        );

        CREATE TABLE IF NOT EXISTS advisory_delivery_state (
            user_key TEXT NOT NULL,
            advisory_key TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            last_shown_at TEXT NOT NULL,
            PRIMARY KEY (user_key, advisory_key, scope_key)
        );

        COMMIT;
        ",
    )
    .await?;
    Ok(())
}

async fn load_limit_windows_async(
    conn: &(impl GenericClient + Sync),
    ledger: &mut BudgetLedger,
) -> Result<(), NoetError> {
    let rows = conn
        .query(
            "
            SELECT rule_id, limit_id, scope_key, started_at, used_usd
            FROM limit_window_states
            ",
            &[],
        )
        .await?;
    ledger.limit_windows = rows
        .into_iter()
        .map(|row| {
            (
                (row.get(0), row.get(1), row.get(2)),
                WindowState {
                    started_at: parse_time(row.get::<_, String>(3)),
                    used_usd: row.get(4),
                },
            )
        })
        .collect();
    Ok(())
}

async fn load_allocation_buckets_async(
    conn: &(impl GenericClient + Sync),
    ledger: &mut BudgetLedger,
) -> Result<(), NoetError> {
    let rows = conn
        .query(
            "
            SELECT rule_id, entity_key, started_at, protected_amount_usd, current_grant_usd, carryover_usd
            FROM budget_allocation_buckets
            ",
            &[],
        )
        .await?;
    ledger.allocation_buckets = rows
        .into_iter()
        .map(|row| {
            let rule_id: String = row.get(0);
            let entity_key: String = row.get(1);
            let started_at: Option<String> = row.get(2);
            (
                (rule_id, entity_key),
                AllocationBucketState {
                    started_at: started_at.map(parse_time).unwrap_or_else(Utc::now),
                    protected_amount_usd: row.get(3),
                    current_grant_usd: row.get(4),
                    carryover_usd: row.get(5),
                },
            )
        })
        .collect();
    Ok(())
}

async fn load_advisory_cadence_async(
    conn: &(impl GenericClient + Sync),
    ledger: &mut BudgetLedger,
) -> Result<(), NoetError> {
    let rows = conn
        .query(
            "
            SELECT user_key, advisory_key, scope_key, last_shown_at
            FROM advisory_delivery_state
            ",
            &[],
        )
        .await?;
    ledger.advisory_cadence = rows
        .into_iter()
        .map(|row| {
            (
                (row.get(0), row.get(1), row.get(2)),
                parse_time(row.get::<_, String>(3)),
            )
        })
        .collect();
    Ok(())
}

async fn load_async_authorization_state(
    conn: &(impl GenericClient + Sync),
    ledger: &mut BudgetLedger,
    policy: Option<&PolicyFile>,
    now: DateTime<Utc>,
    pending_finalizations: Option<&HashSet<String>>,
) -> Result<(), NoetError> {
    load_limit_windows_async(conn, ledger).await?;
    load_allocation_buckets_async(conn, ledger).await?;
    load_rolling_spend_buckets_async(conn, ledger, policy, now).await?;
    load_active_reservations_async(conn, ledger, pending_finalizations).await?;
    load_advisory_cadence_async(conn, ledger).await
}

async fn load_async_finalization_state(
    conn: &(impl GenericClient + Sync),
    ledger: &mut BudgetLedger,
    pending_finalizations: Option<&HashSet<String>>,
) -> Result<(), NoetError> {
    load_limit_windows_async(conn, ledger).await?;
    load_allocation_buckets_async(conn, ledger).await?;
    load_active_reservations_async(conn, ledger, pending_finalizations).await
}

async fn load_rolling_spend_buckets_async(
    conn: &(impl GenericClient + Sync),
    ledger: &mut BudgetLedger,
    policy: Option<&PolicyFile>,
    now: DateTime<Utc>,
) -> Result<(), NoetError> {
    let Some(duration) = policy.and_then(biggest_policy_rolling_spend_window_duration) else {
        ledger.rolling_spend_buckets.clear();
        return Ok(());
    };
    let since = rolling_bucket_start(now - duration).to_rfc3339();
    let until = rolling_bucket_start(now).to_rfc3339();
    let rows = conn
        .query(
            "
            SELECT rule_id, limit_id, scope_key, bucket_start, amount_usd
            FROM rolling_spend_buckets
            WHERE bucket_start >= $1 AND bucket_start <= $2
            ",
            &[&since, &until],
        )
        .await?;
    ledger.rolling_spend_buckets = rows
        .into_iter()
        .map(|row| {
            (
                (
                    row.get(0),
                    row.get(1),
                    row.get(2),
                    parse_time(row.get::<_, String>(3)),
                ),
                row.get(4),
            )
        })
        .collect();
    Ok(())
}

async fn load_reservation_async(
    conn: &(impl GenericClient + Sync),
    ledger: &mut BudgetLedger,
    reservation_id: &str,
) -> Result<(), NoetError> {
    if let Some(row) = conn
        .query_opt(
            "
            SELECT id, amount_usd, estimated_amount_usd, currency, status, created_at, expires_at,
                   budget_rule_ids_json, limit_window_spends_json, allocation_spends_json
            FROM reservations
            WHERE id = $1
            ",
            &[&reservation_id],
        )
        .await?
    {
        let (id, reservation) = stored_reservation_from_async_row(&row);
        ledger.reservations.insert(id, reservation);
    }
    Ok(())
}

async fn load_active_reservations_async(
    conn: &(impl GenericClient + Sync),
    ledger: &mut BudgetLedger,
    pending_finalizations: Option<&HashSet<String>>,
) -> Result<(), NoetError> {
    let local_finalized = pending_finalizations
        .map(|pending_finalizations| {
            ledger
                .reservations
                .iter()
                .filter(|(id, stored)| {
                    pending_finalizations.contains(*id)
                        && stored.reservation.status != ReservationStatus::Active
                })
                .map(|(id, stored)| (id.clone(), stored.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let rows = conn
        .query(
            "
            SELECT id, amount_usd, estimated_amount_usd, currency, status, created_at, expires_at,
                   budget_rule_ids_json, limit_window_spends_json, allocation_spends_json
            FROM reservations
            WHERE status = 'active'
            ",
            &[],
        )
        .await?;
    let mut reservations: HashMap<String, StoredReservation> = rows
        .into_iter()
        .map(|row| stored_reservation_from_async_row(&row))
        .collect();
    for (id, stored) in local_finalized {
        reservations.insert(id, stored);
    }
    ledger.reservations = reservations;
    Ok(())
}

fn stored_reservation_from_async_row(row: &AsyncPostgresRow) -> (String, StoredReservation) {
    let id: String = row.get(0);
    let status: String = row.get(4);
    let budget_rule_ids_json: String = row.get(7);
    let limit_window_spends_json: String = row.get(8);
    let allocation_spends_json: String = row.get(9);
    (
        id.clone(),
        StoredReservation {
            reservation: Reservation {
                id,
                amount_usd: row.get(1),
                currency: row.get(3),
                status: parse_reservation_status(&status),
                created_at: parse_time(row.get::<_, String>(5)),
                expires_at: parse_time(row.get::<_, String>(6)),
            },
            estimated_cost_usd: row.get(2),
            budget_rule_ids: serde_json::from_str(&budget_rule_ids_json).unwrap_or_default(),
            limit_window_spends: serde_json::from_str(&limit_window_spends_json)
                .unwrap_or_default(),
            allocation_spends: serde_json::from_str(&allocation_spends_json).unwrap_or_default(),
        },
    )
}

async fn persist_limit_windows_async(
    conn: &(impl GenericClient + Sync),
    snapshot: &LedgerStateSnapshot,
) -> Result<(), NoetError> {
    for ((rule_id, limit_id, scope_key), window) in &snapshot.limit_windows {
        conn.execute(
            "
            INSERT INTO limit_window_states (rule_id, limit_id, scope_key, started_at, used_usd)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
                started_at = EXCLUDED.started_at,
                used_usd = EXCLUDED.used_usd
            ",
            &[
                rule_id,
                limit_id,
                scope_key,
                &window.started_at.to_rfc3339(),
                &window.used_usd,
            ],
        )
        .await?;
    }
    Ok(())
}

async fn persist_allocation_buckets_async(
    conn: &(impl GenericClient + Sync),
    snapshot: &LedgerStateSnapshot,
) -> Result<(), NoetError> {
    for ((rule_id, entity_key), bucket) in &snapshot.allocation_buckets {
        conn.execute(
            "
            INSERT INTO budget_allocation_buckets (
                rule_id, entity_key, started_at, protected_amount_usd, current_grant_usd, carryover_usd
            ) VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT(rule_id, entity_key) DO UPDATE SET
                started_at = EXCLUDED.started_at,
                protected_amount_usd = EXCLUDED.protected_amount_usd,
                current_grant_usd = EXCLUDED.current_grant_usd,
                carryover_usd = EXCLUDED.carryover_usd
            ",
            &[
                rule_id,
                entity_key,
                &bucket.started_at.to_rfc3339(),
                &bucket.protected_amount_usd,
                &bucket.current_grant_usd,
                &bucket.carryover_usd,
            ],
        )
        .await?;
    }
    Ok(())
}

async fn persist_decision_async(
    conn: &(impl GenericClient + Sync),
    snapshot: &LedgerStateSnapshot,
    policy: Option<&PolicyFile>,
    request: &AuthorizeRequest,
    decision: &AuthorizeDecision,
) -> Result<(), NoetError> {
    let trace_id = string_metadata(request, "trace_id");
    let session_id = string_metadata(request, "session_id");
    let request_id = string_metadata(request, "request_id");
    let routing = snapshot.routing_persistence_fields(policy, request, decision);
    let outcome = outcome_text(decision.outcome);
    let app_run_key = decision_app_run_key(
        trace_id.as_deref(),
        request.provider.as_deref(),
        request.model.as_deref(),
        outcome,
        routing.selected_budget_id.as_deref(),
        &request.metadata,
        decision.created_at,
    );
    let routing_report = decision_routing_report(&routing);
    let estimated_tokens = request.estimated_tokens.map(|value| value as i64);
    let explanations_json = serde_json::to_string(&decision.explanations)?;
    let metadata_json = serde_json::to_string(&request.metadata)?;
    let entities_json = serde_json::to_string(&request.entities)?;
    let routing_json = serde_json::to_string(&routing_report)?;
    let limit_hits_json = serde_json::to_string(&snapshot.limit_hits)?;
    let max_tool_calls = routing.tool_calls.map(|value| value as i64);
    let max_agent_steps = routing.agent_steps.map(|value| value as i64);
    let max_retries = routing.retries.map(|value| value as i64);
    let created_at = decision.created_at.to_rfc3339();

    if let Some(reservation) = &decision.reservation
        && let Some(stored) = snapshot.reservations.get(&reservation.id)
        && stored.limit_window_spends.len() == 1
        && stored.allocation_spends.is_empty()
    {
        let spend = &stored.limit_window_spends[0];
        if let Some(window) = snapshot.limit_windows.get(&(
            spend.rule_id.clone(),
            spend.limit_id.clone(),
            spend.scope_key.clone(),
        )) {
            let budget_rule_ids_json = serde_json::to_string(&stored.budget_rule_ids)?;
            let limit_window_spends_json = serde_json::to_string(&stored.limit_window_spends)?;
            let allocation_spends_json = serde_json::to_string(&stored.allocation_spends)?;
            let reservation_created_at = reservation.created_at.to_rfc3339();
            let reservation_expires_at = reservation.expires_at.to_rfc3339();
            let window_started_at = window.started_at.to_rfc3339();
            let bucket_start = rolling_bucket_start(reservation.created_at).to_rfc3339();
            conn.execute(
                ASYNC_AUTHORIZE_FAST_SQL,
                &[
                    &decision.decision_id.as_str(),
                    &trace_id.as_deref(),
                    &session_id.as_deref(),
                    &request_id.as_deref(),
                    &request.subject.as_deref(),
                    &request.project.as_deref(),
                    &request.provider.as_deref(),
                    &request.model.as_deref(),
                    &estimated_tokens,
                    &request.estimated_cost_usd,
                    &outcome,
                    &action_text(decision.action),
                    &explanations_json,
                    &metadata_json,
                    &entities_json,
                    &routing.selected_budget_id.as_deref(),
                    &routing.matched_entity.as_deref(),
                    &routing.selection_reason.as_deref(),
                    &routing.rejected_budget_id.as_deref(),
                    &routing.rejected_budget_reason.as_deref(),
                    &routing.model_check.as_deref(),
                    &routing.budget_window_remaining_usd,
                    &routing_json,
                    &limit_hits_json,
                    &max_tool_calls,
                    &max_agent_steps,
                    &max_retries,
                    &app_run_key,
                    &created_at,
                    &spend.rule_id.as_str(),
                    &spend.limit_id.as_str(),
                    &spend.scope_key.as_str(),
                    &window_started_at,
                    &window.used_usd,
                    &reservation.id.as_str(),
                    &reservation.amount_usd,
                    &reservation.currency.as_str(),
                    &reservation_status_text(reservation.status),
                    &reservation_created_at,
                    &reservation_expires_at,
                    &budget_rule_ids_json,
                    &limit_window_spends_json,
                    &allocation_spends_json,
                    &bucket_start,
                ],
            )
            .await?;
            persist_advisory_cadence_async(conn, &snapshot.pending_advisory_cadence).await?;
            return Ok(());
        }
    }

    persist_limit_windows_async(conn, snapshot).await?;
    persist_allocation_buckets_async(conn, snapshot).await?;

    conn.execute(
        "
        INSERT INTO decisions (
            decision_id, trace_id, session_id, request_id, subject, project, provider, model,
            estimated_tokens, estimated_cost_usd, outcome, action, explanations_json, metadata_json,
            entities_json, selected_budget_id, matched_entity, selection_reason, rejected_budget_id,
            rejected_budget_reason, model_check, budget_window_remaining_usd, routing_json,
            limit_hits_json, max_tool_calls, max_agent_steps, max_retries, app_run_key, created_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
            $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29
        )
        ",
        &[
            &decision.decision_id.as_str(),
            &trace_id.as_deref(),
            &session_id.as_deref(),
            &request_id.as_deref(),
            &request.subject.as_deref(),
            &request.project.as_deref(),
            &request.provider.as_deref(),
            &request.model.as_deref(),
            &estimated_tokens,
            &request.estimated_cost_usd,
            &outcome,
            &action_text(decision.action),
            &explanations_json,
            &metadata_json,
            &entities_json,
            &routing.selected_budget_id.as_deref(),
            &routing.matched_entity.as_deref(),
            &routing.selection_reason.as_deref(),
            &routing.rejected_budget_id.as_deref(),
            &routing.rejected_budget_reason.as_deref(),
            &routing.model_check.as_deref(),
            &routing.budget_window_remaining_usd,
            &routing_json,
            &limit_hits_json,
            &max_tool_calls,
            &max_agent_steps,
            &max_retries,
            &app_run_key,
            &created_at,
        ],
    )
    .await?;

    persist_advisory_cadence_async(conn, &snapshot.pending_advisory_cadence).await?;

    if let Some(reservation) = &decision.reservation {
        let limit_window_spends = snapshot
            .reservations
            .get(&reservation.id)
            .map(|stored| stored.limit_window_spends.as_slice())
            .unwrap_or_default();
        let budget_rule_ids = snapshot
            .reservations
            .get(&reservation.id)
            .map(|stored| stored.budget_rule_ids.as_slice())
            .unwrap_or_default();
        let budget_rule_ids_json = serde_json::to_string(budget_rule_ids)?;
        let limit_window_spends_json = serde_json::to_string(&limit_window_spends)?;
        let allocation_spends_json = serde_json::to_string(
            &snapshot
                .reservations
                .get(&reservation.id)
                .map(|stored| stored.allocation_spends.as_slice())
                .unwrap_or_default(),
        )?;
        let reservation_created_at = reservation.created_at.to_rfc3339();
        let reservation_expires_at = reservation.expires_at.to_rfc3339();
        conn.execute(
            "
            INSERT INTO reservations (
                id, decision_id, amount_usd, estimated_amount_usd, currency, status,
                created_at, expires_at, budget_rule_ids_json, limit_window_spends_json,
                allocation_spends_json
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ",
            &[
                &reservation.id.as_str(),
                &decision.decision_id.as_str(),
                &reservation.amount_usd,
                &reservation.amount_usd,
                &reservation.currency.as_str(),
                &reservation_status_text(reservation.status),
                &reservation_created_at,
                &reservation_expires_at,
                &budget_rule_ids_json,
                &limit_window_spends_json,
                &allocation_spends_json,
            ],
        )
        .await?;
        for spend in limit_window_spends {
            conn.execute(
                "
                INSERT INTO reservation_limit_scopes (
                    reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at
                ) VALUES ($1, $2, $3, $4, $5, $6)
                ",
                &[
                    &reservation.id.as_str(),
                    &spend.rule_id.as_str(),
                    &spend.limit_id.as_str(),
                    &spend.scope_key.as_str(),
                    &reservation.amount_usd,
                    &reservation_created_at,
                ],
            )
            .await?;
            let bucket_start = rolling_bucket_start(reservation.created_at).to_rfc3339();
            conn.execute(
                "
                INSERT INTO rolling_spend_buckets (
                    rule_id, limit_id, scope_key, bucket_start, amount_usd
                ) VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
                    amount_usd = rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd
                ",
                &[
                    &spend.rule_id.as_str(),
                    &spend.limit_id.as_str(),
                    &spend.scope_key.as_str(),
                    &bucket_start,
                    &reservation.amount_usd,
                ],
            )
            .await?;
        }
    }
    Ok(())
}

fn persist_advisory_cadence_postgres(
    tx: &mut postgres::Transaction<'_>,
    pending: &HashMap<(String, String, String), DateTime<Utc>>,
) -> Result<(), NoetError> {
    for ((user_key, advisory_key, scope_key), shown_at) in pending {
        let shown_at = shown_at.to_rfc3339();
        tx.execute(
            "
            INSERT INTO advisory_delivery_state (
                user_key, advisory_key, scope_key, last_shown_at
            ) VALUES ($1, $2, $3, $4)
            ON CONFLICT(user_key, advisory_key, scope_key) DO UPDATE SET
                last_shown_at = EXCLUDED.last_shown_at
            ",
            &[
                &user_key.as_str(),
                &advisory_key.as_str(),
                &scope_key.as_str(),
                &shown_at,
            ],
        )?;
    }
    Ok(())
}

fn persist_advisory_cadence_sqlite(
    conn: &Connection,
    pending: &HashMap<(String, String, String), DateTime<Utc>>,
) -> Result<(), NoetError> {
    for ((user_key, advisory_key, scope_key), shown_at) in pending {
        conn.execute(
            "
            INSERT INTO advisory_delivery_state (
                user_key, advisory_key, scope_key, last_shown_at
            ) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(user_key, advisory_key, scope_key) DO UPDATE SET
                last_shown_at = excluded.last_shown_at
            ",
            params![
                user_key.as_str(),
                advisory_key.as_str(),
                scope_key.as_str(),
                shown_at.to_rfc3339()
            ],
        )?;
    }
    Ok(())
}

async fn persist_advisory_cadence_async(
    conn: &(impl GenericClient + Sync),
    pending: &HashMap<(String, String, String), DateTime<Utc>>,
) -> Result<(), NoetError> {
    for ((user_key, advisory_key, scope_key), shown_at) in pending {
        let shown_at = shown_at.to_rfc3339();
        conn.execute(
            "
            INSERT INTO advisory_delivery_state (
                user_key, advisory_key, scope_key, last_shown_at
            ) VALUES ($1, $2, $3, $4)
            ON CONFLICT(user_key, advisory_key, scope_key) DO UPDATE SET
                last_shown_at = EXCLUDED.last_shown_at
            ",
            &[
                &user_key.as_str(),
                &advisory_key.as_str(),
                &scope_key.as_str(),
                &shown_at,
            ],
        )
        .await?;
    }
    Ok(())
}

async fn persist_finalization_write_async(
    conn: &(impl GenericClient + Sync),
    statements: &AsyncPostgresStatements,
    write: &AsyncFinalizeWrite,
) -> Result<(), NoetError> {
    let persisted_limit_window = persist_finalization_async(
        conn,
        statements,
        &write.reservation,
        &write.payload,
        &write.snapshot,
    )
    .await?;
    apply_finalization_spend_deltas_async(
        conn,
        &write.reservation,
        &write.snapshot,
        !persisted_limit_window,
    )
    .await?;
    Ok(())
}

async fn persist_finalization_async(
    conn: &(impl GenericClient + Sync),
    statements: &AsyncPostgresStatements,
    reservation: &Reservation,
    payload: &FinalizeReservation,
    snapshot: &LedgerStateSnapshot,
) -> Result<bool, NoetError> {
    let now = Utc::now().to_rfc3339();
    if let Some(stored) = snapshot.reservations.get(&reservation.id)
        && stored.limit_window_spends.len() == 1
    {
        let delta = finalization_amount_delta(reservation, stored);
        let spend = &stored.limit_window_spends[0];
        let window_key = (
            spend.rule_id.clone(),
            spend.limit_id.clone(),
            spend.scope_key.clone(),
        );
        let Some(window) = snapshot.limit_windows.get(&window_key) else {
            return Ok(false);
        };
        let window_started_at = window.started_at.to_rfc3339();
        if let Some(usage) = &payload.usage {
            let trace_id = string_value(&payload.metadata, "trace_id");
            let input_tokens = usage.input_tokens.map(|value| value as i64);
            let output_tokens = usage.output_tokens.map(|value| value as i64);
            let total_tokens = usage.total_tokens.map(|value| value as i64);
            let latency_ms = usage.latency_ms.map(|value| value as i64);
            let cost_usd = usage.cost_usd.or(Some(reservation.amount_usd));
            let metadata_json = serde_json::to_string(&payload.metadata)?;
            let usage_id = Uuid::new_v4().to_string();
            conn.execute(
                &statements.finalize_with_usage_fast,
                &[
                    &reservation.id.as_str(),
                    &reservation.amount_usd,
                    &reservation_status_text(reservation.status),
                    &now,
                    &usage_id,
                    &trace_id.as_deref(),
                    &usage.provider.as_deref(),
                    &usage.model.as_deref(),
                    &input_tokens,
                    &output_tokens,
                    &total_tokens,
                    &cost_usd,
                    &latency_ms,
                    &usage.stop_reason.as_deref(),
                    &metadata_json,
                    &spend.rule_id.as_str(),
                    &spend.limit_id.as_str(),
                    &spend.scope_key.as_str(),
                    &window_started_at,
                    &delta,
                ],
            )
            .await?;
            return Ok(true);
        }
        conn.execute(
            &statements.finalize_without_usage_fast,
            &[
                &reservation.id.as_str(),
                &reservation.amount_usd,
                &reservation_status_text(reservation.status),
                &now,
                &spend.rule_id.as_str(),
                &spend.limit_id.as_str(),
                &spend.scope_key.as_str(),
                &window_started_at,
                &delta,
            ],
        )
        .await?;
        return Ok(true);
    }

    conn.execute(
        "
        UPDATE reservations
        SET amount_usd = $2, actual_amount_usd = $2, status = $3, finalized_at = $4
        WHERE id = $1
        ",
        &[
            &reservation.id.as_str(),
            &reservation.amount_usd,
            &reservation_status_text(reservation.status),
            &now,
        ],
    )
    .await?;
    if let Some(usage) = &payload.usage {
        let decision_trace_id = conn
            .query_opt(
                "
                SELECT d.trace_id
                FROM reservations r
                JOIN decisions d ON d.decision_id = r.decision_id
                WHERE r.id = $1
                ",
                &[&reservation.id.as_str()],
            )
            .await?
            .and_then(|row| row.get::<_, Option<String>>(0));
        let trace_id = decision_trace_id.or_else(|| string_value(&payload.metadata, "trace_id"));
        let input_tokens = usage.input_tokens.map(|value| value as i64);
        let output_tokens = usage.output_tokens.map(|value| value as i64);
        let total_tokens = usage.total_tokens.map(|value| value as i64);
        let latency_ms = usage.latency_ms.map(|value| value as i64);
        let metadata_json = serde_json::to_string(&payload.metadata)?;
        conn.execute(
            "
            INSERT INTO usage_observations (
                id, reservation_id, trace_id, provider, model, input_tokens, output_tokens,
                total_tokens, cost_usd, latency_ms, stop_reason, source, metadata_json,
                created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            ",
            &[
                &Uuid::new_v4().to_string(),
                &reservation.id.as_str(),
                &trace_id.as_deref(),
                &usage.provider.as_deref(),
                &usage.model.as_deref(),
                &input_tokens,
                &output_tokens,
                &total_tokens,
                &usage.cost_usd.or(Some(reservation.amount_usd)),
                &latency_ms,
                &usage.stop_reason.as_deref(),
                &"reservation.finalize",
                &metadata_json,
                &now,
            ],
        )
        .await?;
    }
    Ok(false)
}

async fn apply_finalization_spend_deltas_async(
    conn: &(impl GenericClient + Sync),
    reservation: &Reservation,
    snapshot: &LedgerStateSnapshot,
    apply_limit_windows: bool,
) -> Result<(), NoetError> {
    let Some(stored) = snapshot.reservations.get(&reservation.id) else {
        return Ok(());
    };
    let delta = finalization_amount_delta(reservation, stored);
    if delta == 0.0 {
        return Ok(());
    }
    for spend in &stored.limit_window_spends {
        let bucket_start = rolling_bucket_start(stored.reservation.created_at).to_rfc3339();
        conn.execute(
            "
            INSERT INTO rolling_spend_buckets (
                rule_id, limit_id, scope_key, bucket_start, amount_usd
            ) VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
                amount_usd = GREATEST(rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd, 0)
            ",
            &[
                &spend.rule_id.as_str(),
                &spend.limit_id.as_str(),
                &spend.scope_key.as_str(),
                &bucket_start,
                &delta,
            ],
        )
        .await?;
        if !apply_limit_windows {
            continue;
        }
        let window_key = (
            spend.rule_id.clone(),
            spend.limit_id.clone(),
            spend.scope_key.clone(),
        );
        let Some(window) = snapshot.limit_windows.get(&window_key) else {
            continue;
        };
        let window_started_at = window.started_at.to_rfc3339();
        conn.execute(
            "
            INSERT INTO limit_window_states (
                rule_id, limit_id, scope_key, started_at, used_usd
            ) VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
                used_usd = CASE
                    WHEN limit_window_states.started_at = EXCLUDED.started_at
                    THEN GREATEST(limit_window_states.used_usd + EXCLUDED.used_usd, 0)
                    ELSE limit_window_states.used_usd
                END
            ",
            &[
                &spend.rule_id.as_str(),
                &spend.limit_id.as_str(),
                &spend.scope_key.as_str(),
                &window_started_at,
                &delta,
            ],
        )
        .await?;
    }
    Ok(())
}

fn finalization_amount_delta(reservation: &Reservation, stored: &StoredReservation) -> f64 {
    reservation.amount_usd - stored.estimated_cost_usd
}

async fn persist_event_async(
    conn: &AsyncPostgresClient,
    event: &TraceEvent,
) -> Result<(), NoetError> {
    let occurred_at = event.occurred_at.unwrap_or_else(Utc::now);
    let source = event
        .payload
        .as_object()
        .and_then(|payload| payload.get("source"))
        .and_then(|value| value.as_str());
    let id = event
        .id
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let payload_json = serde_json::to_string(&event.payload)?;
    conn.execute(
        "
        INSERT INTO events (id, trace_id, kind, occurred_at, source, payload_json)
        VALUES ($1, $2, $3, $4, $5, $6)
        ",
        &[
            &id,
            &event.trace_id.as_deref(),
            &event.kind.as_str(),
            &occurred_at.to_rfc3339(),
            &source,
            &payload_json,
        ],
    )
    .await?;
    Ok(())
}

fn backfill_reservation_limit_scope_rollups(conn: &Connection) -> Result<(), NoetError> {
    conn.execute(
        "
        UPDATE reservation_limit_scopes
        SET amount_usd = (
                SELECT COALESCE(r.amount_usd, 0)
                FROM reservations r
                WHERE r.id = reservation_limit_scopes.reservation_id
            ),
            created_at = (
                SELECT r.created_at
                FROM reservations r
                WHERE r.id = reservation_limit_scopes.reservation_id
            )
        WHERE created_at IS NULL OR amount_usd = 0
        ",
        [],
    )?;
    Ok(())
}

fn backfill_rolling_spend_buckets(conn: &Connection) -> Result<(), NoetError> {
    let migrated_to_seconds = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 2)",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if migrated_to_seconds {
        return Ok(());
    }
    conn.execute("DELETE FROM rolling_spend_buckets", [])?;
    conn.execute(
        "
        INSERT INTO rolling_spend_buckets (rule_id, limit_id, scope_key, bucket_start, amount_usd)
        SELECT rule_id,
               limit_id,
               scope_key,
               strftime('%Y-%m-%dT%H:%M:%S+00:00', created_at),
               COALESCE(SUM(amount_usd), 0)
        FROM reservation_limit_scopes
        WHERE created_at IS NOT NULL
        GROUP BY rule_id, limit_id, scope_key, strftime('%Y-%m-%dT%H:%M:%S+00:00', created_at)
        ",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (2, datetime('now'))",
        [],
    )?;
    Ok(())
}

fn backfill_decision_app_run_keys(conn: &Connection) -> Result<(), NoetError> {
    conn.execute(
        "
        UPDATE decisions
        SET app_run_key = CASE
            WHEN json_extract(metadata_json, '$.agent_run_id') IS NOT NULL
                THEN 'agent-run:' || json_extract(metadata_json, '$.agent_run_id')
            WHEN trace_id IS NOT NULL
                THEN 'trace-fallback:' || trace_id
            ELSE 'untraced:' || outcome || ':' ||
                COALESCE(selected_budget_id, 'unattributed') || ':' ||
                COALESCE(
                    CASE
                        WHEN provider IS NOT NULL AND model IS NOT NULL THEN provider || '/' || model
                        WHEN model IS NOT NULL THEN model
                        WHEN provider IS NOT NULL THEN provider
                    END,
                    'unknown'
                ) || ':' || (CAST(strftime('%s', created_at) AS INTEGER) / 60)
        END
        WHERE app_run_key IS NULL OR app_run_key = ''
        ",
        [],
    )?;
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    column_definition: &str,
) -> Result<(), NoetError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    if !columns.iter().any(|existing| existing == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column_definition}"),
            [],
        )?;
    }
    Ok(())
}

fn string_metadata(request: &AuthorizeRequest, key: &str) -> Option<String> {
    string_value(&request.metadata, key)
}

fn string_value(
    metadata: &std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

fn decision_app_run_key(
    trace_id: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    outcome: &str,
    selected_budget_id: Option<&str>,
    metadata: &std::collections::BTreeMap<String, serde_json::Value>,
    created_at: DateTime<Utc>,
) -> String {
    if let Some(agent_run_id) = string_value(metadata, "agent_run_id")
        && !agent_run_id.is_empty()
    {
        return format!("agent-run:{agent_run_id}");
    }
    if let Some(trace_id) = trace_id
        && !trace_id.is_empty()
    {
        return format!("trace-fallback:{trace_id}");
    }
    let model_ref = match (provider, model) {
        (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
        (None, Some(model)) => Some(model.to_owned()),
        (Some(provider), None) => Some(provider.to_owned()),
        (None, None) => None,
    };
    format!(
        "untraced:{}:{}:{}:{}",
        outcome,
        selected_budget_id.unwrap_or("unattributed"),
        model_ref.as_deref().unwrap_or("unknown"),
        created_at.timestamp() / 60
    )
}

fn validate_event_payload(event: &TraceEvent) -> Result<(), NoetError> {
    match event.kind.as_str() {
        "usage.observed" => {
            serde_json::from_value::<UsageObservation>(event.payload.clone())?;
        }
        "tool.observed" => {
            serde_json::from_value::<ToolEvent>(event.payload.clone())?;
        }
        "eval.annotation" => {
            serde_json::from_value::<EvalAnnotation>(event.payload.clone())?;
        }
        _ => {}
    }
    Ok(())
}

fn outcome_text(outcome: DecisionOutcome) -> &'static str {
    match outcome {
        DecisionOutcome::Allow => "allow",
        DecisionOutcome::Warn => "warn",
        DecisionOutcome::Deny => "deny",
    }
}

fn parse_decision_outcome(value: &str) -> DecisionOutcome {
    match value {
        "warn" => DecisionOutcome::Warn,
        "deny" => DecisionOutcome::Deny,
        _ => DecisionOutcome::Allow,
    }
}

fn action_text(action: PolicyAction) -> &'static str {
    match action {
        PolicyAction::Allow => "allow",
        PolicyAction::Warn => "warn",
        PolicyAction::Block => "block",
        PolicyAction::Ask => "ask",
    }
}

fn reservation_status_text(status: ReservationStatus) -> &'static str {
    match status {
        ReservationStatus::Active => "active",
        ReservationStatus::Finalized => "finalized",
    }
}

fn parse_reservation_status(value: &str) -> ReservationStatus {
    match value {
        "finalized" => ReservationStatus::Finalized,
        _ => ReservationStatus::Active,
    }
}

fn advance_tumbling_window_start(
    started_at: DateTime<Utc>,
    window_seconds: Duration,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let elapsed_seconds = (now - started_at).num_seconds();
    let window_size_seconds = window_seconds.num_seconds();
    let completed_windows = elapsed_seconds.div_euclid(window_size_seconds);
    started_at + Duration::seconds(completed_windows * window_size_seconds)
}

fn parse_time(value: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn protected_adoption_report(
    conn: &Connection,
) -> Result<Option<ProtectedAdoptionReport>, NoetError> {
    let mut stmt = conn.prepare(
        "
        SELECT rule_id, entity_key, protected_amount_usd, current_grant_usd, carryover_usd
        FROM budget_allocation_buckets
        WHERE protected_amount_usd > 0
        ORDER BY rule_id, entity_key
        ",
    )?;
    let entities: Vec<ProtectedAdoptionEntityReport> = stmt
        .query_map([], |row| {
            let protected_amount_usd: f64 = row.get(2)?;
            let current_grant_usd: f64 = row.get(3)?;
            let carryover_usd: f64 = row.get(4)?;
            Ok(ProtectedAdoptionEntityReport {
                budget_id: row.get(0)?,
                entity_key: row.get(1)?,
                protected_amount_usd,
                current_grant_usd,
                carryover_usd,
                used_current_grant_usd: (protected_amount_usd - current_grant_usd).max(0.0),
            })
        })?
        .collect::<Result<_, _>>()?;
    if entities.is_empty() {
        return Ok(None);
    }

    let mut low_adopters = Vec::new();
    let mut high_adopters = Vec::new();
    for entity in &entities {
        let usage_fraction = if entity.protected_amount_usd <= 0.0 {
            0.0
        } else {
            entity.used_current_grant_usd / entity.protected_amount_usd
        };
        if usage_fraction <= 0.2 {
            low_adopters.push(entity.clone());
        }
        if usage_fraction >= 0.8 {
            high_adopters.push(entity.clone());
        }
    }

    Ok(Some(ProtectedAdoptionReport {
        unused_protected_opportunity_usd: entities
            .iter()
            .map(|entity| entity.current_grant_usd)
            .sum(),
        carryover_liability_usd: entities.iter().map(|entity| entity.carryover_usd).sum(),
        low_adopters,
        high_adopters,
    }))
}

fn protected_adoption_report_postgres(
    pg_conn: &SyncPostgresClient,
) -> Result<Option<ProtectedAdoptionReport>, NoetError> {
    let rows = pg_conn.0.lock().expect("postgres mutex").query(
        "
        SELECT rule_id, entity_key, protected_amount_usd, current_grant_usd, carryover_usd
        FROM budget_allocation_buckets
        WHERE protected_amount_usd > 0
        ORDER BY rule_id, entity_key
        ",
        &[],
    )?;
    let entities = rows
        .into_iter()
        .map(|row| {
            let protected_amount_usd: f64 = row.get(2);
            let current_grant_usd: f64 = row.get(3);
            let carryover_usd: f64 = row.get(4);
            ProtectedAdoptionEntityReport {
                budget_id: row.get(0),
                entity_key: row.get(1),
                protected_amount_usd,
                current_grant_usd,
                carryover_usd,
                used_current_grant_usd: (protected_amount_usd - current_grant_usd).max(0.0),
            }
        })
        .collect::<Vec<_>>();
    if entities.is_empty() {
        return Ok(None);
    }

    let mut low_adopters = Vec::new();
    let mut high_adopters = Vec::new();
    for entity in &entities {
        let usage_fraction = if entity.protected_amount_usd <= 0.0 {
            0.0
        } else {
            entity.used_current_grant_usd / entity.protected_amount_usd
        };
        if usage_fraction <= 0.2 {
            low_adopters.push(entity.clone());
        }
        if usage_fraction >= 0.8 {
            high_adopters.push(entity.clone());
        }
    }

    Ok(Some(ProtectedAdoptionReport {
        unused_protected_opportunity_usd: entities
            .iter()
            .map(|entity| entity.current_grant_usd)
            .sum(),
        carryover_liability_usd: entities.iter().map(|entity| entity.carryover_usd).sum(),
        low_adopters,
        high_adopters,
    }))
}

fn rule_stat_reason(explanations_json: &str, limit_hits_json: Option<&str>) -> Option<String> {
    limit_hits_json
        .and_then(parse_optional_json::<Vec<DecisionLimitHitReport>>)
        .and_then(|hits| hits.into_iter().next())
        .map(|hit| hit.reason)
        .or_else(|| {
            serde_json::from_str::<Vec<DecisionExplanation>>(explanations_json)
                .ok()?
                .into_iter()
                .find(|explanation| explanation.severity == DecisionSeverity::Deny)
                .or_else(|| {
                    serde_json::from_str::<Vec<DecisionExplanation>>(explanations_json)
                        .ok()?
                        .into_iter()
                        .next()
                })
                .map(|explanation| explanation.reason)
        })
}

fn most_common_count(values: &HashMap<String, u64>) -> Option<String> {
    values
        .iter()
        .max_by(|(left_value, left_count), (right_value, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| right_value.cmp(left_value))
        })
        .map(|(value, _)| value.clone())
}

fn decision_report_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceReportItem> {
    let outcome: String = row.get(1)?;
    let decision_id: String = row.get(2)?;
    let trace_id: Option<String> = row.get(3)?;
    let request_id: Option<String> = row.get(4)?;
    let provider: Option<String> = row.get(5)?;
    let model: Option<String> = row.get(6)?;
    let action: String = row.get(7)?;
    let estimated_tokens: Option<i64> = row.get(8)?;
    let estimated_cost_usd: Option<f64> = row.get(9)?;
    let explanations_json: String = row.get(10)?;
    let metadata_json: String = row.get(11)?;
    let entities_json: String = row.get(12)?;
    let selected_budget_id: Option<String> = row.get(13)?;
    let matched_entity: Option<String> = row.get(14)?;
    let selection_reason: Option<String> = row.get(15)?;
    let rejected_budget_id: Option<String> = row.get(16)?;
    let rejected_budget_reason: Option<String> = row.get(17)?;
    let model_check: Option<String> = row.get(18)?;
    let budget_window_remaining_usd: Option<f64> = row.get(19)?;
    let routing_json: Option<String> = row.get(20)?;
    let limit_hits_json: Option<String> = row.get(21)?;
    let agent_run_id: Option<String> = row.get(22)?;
    let primary_rule_id = selected_budget_id
        .clone()
        .or_else(|| primary_explanation_rule_id(&explanations_json));
    let mut routing = routing_json
        .as_deref()
        .and_then(parse_optional_json::<DecisionRoutingReport>)
        .or_else(|| {
            decision_routing_report(&RoutingPersistenceFields {
                selected_budget_id: primary_rule_id.clone(),
                matched_entity: matched_entity.clone(),
                selection_reason: selection_reason.clone(),
                rejected_budget_id: rejected_budget_id.clone(),
                rejected_budget_reason: rejected_budget_reason.clone(),
                model_check: model_check.clone(),
                budget_window_remaining_usd,
                ..Default::default()
            })
        });
    if let Some(routing) = routing.as_mut()
        && routing.selected_budget_id.is_none()
    {
        routing.selected_budget_id = primary_rule_id.clone();
    }
    let limit_hits = limit_hits_json
        .as_deref()
        .and_then(parse_optional_json::<Vec<DecisionLimitHitReport>>)
        .filter(|hits| !hits.is_empty())
        .or_else(|| limit_hits_from_explanations_json(&explanations_json));
    let summary = DecisionSummary {
        action: &action,
        decision_id: &decision_id,
        trace_id: trace_id.as_deref(),
        request_id: request_id.as_deref(),
        provider: provider.as_deref(),
        model: model.as_deref(),
        estimated_tokens,
        estimated_cost_usd,
        metadata_json: &metadata_json,
        limit_hits: limit_hits.as_deref(),
        routing: DecisionRoutingSummary {
            selected_budget_id: routing
                .as_ref()
                .and_then(|report| report.selected_budget_id.as_deref())
                .or(primary_rule_id.as_deref()),
            matched_entity: routing
                .as_ref()
                .and_then(|report| report.matched_entity.as_deref())
                .or(matched_entity.as_deref()),
            selection_reason: routing
                .as_ref()
                .and_then(|report| report.selection_reason.as_deref())
                .or(selection_reason.as_deref()),
            rejected_budget_id: routing
                .as_ref()
                .and_then(|report| report.rejected_budget_id.as_deref())
                .or(rejected_budget_id.as_deref()),
            rejected_budget_reason: routing
                .as_ref()
                .and_then(|report| report.rejected_budget_reason.as_deref())
                .or(rejected_budget_reason.as_deref()),
            model_check: routing
                .as_ref()
                .and_then(|report| report.model_check.as_deref())
                .or(model_check.as_deref()),
            budget_window_remaining_usd: routing
                .as_ref()
                .and_then(|report| report.budget_window_remaining_usd)
                .or(budget_window_remaining_usd),
            budget_window_mode: routing
                .as_ref()
                .and_then(|report| report.budget_window_mode.as_deref()),
            budget_window_started_at: routing
                .as_ref()
                .and_then(|report| report.budget_window_started_at),
            budget_window_ends_at: routing
                .as_ref()
                .and_then(|report| report.budget_window_ends_at),
        },
    };
    Ok(TraceReportItem {
        occurred_at: parse_time(row.get::<_, String>(0)?),
        kind: format!("decision.{outcome}"),
        summary: summarize_decision(summary),
        trace_id,
        agent_run_id,
        entities: parse_entities_json(entities_json),
        binding_limit: limit_hits.as_deref().and_then(binding_limit_hit).cloned(),
        routing,
        limit_hits,
    })
}

fn decision_report_item_from_postgres_row(row: &PostgresRow) -> TraceReportItem {
    let outcome: String = row.get(1);
    let decision_id: String = row.get(2);
    let trace_id: Option<String> = row.get(3);
    let request_id: Option<String> = row.get(4);
    let provider: Option<String> = row.get(5);
    let model: Option<String> = row.get(6);
    let action: String = row.get(7);
    let estimated_tokens: Option<i64> = row.get(8);
    let estimated_cost_usd: Option<f64> = row.get(9);
    let explanations_json: String = row.get(10);
    let metadata_json: String = row.get(11);
    let entities_json: String = row.get(12);
    let selected_budget_id: Option<String> = row.get(13);
    let matched_entity: Option<String> = row.get(14);
    let selection_reason: Option<String> = row.get(15);
    let rejected_budget_id: Option<String> = row.get(16);
    let rejected_budget_reason: Option<String> = row.get(17);
    let model_check: Option<String> = row.get(18);
    let budget_window_remaining_usd: Option<f64> = row.get(19);
    let routing_json: Option<String> = row.get(20);
    let limit_hits_json: Option<String> = row.get(21);
    let agent_run_id: Option<String> = row.get(22);
    let primary_rule_id = selected_budget_id
        .clone()
        .or_else(|| primary_explanation_rule_id(&explanations_json));
    let mut routing = routing_json
        .as_deref()
        .and_then(parse_optional_json::<DecisionRoutingReport>)
        .or_else(|| {
            decision_routing_report(&RoutingPersistenceFields {
                selected_budget_id: primary_rule_id.clone(),
                matched_entity: matched_entity.clone(),
                selection_reason: selection_reason.clone(),
                rejected_budget_id: rejected_budget_id.clone(),
                rejected_budget_reason: rejected_budget_reason.clone(),
                model_check: model_check.clone(),
                budget_window_remaining_usd,
                ..Default::default()
            })
        });
    if let Some(routing) = routing.as_mut()
        && routing.selected_budget_id.is_none()
    {
        routing.selected_budget_id = primary_rule_id.clone();
    }
    let limit_hits = limit_hits_json
        .as_deref()
        .and_then(parse_optional_json::<Vec<DecisionLimitHitReport>>)
        .filter(|hits| !hits.is_empty())
        .or_else(|| limit_hits_from_explanations_json(&explanations_json));
    let summary = DecisionSummary {
        action: &action,
        decision_id: &decision_id,
        trace_id: trace_id.as_deref(),
        request_id: request_id.as_deref(),
        provider: provider.as_deref(),
        model: model.as_deref(),
        estimated_tokens,
        estimated_cost_usd,
        metadata_json: &metadata_json,
        limit_hits: limit_hits.as_deref(),
        routing: DecisionRoutingSummary {
            selected_budget_id: routing
                .as_ref()
                .and_then(|report| report.selected_budget_id.as_deref())
                .or(primary_rule_id.as_deref()),
            matched_entity: routing
                .as_ref()
                .and_then(|report| report.matched_entity.as_deref())
                .or(matched_entity.as_deref()),
            selection_reason: routing
                .as_ref()
                .and_then(|report| report.selection_reason.as_deref())
                .or(selection_reason.as_deref()),
            rejected_budget_id: routing
                .as_ref()
                .and_then(|report| report.rejected_budget_id.as_deref())
                .or(rejected_budget_id.as_deref()),
            rejected_budget_reason: routing
                .as_ref()
                .and_then(|report| report.rejected_budget_reason.as_deref())
                .or(rejected_budget_reason.as_deref()),
            model_check: routing
                .as_ref()
                .and_then(|report| report.model_check.as_deref())
                .or(model_check.as_deref()),
            budget_window_remaining_usd: routing
                .as_ref()
                .and_then(|report| report.budget_window_remaining_usd)
                .or(budget_window_remaining_usd),
            budget_window_mode: routing
                .as_ref()
                .and_then(|report| report.budget_window_mode.as_deref()),
            budget_window_started_at: routing
                .as_ref()
                .and_then(|report| report.budget_window_started_at),
            budget_window_ends_at: routing
                .as_ref()
                .and_then(|report| report.budget_window_ends_at),
        },
    };
    TraceReportItem {
        occurred_at: parse_time(row.get::<_, String>(0)),
        kind: format!("decision.{outcome}"),
        summary: summarize_decision(summary),
        trace_id,
        agent_run_id,
        entities: parse_entities_json(entities_json),
        binding_limit: limit_hits.as_deref().and_then(binding_limit_hit).cloned(),
        routing,
        limit_hits,
    }
}

fn historical_authorize_request_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<HistoricalAuthorizeRequest> {
    let entities_json: String = row.get(4)?;
    let metadata_json: String = row.get(11)?;
    Ok(HistoricalAuthorizeRequest {
        occurred_at: parse_time(row.get::<_, String>(0)?),
        decision_id: row.get(1)?,
        baseline_outcome: parse_decision_outcome(row.get::<_, String>(2)?.as_str()),
        request: AuthorizeRequest {
            budget_id: serde_json::from_str::<Value>(&metadata_json)
                .ok()
                .and_then(|value| {
                    value
                        .get("budget_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }),
            entities: parse_entities_json(entities_json),
            subject: row.get(5)?,
            project: row.get(6)?,
            provider: row.get(7)?,
            model: row.get(8)?,
            estimated_tokens: row
                .get::<_, Option<i64>>(9)?
                .map(|value| value.max(0) as u64),
            estimated_cost_usd: row.get(10)?,
            metadata: serde_json::from_str(&metadata_json).unwrap_or_default(),
        },
    })
}

fn historical_authorize_request_from_postgres_row(row: &PostgresRow) -> HistoricalAuthorizeRequest {
    let entities_json: String = row.get(4);
    let metadata_json: String = row.get(11);
    HistoricalAuthorizeRequest {
        occurred_at: parse_time(row.get::<_, String>(0)),
        decision_id: row.get(1),
        baseline_outcome: parse_decision_outcome(row.get::<_, String>(2).as_str()),
        request: AuthorizeRequest {
            budget_id: serde_json::from_str::<Value>(&metadata_json)
                .ok()
                .and_then(|value| {
                    value
                        .get("budget_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }),
            entities: parse_entities_json(entities_json),
            subject: row.get(5),
            project: row.get(6),
            provider: row.get(7),
            model: row.get(8),
            estimated_tokens: row
                .get::<_, Option<i64>>(9)
                .map(|value| value.max(0) as u64),
            estimated_cost_usd: row.get(10),
            metadata: serde_json::from_str(&metadata_json).unwrap_or_default(),
        },
    }
}

fn usage_activity_record_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<UsageActivityRecord> {
    Ok(UsageActivityRecord {
        occurred_at: parse_time(row.get::<_, String>(0)?),
        trace_id: row.get(1)?,
        agent_run_id: row.get(15)?,
        request_id: row.get(16)?,
        subject: row.get(2)?,
        project: row.get(3)?,
        provider: row.get(4)?,
        model: row.get(5)?,
        selected_budget_id: row.get(6)?,
        matched_entity: row.get(7)?,
        entities: parse_entities_json(row.get::<_, String>(8)?),
        input_tokens: row.get::<_, i64>(9)?.max(0) as u64,
        output_tokens: row.get::<_, i64>(10)?.max(0) as u64,
        cache_read_tokens: row.get::<_, i64>(11)?.max(0) as u64,
        cache_write_tokens: row.get::<_, i64>(12)?.max(0) as u64,
        total_tokens: row.get::<_, i64>(13)?.max(0) as u64,
        cost_usd: row.get(14)?,
    })
}

fn usage_activity_record_from_postgres_row(row: &PostgresRow) -> UsageActivityRecord {
    UsageActivityRecord {
        occurred_at: parse_time(row.get::<_, String>(0)),
        trace_id: row.get(1),
        agent_run_id: row.get(15),
        request_id: row.get(16),
        subject: row.get(2),
        project: row.get(3),
        provider: row.get(4),
        model: row.get(5),
        selected_budget_id: row.get(6),
        matched_entity: row.get(7),
        entities: parse_entities_json(row.get::<_, String>(8)),
        input_tokens: row.get::<_, i64>(9).max(0) as u64,
        output_tokens: row.get::<_, i64>(10).max(0) as u64,
        cache_read_tokens: row.get::<_, i64>(11).max(0) as u64,
        cache_write_tokens: row.get::<_, i64>(12).max(0) as u64,
        total_tokens: row.get::<_, i64>(13).max(0) as u64,
        cost_usd: row.get(14),
    }
}

fn event_report_item_from_postgres_row(row: PostgresRow) -> TraceReportItem {
    let kind: String = row.get(1);
    let payload_json: String = row.get(2);
    TraceReportItem {
        occurred_at: parse_time(row.get::<_, String>(0)),
        summary: summarize_event_payload(&kind, &payload_json),
        kind,
        trace_id: row.get(3),
        agent_run_id: None,
        entities: Vec::new(),
        routing: None,
        limit_hits: None,
        binding_limit: None,
    }
}

fn trace_event_from_sqlite_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceEvent> {
    let payload_json: String = row.get(4)?;
    let payload = serde_json::from_str::<Value>(&payload_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(TraceEvent {
        id: row.get(0)?,
        trace_id: row.get(1)?,
        kind: row.get(2)?,
        occurred_at: Some(parse_time(row.get::<_, String>(3)?)),
        payload,
    })
}

fn trace_event_from_postgres_row(row: PostgresRow) -> Result<TraceEvent, NoetError> {
    let payload_json: String = row.get(4);
    Ok(TraceEvent {
        id: row.get(0),
        trace_id: row.get(1),
        kind: row.get(2),
        occurred_at: Some(parse_time(row.get::<_, String>(3))),
        payload: serde_json::from_str(&payload_json)?,
    })
}

fn agent_run_id_from_metadata_json(metadata_json: &str) -> Option<String> {
    serde_json::from_str::<Value>(metadata_json)
        .ok()
        .and_then(|value| {
            value
                .get("agent_run_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

fn decision_routing_report(fields: &RoutingPersistenceFields) -> Option<DecisionRoutingReport> {
    let has_fields = fields.selected_budget_id.is_some()
        || fields.matched_entity.is_some()
        || fields.selection_reason.is_some()
        || fields.rejected_budget_id.is_some()
        || fields.rejected_budget_reason.is_some()
        || fields.model_check.is_some()
        || fields.budget_window_remaining_usd.is_some()
        || fields.budget_window_mode.is_some()
        || fields.budget_window_started_at.is_some()
        || fields.budget_window_ends_at.is_some();
    has_fields.then_some(DecisionRoutingReport {
        selected_budget_id: fields.selected_budget_id.clone(),
        matched_entity: fields.matched_entity.clone(),
        selection_reason: fields.selection_reason.clone(),
        rejected_budget_id: fields.rejected_budget_id.clone(),
        rejected_budget_reason: fields.rejected_budget_reason.clone(),
        model_check: fields.model_check.clone(),
        budget_window_remaining_usd: fields.budget_window_remaining_usd,
        budget_window_mode: fields.budget_window_mode.clone(),
        budget_window_started_at: fields.budget_window_started_at,
        budget_window_ends_at: fields.budget_window_ends_at,
    })
}

fn parse_entities_json(value: String) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&value).unwrap_or_default()
}

fn parse_optional_json<T: DeserializeOwned>(value: &str) -> Option<T> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null") {
        None
    } else {
        serde_json::from_str(trimmed).ok()
    }
}

fn limit_hits_from_explanations_json(
    explanations_json: &str,
) -> Option<Vec<DecisionLimitHitReport>> {
    let hits: Vec<DecisionLimitHitReport> =
        serde_json::from_str::<Vec<DecisionExplanation>>(explanations_json)
            .unwrap_or_default()
            .into_iter()
            .filter(|explanation| is_limit_rule_id(&explanation.rule_id))
            .map(|explanation| DecisionLimitHitReport {
                rule_id: explanation.rule_id,
                reason: explanation.reason,
                severity: explanation.severity,
                window_id: None,
                window_mode: None,
                window_started_at: None,
                window_ends_at: None,
                projected_spend_usd: None,
                max_usd: None,
                scope_entity: None,
            })
            .collect();
    (!hits.is_empty()).then_some(hits)
}

fn primary_explanation_rule_id(explanations_json: &str) -> Option<String> {
    serde_json::from_str::<Vec<DecisionExplanation>>(explanations_json)
        .ok()?
        .into_iter()
        .find(|explanation| {
            !matches!(
                explanation.rule_id.as_str(),
                "no_policy" | "no_budget_match" | "no_fallback_budget"
            )
        })
        .map(|explanation| explanation.rule_id)
}

fn is_limit_rule_id(rule_id: &str) -> bool {
    rule_id.contains(".request_cost")
        || rule_id.contains(".context_tokens")
        || rule_id.contains(".spend_window.")
}

fn warning_advisory_cooldown(
    config: &AdvisoryConfig,
    configured_cadence: Option<&str>,
) -> Duration {
    configured_cadence
        .and_then(parse_limit_window)
        .or_else(|| {
            config
                .warning_cadence
                .as_deref()
                .and_then(parse_limit_window)
        })
        .unwrap_or(WARN_ADVISORY_COOLDOWN)
}

mod authorization;
pub use authorization::binding_limit_hit;
use authorization::{
    AuthorizationEngine, BudgetEvaluationOutputs, BudgetLimitEvaluation, apply_budget_limits,
    authorize_metadata, biggest_policy_rolling_spend_window_duration,
    biggest_spend_window_duration, biggest_spend_window_projection, limit_hit_identifier,
    message_hint_from_limit_hit, message_hints_metadata, request_user_key, spend_limit_hit,
    spend_limit_identifier, spend_limit_scope_key, spend_window_projections,
};

struct EnablementHeadroom {
    scope_key: String,
}

struct EnablementTip {
    key: &'static str,
    body: &'static str,
}

struct ResolvedEnablementTip {
    key: String,
    body: String,
}

const ENABLEMENT_TIPS: &[EnablementTip] = &[
    EnablementTip {
        key: "workflow.codify_repeated_process",
        body: "Try codifying a repeated team workflow into a reusable skill or checklist.",
    },
    EnablementTip {
        key: "automation.mechanical_process",
        body: "Try turning a weekly mechanical process into a script, template, or command.",
    },
    EnablementTip {
        key: "visualization.interactive_html",
        body: "Try asking for an interactive HTML view of messy logs, reports, or datasets.",
    },
    EnablementTip {
        key: "agentic.background_investigation",
        body: "Try delegating a low-risk cleanup or investigation to a background agent.",
    },
    EnablementTip {
        key: "agentic.parallel_review",
        body: "Try parallel reviews of the same diff, one focused on bugs and one on simplification.",
    },
    EnablementTip {
        key: "architecture.design_suggestions",
        body: "Try asking for codebase architecture suggestions before a large refactor.",
    },
    EnablementTip {
        key: "reflection.workflow_review",
        body: "Try asking an agent to review your recent commits or logs and identify repeated friction.",
    },
    EnablementTip {
        key: "reflection.automation_patterns",
        body: "Try giving an agent JSON logs and asking for bottlenecks or missed automation.",
    },
];

fn enablement_tip(
    config: &AdvisoryConfig,
    user_key: &str,
    now: DateTime<Utc>,
) -> Option<ResolvedEnablementTip> {
    let mut tips: Vec<ResolvedEnablementTip> = match config.enablement_tips.mode {
        TipCatalogMode::Extend => ENABLEMENT_TIPS
            .iter()
            .map(|tip| ResolvedEnablementTip {
                key: tip.key.to_owned(),
                body: tip.body.to_owned(),
            })
            .collect(),
        TipCatalogMode::Replace => Vec::new(),
    };
    tips.extend(
        config
            .enablement_tips
            .tips
            .iter()
            .map(|tip| ResolvedEnablementTip {
                key: tip.key.clone(),
                body: tip.body.clone(),
            }),
    );
    if tips.is_empty() {
        return None;
    }
    let bucket = now
        .timestamp()
        .div_euclid(ENABLEMENT_NOTIFICATION_COOLDOWN.num_seconds());
    let seed = stable_hash(user_key).wrapping_add(bucket as u64);
    Some(tips.swap_remove((seed as usize) % tips.len()))
}

fn stable_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        hash.wrapping_mul(0x100000001b3) ^ u64::from(byte)
    })
}

fn enablement_headroom(
    policy: &PolicyFile,
    ledger: &BudgetLedger,
    request: &AuthorizeRequest,
    now: DateTime<Utc>,
) -> Option<EnablementHeadroom> {
    let applicable: Vec<&BudgetRule> = policy
        .budgets
        .iter()
        .filter(|rule| budget_rule_matches(rule, request) && budget_model_allowed(rule, request))
        .collect();
    if applicable.is_empty() {
        return None;
    }

    let mut underpaced_scope = None;
    for rule in applicable {
        let projections =
            spend_window_projections(ledger, rule, request, request.estimated_cost(), now).ok()?;
        let has_spend_budget = !projections.is_empty();
        let has_available_allocation = allocation_bucket_available_usd(ledger, rule, request, now)
            .is_some_and(|available| available > 0.0);
        if !has_spend_budget && !has_available_allocation {
            continue;
        }
        let pressure = projections
            .iter()
            .map(|projection| projection.projected_spend_usd / projection.max_usd)
            .fold(0.0, f64::max);
        let warn_at = rule
            .limits
            .spend
            .iter()
            .flat_map(|limit| limit.warn_at_fractions.iter().copied())
            .filter(|fraction| *fraction < 1.0)
            .fold(1.0, f64::min);
        if pressure >= warn_at {
            return None;
        }
        if has_available_allocation
            || (has_spend_budget && pressure <= ENABLEMENT_UNDER_PACE_FRACTION)
        {
            underpaced_scope.get_or_insert_with(|| rule.id.clone());
        }
    }

    underpaced_scope.map(|scope_key| EnablementHeadroom { scope_key })
}

fn allocation_bucket_entity_key(rule: &BudgetRule, request: &AuthorizeRequest) -> Option<String> {
    let allocation = rule.allocation.as_ref()?;
    if allocation.standard != "protected_adoption_pool" {
        return None;
    }
    match allocation.by.as_deref() {
        Some("user") => request
            .entities
            .iter()
            .find(|entity| entity.starts_with("user:"))
            .cloned()
            .or_else(|| {
                request.subject.as_deref().map(|subject| {
                    if subject.contains(':') {
                        subject.to_owned()
                    } else {
                        format!("user:{subject}")
                    }
                })
            }),
        Some("team") => request
            .entities
            .iter()
            .find(|entity| entity.starts_with("team:"))
            .cloned(),
        _ => None,
    }
}

fn allocation_bucket_available_usd(
    ledger: &BudgetLedger,
    rule: &BudgetRule,
    request: &AuthorizeRequest,
    now: DateTime<Utc>,
) -> Option<f64> {
    let entity_key = allocation_bucket_entity_key(rule, request)?;
    let protected_amount_usd = rule
        .allocation
        .as_ref()
        .and_then(|allocation| allocation.protected_amount_usd)?;
    let bucket = ledger
        .allocation_buckets
        .get(&(rule.id.clone(), entity_key))
        .cloned()
        .unwrap_or(AllocationBucketState {
            started_at: now,
            protected_amount_usd,
            current_grant_usd: protected_amount_usd,
            carryover_usd: 0.0,
        });
    let bucket = rolled_allocation_bucket_state(rule, bucket, now)?;
    Some(bucket.current_grant_usd + bucket.carryover_usd)
}

fn allocation_limit_hit(
    rule: &BudgetRule,
    request: &AuthorizeRequest,
    estimated_cost: f64,
    available_usd: f64,
) -> DecisionLimitHitReport {
    let reason = format!(
        "estimated request cost ${estimated_cost:.6} exceeds protected allocation available ${available_usd:.6}"
    );
    DecisionLimitHitReport {
        rule_id: format!("{}.allocation", rule.id),
        reason,
        severity: DecisionSeverity::Deny,
        window_id: Some("protected_adoption_pool".to_owned()),
        window_mode: None,
        window_started_at: None,
        window_ends_at: None,
        projected_spend_usd: Some(estimated_cost),
        max_usd: Some(available_usd),
        scope_entity: allocation_bucket_entity_key(rule, request),
    }
}

fn consume_allocation_bucket(
    ledger: &mut BudgetLedger,
    rule: &BudgetRule,
    request: &AuthorizeRequest,
    amount_usd: f64,
    now: DateTime<Utc>,
) -> Option<AllocationReservationSpend> {
    let entity_key = allocation_bucket_entity_key(rule, request)?;
    let protected_amount_usd = rule
        .allocation
        .as_ref()
        .and_then(|allocation| allocation.protected_amount_usd)?;
    let bucket = ledger
        .allocation_buckets
        .entry((rule.id.clone(), entity_key.clone()))
        .or_insert(AllocationBucketState {
            started_at: now,
            protected_amount_usd,
            current_grant_usd: protected_amount_usd,
            carryover_usd: 0.0,
        });
    *bucket = rolled_allocation_bucket_state(rule, bucket.clone(), now)?;
    let carryover_usd = bucket.carryover_usd.min(amount_usd);
    bucket.carryover_usd = (bucket.carryover_usd - carryover_usd).max(0.0);
    let current_grant_usd = bucket.current_grant_usd.min(amount_usd - carryover_usd);
    bucket.current_grant_usd = (bucket.current_grant_usd - current_grant_usd).max(0.0);
    Some(AllocationReservationSpend {
        rule_id: rule.id.clone(),
        entity_key,
        carryover_usd,
        current_grant_usd,
    })
}

fn rolled_allocation_bucket_state(
    rule: &BudgetRule,
    mut bucket: AllocationBucketState,
    now: DateTime<Utc>,
) -> Option<AllocationBucketState> {
    let allocation = rule.allocation.as_ref()?;
    if allocation.standard != "protected_adoption_pool" {
        return Some(bucket);
    }
    let biggest_window = biggest_spend_window_duration(rule)?;
    if now - bucket.started_at < biggest_window {
        return Some(bucket);
    }
    let protected_amount_usd = allocation.protected_amount_usd?;
    let carryover = allocation.carryover.as_ref()?;
    let percent = carryover.percent.unwrap_or(0.0) / 100.0;
    let cap_usd = carryover.cap_usd.unwrap_or(0.0);
    bucket.carryover_usd =
        (bucket.carryover_usd + (bucket.current_grant_usd * percent)).min(cap_usd);
    bucket.current_grant_usd = protected_amount_usd;
    bucket.started_at = now;
    Some(bucket)
}

fn recent_spend_usd(
    ledger: &BudgetLedger,
    rule_id: &str,
    limit_id: &str,
    scope_key: &str,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> f64 {
    match &ledger.store {
        LedgerStore::Postgres(pg_conn) => {
            let bucket_since = rolling_bucket_start(since).to_rfc3339();
            let bucket_now = rolling_bucket_start(now).to_rfc3339();
            let value = pg_conn.0.lock().expect("postgres mutex").query_one(
                "
                SELECT COALESCE(SUM(amount_usd), 0)
                FROM rolling_spend_buckets
                WHERE rule_id = $1
                  AND limit_id = $2
                  AND scope_key = $3
                  AND bucket_start >= $4
                  AND bucket_start <= $5
                ",
                &[&rule_id, &limit_id, &scope_key, &bucket_since, &bucket_now],
            );
            return value.map(|row| row.get::<_, f64>(0)).unwrap_or(0.0);
        }
        LedgerStore::Sqlite(conn) => {
            let bucket_since = rolling_bucket_start(since);
            let bucket_now = rolling_bucket_start(now);
            let value = conn.query_row(
                "
                SELECT COALESCE(SUM(amount_usd), 0)
                FROM rolling_spend_buckets
                WHERE rule_id = ?1
                  AND limit_id = ?2
                  AND scope_key = ?3
                  AND bucket_start >= ?4
                  AND bucket_start <= ?5
                ",
                params![
                    rule_id,
                    limit_id,
                    scope_key,
                    bucket_since.to_rfc3339(),
                    bucket_now.to_rfc3339()
                ],
                |row| row.get::<_, f64>(0),
            );
            return value.unwrap_or(0.0);
        }
        LedgerStore::InMemory => {}
    }

    if !ledger.rolling_spend_buckets.is_empty() {
        let bucket_since = rolling_bucket_start(since);
        let bucket_now = rolling_bucket_start(now);
        return ledger
            .rolling_spend_buckets
            .iter()
            .filter(
                |((bucket_rule, bucket_limit, bucket_scope, bucket_start), _)| {
                    bucket_rule == rule_id
                        && bucket_limit == limit_id
                        && bucket_scope == scope_key
                        && *bucket_start >= bucket_since
                        && *bucket_start <= bucket_now
                },
            )
            .map(|(_, amount)| *amount)
            .sum();
    }

    ledger
        .reservations
        .values()
        .filter(|stored| {
            stored.limit_window_spends.iter().any(|spend| {
                spend.rule_id == rule_id
                    && spend.limit_id == limit_id
                    && spend.scope_key == scope_key
            }) && stored.reservation.created_at >= since
                && stored.reservation.created_at <= now
        })
        .map(|stored| stored.reservation.amount_usd)
        .sum()
}

fn rolling_bucket_start(at: DateTime<Utc>) -> DateTime<Utc> {
    let timestamp = at.timestamp();
    DateTime::from_timestamp(timestamp, 0).expect("valid rolling bucket timestamp")
}

fn matched_entity_and_rank(
    rule: &BudgetRule,
    request: &AuthorizeRequest,
    specificity: &[String],
) -> (Option<String>, usize) {
    let matched_entity = candidate_matched_entities(rule, request)
        .into_iter()
        .min_by_key(|entity| entity_specificity_rank(entity, specificity));
    let rank = matched_entity
        .as_deref()
        .map(|entity| entity_specificity_rank(entity, specificity))
        .unwrap_or(specificity.len());
    (matched_entity, rank)
}

fn candidate_matched_entities(rule: &BudgetRule, request: &AuthorizeRequest) -> Vec<String> {
    matched_entities_from_rule_match(&rule.rule_match, request)
}

fn matched_entities_from_rule_match(
    rule_match: &RuleMatch,
    request: &AuthorizeRequest,
) -> Vec<String> {
    let mut entities = Vec::new();
    if let Some(project) = rule_match.project.as_deref()
        && request_entity_matches(request, &format!("project:{project}"))
    {
        entities.push(format!("project:{project}"));
    }
    if let Some(user) = rule_match.user.as_deref()
        && request_entity_matches(request, &format!("user:{user}"))
    {
        entities.push(format!("user:{user}"));
    }
    if let Some(subject) = rule_match.subject.as_deref() {
        let entity = if subject.contains(':') {
            subject.to_owned()
        } else {
            format!("user:{subject}")
        };
        if request_entity_matches(request, &entity) {
            entities.push(entity);
        }
    }
    for (kind, value) in [
        ("team", rule_match.team.as_deref()),
        ("group", rule_match.group.as_deref()),
        ("org", rule_match.org.as_deref()),
        ("workflow", rule_match.workflow.as_deref()),
        ("surface", rule_match.surface.as_deref()),
    ] {
        if let Some(value) = value {
            let entity = format!("{kind}:{value}");
            if request_entity_matches(request, &entity) {
                entities.push(entity);
            }
        }
    }
    for nested in &rule_match.any {
        entities.extend(matched_entities_from_rule_match(nested, request));
    }
    if entities.is_empty() && rule_match == &RuleMatch::default() {
        entities.push("global".to_owned());
    }
    entities
}

fn request_entity_matches(request: &AuthorizeRequest, expected: &str) -> bool {
    if expected.eq_ignore_ascii_case("global") {
        return true;
    }
    request
        .entities
        .iter()
        .any(|entity| entity.eq_ignore_ascii_case(expected))
        || request
            .project
            .as_deref()
            .is_some_and(|project| expected.eq_ignore_ascii_case(&format!("project:{project}")))
        || request.subject.as_deref().is_some_and(|subject| {
            if subject.contains(':') {
                expected.eq_ignore_ascii_case(subject)
            } else {
                expected.eq_ignore_ascii_case(&format!("user:{subject}"))
            }
        })
}

fn entity_specificity_rank(entity: &str, specificity: &[String]) -> usize {
    let kind = entity
        .split_once(':')
        .map(|(kind, _)| kind)
        .unwrap_or(entity);
    specificity
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(kind))
        .unwrap_or(specificity.len())
}

fn routing_model_check(
    decision: &AuthorizeDecision,
    selected_budget_id: Option<&str>,
) -> Option<String> {
    if decision
        .explanations
        .iter()
        .any(|explanation| explanation.reason.contains("provider/model is not allowed"))
    {
        return Some("denied".to_owned());
    }

    selected_budget_id.map(|budget_id| format!("allowed:{budget_id}"))
}

struct DecisionSummary<'a> {
    action: &'a str,
    decision_id: &'a str,
    trace_id: Option<&'a str>,
    request_id: Option<&'a str>,
    provider: Option<&'a str>,
    model: Option<&'a str>,
    estimated_tokens: Option<i64>,
    estimated_cost_usd: Option<f64>,
    metadata_json: &'a str,
    limit_hits: Option<&'a [DecisionLimitHitReport]>,
    routing: DecisionRoutingSummary<'a>,
}

#[derive(Clone, Copy)]
struct DecisionRoutingSummary<'a> {
    selected_budget_id: Option<&'a str>,
    matched_entity: Option<&'a str>,
    selection_reason: Option<&'a str>,
    rejected_budget_id: Option<&'a str>,
    rejected_budget_reason: Option<&'a str>,
    model_check: Option<&'a str>,
    budget_window_remaining_usd: Option<f64>,
    budget_window_mode: Option<&'a str>,
    budget_window_started_at: Option<DateTime<Utc>>,
    budget_window_ends_at: Option<DateTime<Utc>>,
}

fn summarize_decision(decision: DecisionSummary<'_>) -> String {
    let metadata = serde_json::from_str::<Value>(decision.metadata_json).unwrap_or(Value::Null);
    let mut parts = vec![format!("decision_id={}", decision.decision_id)];
    parts.push(format!("action={}", decision.action));
    push_opt(&mut parts, "trace", decision.trace_id);
    push_opt(&mut parts, "request", decision.request_id);
    let model_ref = match (decision.provider, decision.model) {
        (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
        (None, Some(model)) => Some(model.to_owned()),
        (Some(provider), None) => Some(provider.to_owned()),
        (None, None) => None,
    };
    if let Some(model_ref) = model_ref {
        parts.push(format!("model={model_ref}"));
    }
    if let Some(tokens) = decision.estimated_tokens {
        parts.push(format!("estimated_tokens={}", tokens.max(0)));
    }
    if let Some(cost) = decision.estimated_cost_usd {
        parts.push(format!("estimated_cost={cost:.6}"));
    }
    push_opt(
        &mut parts,
        "selected_budget",
        decision.routing.selected_budget_id,
    );
    push_opt(
        &mut parts,
        "matched_entity",
        decision.routing.matched_entity,
    );
    push_opt(
        &mut parts,
        "selection_reason",
        decision.routing.selection_reason,
    );
    push_opt(
        &mut parts,
        "rejected_budget",
        decision.routing.rejected_budget_id,
    );
    push_opt(
        &mut parts,
        "rejected_reason",
        decision.routing.rejected_budget_reason,
    );
    push_opt(&mut parts, "model_check", decision.routing.model_check);
    if let Some(budget_window_remaining_usd) = decision.routing.budget_window_remaining_usd {
        parts.push(format!(
            "budget_window_remaining={budget_window_remaining_usd:.6}"
        ));
    }
    push_opt(
        &mut parts,
        "budget_window_mode",
        decision.routing.budget_window_mode,
    );
    if let Some(started_at) = decision.routing.budget_window_started_at {
        parts.push(format!("budget_window_start={}", started_at.to_rfc3339()));
    }
    if let Some(ends_at) = decision.routing.budget_window_ends_at {
        parts.push(format!("budget_window_end={}", ends_at.to_rfc3339()));
    }
    if let Some(limit_hits) = decision.limit_hits
        && !limit_hits.is_empty()
    {
        let hits = limit_hits
            .iter()
            .map(|hit| hit.rule_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("limit_hits={hits}"));
        let limit_ids = limit_hits
            .iter()
            .map(limit_hit_identifier)
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("limit_ids={limit_ids}"));
        if let Some(binding_limit) = binding_limit_hit(limit_hits) {
            parts.push(format!(
                "binding_limit={}",
                limit_hit_identifier(binding_limit)
            ));
        }
    }
    let shape = summarize_request_shape(&metadata);
    if !shape.is_empty() {
        parts.push(format!("shape={}", shape.join(",")));
    }
    push_value_u64(&mut parts, "context_window", metadata.get("context_window"));
    push_value_f64(
        &mut parts,
        "context_usage_pct",
        metadata.get("context_usage_percent"),
    );
    parts.join(" ")
}

fn summarize_event_payload(kind: &str, payload_json: &str) -> String {
    let payload = serde_json::from_str::<Value>(payload_json).unwrap_or(Value::Null);
    match kind {
        "pi.agent_context" => summarize_agent_context_payload(&payload),
        "pi.tool_call" => summarize_tool_call_payload(&payload),
        "pi.provider_call.started" => summarize_provider_call_payload(&payload),
        "pi.stream_summary" => summarize_stream_payload(&payload),
        "tool.observed" => summarize_tool_payload(&payload),
        "eval.annotation" => summarize_eval_payload(&payload),
        "pi.message_end" => summarize_usage_payload(&payload),
        "pi.turn_end" => summarize_turn_payload(&payload),
        "pi.agent_end" => summarize_agent_payload(&payload),
        _ => summarize_generic_payload(&payload),
    }
}

struct FinalizedUsageSummary<'a> {
    provider: Option<&'a str>,
    model: Option<&'a str>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    cost: Option<f64>,
    stop_reason: Option<&'a str>,
    metadata_json: &'a str,
}

fn summarize_finalized_usage(usage: FinalizedUsageSummary<'_>) -> String {
    let metadata = serde_json::from_str::<Value>(usage.metadata_json).unwrap_or(Value::Null);
    let details = metadata.get("usage_details").unwrap_or(&Value::Null);
    let mut parts = Vec::new();
    if let Some(provider) = usage.provider {
        parts.push(format!("provider={provider}"));
    }
    if let Some(model) = usage.model {
        parts.push(format!("model={model}"));
    }
    if let Some(tokens) = usage.input_tokens {
        parts.push(format!("input_tokens={}", tokens.max(0)));
    }
    if let Some(tokens) = usage.output_tokens {
        parts.push(format!("output_tokens={}", tokens.max(0)));
    }
    if let Some(tokens) = usage.total_tokens {
        parts.push(format!("total_tokens={}", tokens.max(0)));
    }
    push_value_u64(
        &mut parts,
        "cache_read_tokens",
        details.get("cache_read_tokens"),
    );
    push_value_u64(
        &mut parts,
        "cache_write_tokens",
        details.get("cache_write_tokens"),
    );
    if let Some(cost) = usage.cost {
        parts.push(format!("cost={cost:.6}"));
    }
    push_value_f64(
        &mut parts,
        "cache_read_cost",
        details.get("cache_read_cost_usd"),
    );
    push_value_f64(
        &mut parts,
        "cache_write_cost",
        details.get("cache_write_cost_usd"),
    );
    if let Some(stop_reason) = usage.stop_reason {
        parts.push(format!("stop={stop_reason}"));
    }
    summarize_parts_or_kind(parts, "usage")
}

fn summarize_provider_call_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    push_value_str(&mut parts, "provider", payload.get("provider"));
    push_value_str(&mut parts, "model", payload.get("model"));
    push_value_str(&mut parts, "provider_call", payload.get("provider_call_id"));
    push_value_u64(&mut parts, "context_tokens", payload.get("context_tokens"));
    push_value_u64(&mut parts, "context_window", payload.get("context_window"));
    push_value_f64(
        &mut parts,
        "context_usage_pct",
        payload.get("context_usage_percent"),
    );
    if let Some(summary) = payload.get("payload_summary") {
        let shape = summarize_request_shape(&serde_json::json!({ "payload_summary": summary }));
        if !shape.is_empty() {
            parts.push(format!("shape={}", shape.join(",")));
        }
    }
    summarize_attribution(&mut parts, payload);
    summarize_parts_or_kind(parts, "provider call")
}

fn summarize_agent_context_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    push_value_str(&mut parts, "cwd", payload.get("cwd"));
    push_array_len(&mut parts, "selected_tools", payload.get("selected_tools"));
    push_array_len(&mut parts, "skills", payload.get("skills"));
    push_array_len(&mut parts, "context_files", payload.get("context_files"));
    if let Some(names) = summarized_names(payload.get("selected_tools"), 3) {
        parts.push(format!("tool_names={names}"));
    }
    if let Some(names) = summarized_names(payload.get("skills"), 3) {
        parts.push(format!("skill_names={names}"));
    }
    summarize_parts_or_kind(parts, "agent context")
}

fn summarize_tool_call_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    push_value_str(&mut parts, "tool_name", payload.get("tool_name"));
    push_value_str(&mut parts, "tool_call_id", payload.get("tool_call_id"));
    push_value_str(&mut parts, "provider_call", payload.get("provider_call_id"));
    summarize_attribution(&mut parts, payload);
    summarize_parts_or_kind(parts, "tool call")
}

fn summarize_stream_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(counts) = payload.get("counts").and_then(Value::as_object) {
        let mut pairs: Vec<String> = counts
            .iter()
            .filter_map(|(key, value)| value.as_u64().map(|count| format!("{key}={count}")))
            .collect();
        pairs.sort();
        if !pairs.is_empty() {
            parts.push(format!("deltas={}", pairs.join(",")));
        }
    }
    if let Some(tool_count) = payload
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|values| values.len())
    {
        parts.push(format!("tool_calls={tool_count}"));
    }
    summarize_attribution(&mut parts, payload);
    summarize_parts_or_kind(parts, "stream")
}

fn summarize_tool_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    push_value_str(&mut parts, "name", payload.get("name"));
    push_value_bool(&mut parts, "success", payload.get("success"));
    push_value_u64(&mut parts, "duration_ms", payload.get("duration_ms"));
    push_value_str(&mut parts, "provider_call", payload.get("provider_call_id"));
    summarize_attribution(&mut parts, payload);
    if let Some(tool_call_id) = payload
        .get("metadata")
        .and_then(|metadata| metadata.get("tool_call_id"))
        .and_then(Value::as_str)
    {
        parts.push(format!("tool_call_id={tool_call_id}"));
    }
    summarize_parts_or_kind(parts, "tool observation")
}

fn summarize_eval_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    push_value_str(&mut parts, "label", payload.get("label"));
    push_value_f64(&mut parts, "score", payload.get("score"));
    push_value_str(&mut parts, "annotator", payload.get("annotator"));
    summarize_parts_or_kind(parts, "eval annotation")
}

fn summarize_usage_payload(payload: &Value) -> String {
    let usage = payload.get("usage").unwrap_or(payload);
    let mut parts = Vec::new();
    push_value_str(&mut parts, "provider", usage.get("provider"));
    push_value_str(&mut parts, "model", usage.get("model"));
    push_value_u64(&mut parts, "input_tokens", usage.get("input_tokens"));
    push_value_u64(&mut parts, "output_tokens", usage.get("output_tokens"));
    push_value_u64(&mut parts, "tokens", usage.get("total_tokens"));
    push_value_u64(
        &mut parts,
        "cache_read_tokens",
        usage.get("cache_read_tokens"),
    );
    push_value_u64(
        &mut parts,
        "cache_write_tokens",
        usage.get("cache_write_tokens"),
    );
    push_value_f64(&mut parts, "cost", usage.get("cost_usd"));
    push_value_str(&mut parts, "stop", usage.get("stop_reason"));
    push_value_str(&mut parts, "provider_call", payload.get("provider_call_id"));
    summarize_attribution(&mut parts, payload);
    summarize_parts_or_kind(parts, "usage")
}

fn summarize_turn_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    push_value_u64(&mut parts, "turn", payload.get("turn_index"));
    if let Some(usage) = payload.get("usage").and_then(Value::as_object) {
        push_value_str(&mut parts, "provider", usage.get("provider"));
        push_value_str(&mut parts, "model", usage.get("model"));
        push_value_u64(&mut parts, "usage_tokens", usage.get("total_tokens"));
        push_value_f64(&mut parts, "usage_cost", usage.get("cost_usd"));
    }
    summarize_parts_or_kind(parts, "turn")
}

fn summarize_agent_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    push_value_u64(&mut parts, "messages", payload.get("message_count"));
    push_value_u64(
        &mut parts,
        "provider_calls",
        payload.get("provider_call_count"),
    );
    if let Some(counts) = payload.get("attribution_counts").and_then(Value::as_object) {
        if let Some(value) = counts.get("fallback").and_then(Value::as_u64) {
            parts.push(format!("fallback_attribution={value}"));
        }
        if let Some(value) = counts.get("unmatched").and_then(Value::as_u64) {
            parts.push(format!("unmatched_attribution={value}"));
        }
    }
    summarize_parts_or_kind(parts, "agent")
}

fn summarize_generic_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    push_value_str(&mut parts, "source", payload.get("source"));
    push_value_str(&mut parts, "decision", payload.get("decision_id"));
    push_value_str(&mut parts, "reservation", payload.get("reservation_id"));
    push_value_str(&mut parts, "request", payload.get("request_id"));
    push_value_str(&mut parts, "provider_call", payload.get("provider_call_id"));
    summarize_attribution(&mut parts, payload);
    if let Some(object) = payload.as_object() {
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        if !keys.is_empty() {
            parts.push(format!("keys={}", keys.join(",")));
        }
    }
    summarize_parts_or_kind(parts, "event")
}

fn summarize_attribution(parts: &mut Vec<String>, payload: &Value) {
    if let Some(status) = payload.get("attribution_status").and_then(Value::as_str)
        && status != "exact"
    {
        parts.push(format!("attribution={status}"));
    }
}

fn summarize_request_shape(metadata: &Value) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(summary) = metadata.get("payload_summary") {
        if let Some(input_len) = summary
            .get("input")
            .and_then(|input| input.get("length"))
            .and_then(Value::as_u64)
        {
            parts.push(format!("input_count={input_len}"));
        }
        if let Some(tools_len) = summary
            .get("tools")
            .and_then(|tools| tools.get("length"))
            .and_then(Value::as_u64)
        {
            parts.push(format!("tools_count={tools_len}"));
        }
        if let Some(effort) = summary
            .get("reasoning")
            .and_then(|reasoning| reasoning.get("effort"))
            .and_then(Value::as_str)
        {
            parts.push(format!("reasoning_effort={effort}"));
        }
        if let Some(verbosity) = summary
            .get("text")
            .and_then(|text| text.get("verbosity"))
            .and_then(Value::as_str)
        {
            parts.push(format!("text_verbosity={verbosity}"));
        }
    }
    parts
}

fn push_opt(parts: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        parts.push(format!("{label}={value}"));
    }
}

fn push_value_str(parts: &mut Vec<String>, label: &str, value: Option<&Value>) {
    if let Some(value) = value.and_then(Value::as_str) {
        parts.push(format!("{label}={value}"));
    }
}

fn push_value_bool(parts: &mut Vec<String>, label: &str, value: Option<&Value>) {
    if let Some(value) = value.and_then(Value::as_bool) {
        parts.push(format!("{label}={value}"));
    }
}

fn push_value_u64(parts: &mut Vec<String>, label: &str, value: Option<&Value>) {
    if let Some(value) = value.and_then(Value::as_u64) {
        parts.push(format!("{label}={value}"));
    }
}

fn push_value_f64(parts: &mut Vec<String>, label: &str, value: Option<&Value>) {
    if let Some(value) = value.and_then(Value::as_f64) {
        parts.push(format!("{label}={value:.6}"));
    }
}

fn push_array_len(parts: &mut Vec<String>, label: &str, value: Option<&Value>) {
    if let Some(value) = value.and_then(Value::as_array) {
        parts.push(format!("{label}_count={}", value.len()));
    }
}

fn summarized_names(value: Option<&Value>, limit: usize) -> Option<String> {
    let values = value?.as_array()?;
    let names: Vec<&str> = values
        .iter()
        .filter_map(Value::as_str)
        .take(limit)
        .collect();
    (!names.is_empty()).then(|| names.join(","))
}

fn summarize_parts_or_kind(parts: Vec<String>, fallback: &str) -> String {
    if parts.is_empty() {
        fallback.to_owned()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use crate::config::{EnablementTipConfig, EnablementTipsConfig};
    use crate::contract::{
        BudgetLimitPolicy, BudgetModelPolicy, BudgetRule, ContextTokenLimit, RequestCostLimit,
        RuleMatch, SpendWindowBy, SpendWindowLimit, SpendWindowMode, WindowAnchorKind,
        WindowAnchorPolicy,
    };

    use super::*;

    fn sqlite_conn(ledger: &BudgetLedger) -> &Connection {
        let LedgerStore::Sqlite(conn) = &ledger.store else {
            panic!("expected sqlite ledger store");
        };
        conn
    }

    fn policy(limit_usd: f64, warn_at_fraction: f64) -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "dev-budget".to_owned(),
                priority: 0,
                models: Default::default(),
                limits: BudgetLimitPolicy {
                    request_cost: None,
                    context_tokens: None,
                    spend: vec![SpendWindowLimit {
                        by: SpendWindowBy::Project,
                        id: Some("budget-cap".to_owned()),
                        window: "60s".to_owned(),
                        mode: Some(SpendWindowMode::Tumbling),
                        anchor: Some(WindowAnchorPolicy {
                            kind: WindowAnchorKind::FirstSeen,
                        }),
                        max_usd: limit_usd,
                        warn_at_fractions: vec![warn_at_fraction],
                        action: PolicyAction::Block,
                        warning_cadence: None,
                    }],
                    tool_calls: None,
                    agent_steps: None,
                    retries: None,
                },
                allocation: None,
                rule_match: RuleMatch {
                    project: Some("noether".to_owned()),
                    ..RuleMatch::default()
                },
            }],
            policies: Vec::new(),
        }
    }

    fn request(cost: f64) -> AuthorizeRequest {
        AuthorizeRequest {
            budget_id: None,
            entities: Vec::new(),
            project: Some("noether".to_owned()),
            estimated_cost_usd: Some(cost),
            subject: None,
            provider: None,
            model: None,
            estimated_tokens: None,
            metadata: Default::default(),
        }
    }

    fn budget_cap_used(ledger: &BudgetLedger, budget_id: &str, scope_key: &str) -> f64 {
        ledger
            .limit_windows
            .get(&(
                budget_id.to_owned(),
                "budget-cap".to_owned(),
                scope_key.to_owned(),
            ))
            .map(|window| window.used_usd)
            .unwrap_or(0.0)
    }

    fn has_budget_cap(ledger: &BudgetLedger, budget_id: &str, scope_key: &str) -> bool {
        ledger.limit_windows.contains_key(&(
            budget_id.to_owned(),
            "budget-cap".to_owned(),
            scope_key.to_owned(),
        ))
    }

    fn first_message_hint_recommendation(decision: &AuthorizeDecision) -> Option<String> {
        decision
            .metadata
            .as_ref()?
            .get("message_hints")?
            .as_array()?
            .first()?
            .get("recommendation")?
            .as_str()
            .map(str::to_owned)
    }

    fn approval_audit_trace_event(trace_id: &str) -> TraceEvent {
        TraceEvent {
            id: None,
            trace_id: Some(trace_id.to_owned()),
            occurred_at: Some(Utc.with_ymd_and_hms(2026, 5, 29, 12, 0, 0).unwrap()),
            kind: "pi.authorize".to_owned(),
            payload: json!({
                "request": {
                    "subject": "user:alice",
                    "project": "noether",
                    "estimated_cost_usd": 1.25,
                    "metadata": {
                        "request_id": "req-approval",
                        "agent_run_id": "run-approval"
                    }
                },
                "decision_action": "ask",
                "policy_action": "approved",
                "user_approval": "approved",
                "decision_reason": "tool access requires explicit approval",
                "explanations": [
                    { "rule_id": "restricted-tool", "reason": "approval required" }
                ]
            }),
        }
    }

    fn first_notification(decision: &AuthorizeDecision) -> Option<&Value> {
        decision
            .metadata
            .as_ref()?
            .get("notifications")?
            .as_array()?
            .first()
    }

    fn protected_adoption_policy(cap_window: &str) -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "ai-adoption".to_owned(),
                priority: 0,
                models: BudgetModelPolicy::default(),
                limits: BudgetLimitPolicy {
                    request_cost: None,
                    context_tokens: None,
                    spend: vec![SpendWindowLimit {
                        id: Some("budget-cap".to_owned()),
                        by: SpendWindowBy::Org,
                        window: cap_window.to_owned(),
                        mode: Some(SpendWindowMode::Tumbling),
                        anchor: Some(WindowAnchorPolicy {
                            kind: WindowAnchorKind::FirstSeen,
                        }),
                        max_usd: 2000.0,
                        warn_at_fractions: vec![1.0],
                        action: PolicyAction::Block,
                        warning_cadence: None,
                    }],
                    tool_calls: None,
                    agent_steps: None,
                    retries: None,
                },
                allocation: Some(crate::contract::BudgetAllocationPolicy {
                    standard: "protected_adoption_pool".to_owned(),
                    by: Some("user".to_owned()),
                    protected_amount_usd: Some(25.0),
                    window: Some("monthly".to_owned()),
                    carryover: Some(crate::contract::ProtectedCarryoverPolicy {
                        percent: Some(10.0),
                        cap_usd: Some(50.0),
                    }),
                }),
                rule_match: RuleMatch {
                    org: Some("example".to_owned()),
                    ..RuleMatch::default()
                },
            }],
            policies: Vec::new(),
        }
    }

    #[test]
    fn budget_evaluator_allows_with_reservation_under_threshold() {
        let policy = policy(1.0, 0.8);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.25));

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert!(decision.reservation.is_some());
    }

    #[test]
    fn authorization_engine_evaluates_without_reserving_or_persisting_state() {
        let policy = policy(1.0, 0.8);
        let mut ledger = BudgetLedger::default();
        let now = Utc.with_ymd_and_hms(2026, 5, 29, 12, 0, 0).unwrap();

        let outcome = AuthorizationEngine::new(&mut ledger).evaluate_replay(
            Some(&policy),
            &request(0.25),
            now,
        );
        let snapshot = ledger.state_snapshot();

        assert_eq!(outcome.action, PolicyAction::Allow);
        assert_eq!(outcome.selected_budget_id.as_deref(), Some("dev-budget"));
        assert!(outcome.limit_hits.is_empty());
        assert!(snapshot.reservations.is_empty());
        assert!(snapshot.limit_windows.is_empty());
        assert!(snapshot.selected_budget_id.is_none());
    }

    #[test]
    fn ledger_state_snapshot_captures_authorization_reservation_state() {
        let policy = policy(1.0, 0.8);
        let mut ledger = BudgetLedger::default();
        let now = Utc.with_ymd_and_hms(2026, 5, 29, 12, 0, 0).unwrap();

        let decision = ledger
            .try_authorize_at(Some(&policy), &request(0.25), now)
            .expect("authorize");
        let reservation = decision.reservation.expect("reservation");
        let snapshot = ledger.state_snapshot();

        assert_eq!(snapshot.selected_budget_id.as_deref(), Some("dev-budget"));
        assert!(snapshot.reservations.contains_key(&reservation.id));
        assert!(snapshot.limit_hits.is_empty());
        assert_eq!(
            snapshot
                .limit_windows
                .get(&(
                    "dev-budget".to_owned(),
                    "budget-cap".to_owned(),
                    "project:noether".to_owned(),
                ))
                .map(|window| window.used_usd),
            Some(0.25)
        );
    }

    #[test]
    fn authorize_emits_enablement_notification_for_underpaced_budget_headroom() {
        let policy = policy(10.0, 0.8);
        let mut request = request(0.10);
        request.subject = Some("user:alice".to_owned());
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);
        let notification = first_notification(&decision).expect("enablement notification");

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert_eq!(notification["kind"], "enablement_tip");
        assert_eq!(notification["severity"], "info");
        assert_eq!(notification["title"], "AI budget headroom available");
        assert!(
            notification["body"]
                .as_str()
                .expect("body")
                .starts_with("Try ")
        );
    }

    #[test]
    fn authorize_computes_enablement_headroom_before_current_reservation() {
        let policy = policy(1.0, 0.8);
        let mut request = request(0.45);
        request.subject = Some("user:alice".to_owned());
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert!(
            first_notification(&decision).is_some(),
            "current reservation must not be counted twice when deciding whether headroom exists"
        );
    }

    #[test]
    fn authorize_can_extend_enablement_tip_catalog_from_config() {
        let mut config = AdvisoryConfig::default();
        config.enablement_tips.tips = vec![EnablementTipConfig {
            key: "custom.deep_dive".to_owned(),
            body: "Try the custom deep-dive workflow.".to_owned(),
        }];
        let now = Utc.with_ymd_and_hms(2026, 5, 30, 8, 0, 0).unwrap();

        let custom_tip_is_reachable = (0..32).any(|offset| {
            enablement_tip(
                &config,
                "user:alice",
                now + ENABLEMENT_NOTIFICATION_COOLDOWN * offset,
            )
            .is_some_and(|tip| tip.key == "custom.deep_dive")
        });

        assert!(custom_tip_is_reachable);
    }

    #[test]
    fn authorize_can_replace_enablement_tip_catalog_from_config() {
        let policy = policy(10.0, 0.8);
        let mut config = AdvisoryConfig::default();
        config.enablement_tips = EnablementTipsConfig {
            mode: TipCatalogMode::Replace,
            tips: vec![EnablementTipConfig {
                key: "custom.only_tip".to_owned(),
                body: "Try only the configured tip.".to_owned(),
            }],
        };
        let mut request = request(0.10);
        request.subject = Some("user:alice".to_owned());
        let mut ledger = BudgetLedger::default();
        ledger.set_advisory_config(config);

        let decision = ledger.authorize(Some(&policy), &request);
        let notification = first_notification(&decision).expect("enablement notification");

        assert_eq!(notification["key"], "custom.only_tip");
        assert_eq!(notification["body"], "Try only the configured tip.");
    }

    #[test]
    fn authorize_cadences_enablement_notifications_per_user() {
        let policy = policy(10.0, 0.8);
        let mut request = request(0.10);
        request.subject = Some("user:alice".to_owned());
        let mut ledger = BudgetLedger::default();

        let first = ledger.authorize(Some(&policy), &request);
        let second = ledger.authorize(Some(&policy), &request);

        assert!(first_notification(&first).is_some());
        assert!(first_notification(&second).is_none());
    }

    #[test]
    fn authorize_suppresses_enablement_notification_when_any_applicable_budget_is_over_pace() {
        let mut policy = policy(10.0, 0.8);
        let mut hot_budget = policy.budgets[0].clone();
        hot_budget.id = "hot-budget".to_owned();
        hot_budget.limits.spend[0].max_usd = 1.0;
        policy.budgets.push(hot_budget);
        let mut request = request(0.10);
        request.subject = Some("user:alice".to_owned());
        let mut ledger = BudgetLedger::default();
        ledger.limit_windows.insert(
            (
                "hot-budget".to_owned(),
                "budget-cap".to_owned(),
                "project:noether".to_owned(),
            ),
            WindowState {
                started_at: Utc::now(),
                used_usd: 0.80,
            },
        );

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert!(first_notification(&decision).is_none());
    }

    #[test]
    fn authorize_suppresses_enablement_notification_when_decision_warns() {
        let mut policy = policy(10.0, 0.8);
        policy.budgets[0].limits.request_cost = Some(RequestCostLimit {
            max_usd: 0.05,
            action: PolicyAction::Warn,
            warning_cadence: None,
        });
        let mut request = request(0.10);
        request.subject = Some("user:alice".to_owned());
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Warn);
        assert!(first_notification(&decision).is_none());
    }

    #[test]
    fn budget_evaluator_warns_at_threshold() {
        let policy = policy(1.0, 0.5);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.50));

        assert_eq!(decision.outcome, DecisionOutcome::Warn);
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.spend_window.budget-cap"
                && explanation.severity == DecisionSeverity::Warn
        }));
    }

    #[test]
    fn budget_evaluator_warns_at_default_full_threshold() {
        let policy = policy(1.0, 1.0);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(1.0));

        assert_eq!(decision.outcome, DecisionOutcome::Warn);
        assert!(decision.metadata.as_ref().is_some_and(|metadata| {
            metadata.to_string().contains("\"threshold_percent\":100")
        }));
    }

    #[test]
    fn budget_evaluator_warns_only_when_crossing_threshold() {
        let policy = policy(1.0, 0.5);
        let mut ledger = BudgetLedger::default();

        let first = ledger.authorize(Some(&policy), &request(0.50));
        let second = ledger.authorize(Some(&policy), &request(0.10));

        assert_eq!(first.outcome, DecisionOutcome::Warn);
        assert_eq!(second.outcome, DecisionOutcome::Allow);
    }

    #[test]
    fn budget_evaluator_warns_at_each_configured_threshold_once() {
        let mut policy = policy(1.0, 1.0);
        policy.budgets[0].limits.spend[0].warn_at_fractions = vec![0.5, 0.75, 0.9];
        let mut ledger = BudgetLedger::default();

        let fifty = ledger.authorize(Some(&policy), &request(0.50));
        let no_new_threshold = ledger.authorize(Some(&policy), &request(0.10));
        let seventy_five = ledger.authorize(Some(&policy), &request(0.20));
        let ninety = ledger.authorize(Some(&policy), &request(0.11));

        assert_eq!(fifty.outcome, DecisionOutcome::Warn);
        assert_eq!(no_new_threshold.outcome, DecisionOutcome::Allow);
        assert_eq!(seventy_five.outcome, DecisionOutcome::Warn);
        assert_eq!(ninety.outcome, DecisionOutcome::Warn);
        assert!(
            fifty
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.to_string().contains("\"threshold_percent\":50"))
        );
        assert!(
            seventy_five
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.to_string().contains("\"threshold_percent\":75"))
        );
        assert!(
            ninety
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.to_string().contains("\"threshold_percent\":90"))
        );
    }

    #[test]
    fn budget_evaluator_denies_over_limit() {
        let policy = policy(1.0, 0.8);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(1.01));

        assert_eq!(decision.outcome, DecisionOutcome::Deny);
        assert!(decision.reservation.is_none());
    }

    #[test]
    fn budget_evaluator_denies_model_disallowed_by_matching_budget() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].models = BudgetModelPolicy {
            allow: vec!["openai:gpt-4.1-mini".to_owned()],
        };
        let mut request = request(0.25);
        request.provider = Some("openai".to_owned());
        request.model = Some("gpt-4.1".to_owned());
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Deny);
        assert!(decision.reservation.is_none());
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget"
                && explanation.reason == "requested provider/model is not allowed by budget"
                && explanation.severity == DecisionSeverity::Deny
        }));
    }

    #[test]
    fn budget_limit_warns_on_expensive_single_request() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: Some(RequestCostLimit {
                max_usd: 0.20,
                action: PolicyAction::Warn,
                warning_cadence: None,
            }),
            context_tokens: None,
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.25));

        assert_eq!(decision.outcome, DecisionOutcome::Warn);
        assert!(decision.reservation.is_some());
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.request_cost"
                && explanation.reason
                    == "estimated request cost $0.250000 exceeds limit max $0.200000"
                && explanation.severity == DecisionSeverity::Warn
        }));
    }

    #[test]
    fn budget_limit_denies_expensive_single_request_when_enforced() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: Some(RequestCostLimit {
                max_usd: 0.20,
                action: PolicyAction::Block,
                warning_cadence: None,
            }),
            context_tokens: None,
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.25));

        assert_eq!(decision.outcome, DecisionOutcome::Deny);
        assert!(decision.reservation.is_none());
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.request_cost"
                && explanation.reason
                    == "estimated request cost $0.250000 exceeds enforced limit max $0.200000"
                && explanation.severity == DecisionSeverity::Deny
        }));
    }

    #[test]
    fn budget_limit_warns_on_large_context_estimate() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 1_000,
                action: PolicyAction::Warn,
                warning_cadence: None,
            }),
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut request = request(0.25);
        request.estimated_tokens = Some(1_200);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Warn);
        assert!(decision.reservation.is_some());
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.context_tokens"
                && explanation.reason == "estimated context tokens 1200 exceed limit max 1000"
                && explanation.severity == DecisionSeverity::Warn
        }));
    }

    #[test]
    fn budget_limit_cadences_repeated_context_warning_recommendations() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 100,
                action: PolicyAction::Warn,
                warning_cadence: None,
            }),
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut request = request(0.01);
        request.subject = Some("user:alice".to_owned());
        request.estimated_tokens = Some(101);
        let mut ledger = BudgetLedger::default();

        let first = ledger.authorize(Some(&policy), &request);
        let second = ledger.authorize(Some(&policy), &request);

        assert_eq!(first.outcome, DecisionOutcome::Warn);
        assert_eq!(second.outcome, DecisionOutcome::Warn);
        assert_eq!(
            first_message_hint_recommendation(&first).as_deref(),
            Some("show")
        );
        assert_eq!(
            first_message_hint_recommendation(&second).as_deref(),
            Some("hide")
        );
    }

    #[test]
    fn budget_limit_uses_noether_config_warning_cadence_fallback() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 100,
                action: PolicyAction::Warn,
                warning_cadence: None,
            }),
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut request = request(0.01);
        request.subject = Some("user:alice".to_owned());
        request.estimated_tokens = Some(101);
        let now = Utc.with_ymd_and_hms(2026, 5, 30, 8, 0, 0).unwrap();
        let mut ledger = BudgetLedger::default();
        ledger.set_advisory_config(AdvisoryConfig {
            warning_cadence: Some("10s".to_owned()),
            enablement_tips: Default::default(),
        });

        let first = ledger
            .try_authorize_at(Some(&policy), &request, now)
            .expect("first authorize");
        let hidden = ledger
            .try_authorize_at(Some(&policy), &request, now + Duration::seconds(5))
            .expect("second authorize");
        let shown_again = ledger
            .try_authorize_at(Some(&policy), &request, now + Duration::seconds(11))
            .expect("third authorize");

        assert_eq!(
            first_message_hint_recommendation(&first).as_deref(),
            Some("show")
        );
        assert_eq!(
            first_message_hint_recommendation(&hidden).as_deref(),
            Some("hide")
        );
        assert_eq!(
            first_message_hint_recommendation(&shown_again).as_deref(),
            Some("show")
        );
    }

    #[test]
    fn budget_limit_uses_per_warning_policy_cadence_override() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 100,
                action: PolicyAction::Warn,
                warning_cadence: Some("10s".to_owned()),
            }),
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut request = request(0.01);
        request.subject = Some("user:alice".to_owned());
        request.estimated_tokens = Some(101);
        let now = Utc.with_ymd_and_hms(2026, 5, 30, 8, 0, 0).unwrap();
        let mut ledger = BudgetLedger::default();
        ledger.set_advisory_config(AdvisoryConfig {
            warning_cadence: Some("1h".to_owned()),
            enablement_tips: Default::default(),
        });

        let first = ledger
            .try_authorize_at(Some(&policy), &request, now)
            .expect("first authorize");
        let shown_again = ledger
            .try_authorize_at(Some(&policy), &request, now + Duration::seconds(11))
            .expect("second authorize");

        assert_eq!(
            first_message_hint_recommendation(&first).as_deref(),
            Some("show")
        );
        assert_eq!(
            first_message_hint_recommendation(&shown_again).as_deref(),
            Some("show")
        );
    }

    #[test]
    fn spend_threshold_warning_uses_policy_cadence_override() {
        let mut policy = policy(1.0, 1.0);
        policy.budgets[0].limits.spend[0].warn_at_fractions = vec![0.5, 0.75];
        policy.budgets[0].limits.spend[0].warning_cadence = Some("1h".to_owned());
        let now = Utc.with_ymd_and_hms(2026, 5, 30, 8, 0, 0).unwrap();
        let mut ledger = BudgetLedger::default();

        let first = ledger
            .try_authorize_at(Some(&policy), &request(0.50), now)
            .expect("first threshold");
        let hidden = ledger
            .try_authorize_at(Some(&policy), &request(0.26), now + Duration::seconds(10))
            .expect("second threshold");

        assert_eq!(first.outcome, DecisionOutcome::Warn);
        assert_eq!(hidden.outcome, DecisionOutcome::Warn);
        assert_eq!(
            first_message_hint_recommendation(&first).as_deref(),
            Some("show")
        );
        assert_eq!(
            first_message_hint_recommendation(&hidden).as_deref(),
            Some("hide")
        );
    }

    #[test]
    fn spend_threshold_cadence_does_not_hide_simultaneous_limit_warning() {
        let mut policy = policy(1.0, 0.5);
        policy.budgets[0].limits.spend[0].action = PolicyAction::Warn;
        policy.budgets[0].limits.spend[0].warning_cadence = Some("1h".to_owned());
        let now = Utc.with_ymd_and_hms(2026, 5, 30, 8, 0, 0).unwrap();
        let mut ledger = BudgetLedger::default();

        let decision = ledger
            .try_authorize_at(Some(&policy), &request(1.10), now)
            .expect("authorize");
        let hints = decision
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("message_hints"))
            .and_then(Value::as_array)
            .expect("message hints");
        let recommendations: Vec<_> = hints
            .iter()
            .filter_map(|hint| hint.get("recommendation").and_then(Value::as_str))
            .collect();

        assert_eq!(decision.outcome, DecisionOutcome::Warn);
        assert_eq!(recommendations, vec!["show", "show"]);
    }

    #[test]
    fn sqlite_ledger_persists_advisory_cadence_across_reopen() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("advisory-cadence.sqlite");
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 100,
                action: PolicyAction::Warn,
                warning_cadence: None,
            }),
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut request = request(0.01);
        request.subject = Some("user:alice".to_owned());
        request.estimated_tokens = Some(101);

        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        let first = ledger
            .try_authorize(Some(&policy), &request)
            .expect("first authorize");
        drop(ledger);

        let mut reopened = BudgetLedger::open_sqlite(&db_path).expect("reopen sqlite");
        let second = reopened
            .try_authorize(Some(&policy), &request)
            .expect("second authorize");

        assert_eq!(
            first_message_hint_recommendation(&first).as_deref(),
            Some("show")
        );
        assert_eq!(
            first_message_hint_recommendation(&second).as_deref(),
            Some("hide")
        );
    }

    #[test]
    fn sqlite_ledger_exposes_persisted_approval_audit_events() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("approval-audit.sqlite");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        ledger
            .record_event(approval_audit_trace_event("trace-approval"))
            .expect("record approval event");
        drop(ledger);

        let reopened = BudgetLedger::open_sqlite(&db_path).expect("reopen sqlite");
        let report =
            crate::reporting::approval_audit_report(&reopened).expect("approval audit report");

        assert_eq!(report.summary.total, 1);
        assert_eq!(report.summary.approved, 1);
        assert_eq!(report.items[0].trace_id.as_deref(), Some("trace-approval"));
        assert_eq!(report.items[0].subject.as_deref(), Some("user:alice"));
        assert_eq!(report.items[0].project.as_deref(), Some("noether"));
        assert_eq!(report.items[0].rule_id.as_deref(), Some("restricted-tool"));
    }

    #[test]
    #[ignore = "requires NOET_TEST_POSTGRES_URL and an isolated PostgreSQL database"]
    fn postgres_ledger_persists_advisory_cadence_across_reopen() {
        let database_url = std::env::var("NOET_TEST_POSTGRES_URL").expect("NOET_TEST_POSTGRES_URL");
        let schema = format!("noether_test_{}", Uuid::new_v4().simple());
        let mut admin =
            PostgresClient::connect(&database_url, NoTls).expect("postgres admin connection");
        admin
            .batch_execute(&format!(
                r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE; CREATE SCHEMA "{schema}";"#
            ))
            .expect("create test schema");
        let separator = if database_url.contains('?') { '&' } else { '?' };
        let scoped_url = format!("{database_url}{separator}options=-csearch_path%3D{schema}");

        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 100,
                action: PolicyAction::Warn,
                warning_cadence: None,
            }),
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut request = request(0.01);
        request.subject = Some("user:alice".to_owned());
        request.estimated_tokens = Some(101);

        let mut ledger = BudgetLedger::open_postgres(&scoped_url).expect("postgres ledger");
        let first = ledger
            .try_authorize(Some(&policy), &request)
            .expect("first authorize");
        drop(ledger);

        let mut reopened = BudgetLedger::open_postgres(&scoped_url).expect("reopen postgres");
        let second = reopened
            .try_authorize(Some(&policy), &request)
            .expect("second authorize");

        assert_eq!(
            first_message_hint_recommendation(&first).as_deref(),
            Some("show")
        );
        assert_eq!(
            first_message_hint_recommendation(&second).as_deref(),
            Some("hide")
        );

        drop(reopened);
        admin
            .batch_execute(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE;"#))
            .expect("drop test schema");
    }

    #[test]
    #[ignore = "requires NOET_TEST_POSTGRES_URL and an isolated PostgreSQL database"]
    fn postgres_simulation_batch_persists_rolling_spend_buckets() {
        let database_url = std::env::var("NOET_TEST_POSTGRES_URL").expect("NOET_TEST_POSTGRES_URL");
        let schema = format!("noether_test_{}", Uuid::new_v4().simple());
        let mut admin =
            PostgresClient::connect(&database_url, NoTls).expect("postgres admin connection");
        admin
            .batch_execute(&format!(
                r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE; CREATE SCHEMA "{schema}";"#
            ))
            .expect("create test schema");
        let separator = if database_url.contains('?') { '&' } else { '?' };
        let scoped_url = format!("{database_url}{separator}options=-csearch_path%3D{schema}");

        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![SpendWindowLimit {
                by: SpendWindowBy::Project,
                id: Some("rolling".to_owned()),
                window: "7d".to_owned(),
                mode: Some(SpendWindowMode::Rolling),
                anchor: None,
                max_usd: 10.0,
                warn_at_fractions: vec![1.0],
                action: PolicyAction::Block,
                warning_cadence: None,
            }],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::default();
        let mut batch = SimulationLedgerBatch::default();
        let first_request = request(6.0);
        let first = ledger
            .try_authorize(Some(&policy), &first_request)
            .expect("first authorize");
        assert_eq!(first.outcome, DecisionOutcome::Allow);
        ledger
            .capture_simulation_decision(&mut batch, Some(&policy), &first_request, &first)
            .expect("capture decision");

        ledger
            .persist_simulation_batch_to_postgres_with_options(
                &scoped_url,
                &batch,
                &AsyncPostgresLedgerOptions::strict(),
            )
            .expect("persist simulation batch");

        let mut persisted =
            PostgresClient::connect(&scoped_url, NoTls).expect("postgres scoped connection");
        let bucket_amount = persisted
            .query_one(
                "
                SELECT COALESCE(SUM(amount_usd), 0)
                FROM rolling_spend_buckets
                WHERE rule_id = 'dev-budget'
                  AND limit_id = 'rolling'
                  AND scope_key = 'project:noether'
                ",
                &[],
            )
            .expect("rolling bucket row")
            .get::<_, f64>(0);
        assert_eq!(bucket_amount, 6.0);

        let mut reopened = BudgetLedger::open_postgres(&scoped_url).expect("reopen postgres");
        let second = reopened
            .try_authorize(Some(&policy), &request(5.0))
            .expect("second authorize");
        assert_eq!(second.outcome, DecisionOutcome::Deny);

        drop(reopened);
        drop(persisted);

        admin
            .batch_execute(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE;"#))
            .expect("drop test schema");
    }

    #[test]
    #[ignore = "requires NOET_TEST_POSTGRES_URL and an isolated PostgreSQL database"]
    fn postgres_ledger_exposes_persisted_approval_audit_events() {
        let database_url = std::env::var("NOET_TEST_POSTGRES_URL").expect("NOET_TEST_POSTGRES_URL");
        let schema = format!("noether_test_{}", Uuid::new_v4().simple());
        let mut admin =
            PostgresClient::connect(&database_url, NoTls).expect("postgres admin connection");
        admin
            .batch_execute(&format!(
                r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE; CREATE SCHEMA "{schema}";"#
            ))
            .expect("create test schema");
        let separator = if database_url.contains('?') { '&' } else { '?' };
        let scoped_url = format!("{database_url}{separator}options=-csearch_path%3D{schema}");

        let mut ledger = BudgetLedger::open_postgres(&scoped_url).expect("postgres ledger");
        ledger
            .record_event(approval_audit_trace_event("trace-postgres-approval"))
            .expect("record approval event");
        let report = crate::reporting::approval_audit_report(&ledger).expect("approval report");

        assert_eq!(report.summary.total, 1);
        assert_eq!(report.summary.approved, 1);
        assert_eq!(
            report.items[0].trace_id.as_deref(),
            Some("trace-postgres-approval")
        );

        drop(ledger);
        admin
            .batch_execute(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE;"#))
            .expect("drop test schema");
    }

    #[test]
    fn budget_limit_denies_large_context_estimate_when_enforced() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 1_000,
                action: PolicyAction::Block,
                warning_cadence: None,
            }),
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut request = request(0.25);
        request.estimated_tokens = Some(1_200);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Deny);
        assert!(decision.reservation.is_none());
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.context_tokens"
                && explanation.reason
                    == "estimated context tokens 1200 exceed enforced limit max 1000"
                && explanation.severity == DecisionSeverity::Deny
        }));
    }

    #[test]
    fn budget_limit_allows_missing_context_estimate() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 1_000,
                action: PolicyAction::Block,
                warning_cadence: None,
            }),
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.25));

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert!(decision.reservation.is_some());
        assert!(
            !decision
                .explanations
                .iter()
                .any(|explanation| { explanation.rule_id == "dev-budget.context_tokens" })
        );
    }

    #[test]
    fn spend_window_limit_warns_on_projected_recent_spend() {
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![SpendWindowLimit {
                by: SpendWindowBy::Project,
                id: None,
                window: "5m".to_owned(),
                mode: Some(SpendWindowMode::Rolling),
                anchor: None,
                max_usd: 10.0,
                warn_at_fractions: vec![1.0],
                action: PolicyAction::Warn,
                warning_cadence: None,
            }],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::default();

        let first = ledger.authorize(Some(&policy), &request(6.0));
        let second = ledger.authorize(Some(&policy), &request(5.0));

        assert_eq!(first.outcome, DecisionOutcome::Allow);
        assert_eq!(second.outcome, DecisionOutcome::Warn);
        assert!(second.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.spend_window.5m"
                && explanation.reason
                    == "projected spend $11.000000 exceeds 5m limit max $10.000000"
                && explanation.severity == DecisionSeverity::Warn
        }));
    }

    #[test]
    fn spend_window_limit_denies_on_projected_recent_spend() {
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![SpendWindowLimit {
                by: SpendWindowBy::Project,
                id: None,
                window: "7d".to_owned(),
                mode: Some(SpendWindowMode::Rolling),
                anchor: None,
                max_usd: 10.0,
                warn_at_fractions: vec![1.0],
                action: PolicyAction::Block,
                warning_cadence: None,
            }],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::default();

        ledger.authorize(Some(&policy), &request(6.0));
        let second = ledger.authorize(Some(&policy), &request(5.0));

        assert_eq!(second.outcome, DecisionOutcome::Deny);
        assert!(second.reservation.is_none());
        assert!(second.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.spend_window.7d"
                && explanation.reason
                    == "projected spend $11.000000 exceeds enforced 7d limit max $10.000000"
                && explanation.severity == DecisionSeverity::Deny
        }));
    }

    #[test]
    fn sqlite_rolling_spend_uses_persisted_scope_rollups() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("rolling-rollups.sqlite");
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![SpendWindowLimit {
                by: SpendWindowBy::Project,
                id: Some("rolling".to_owned()),
                window: "7d".to_owned(),
                mode: Some(SpendWindowMode::Rolling),
                anchor: None,
                max_usd: 10.0,
                warn_at_fractions: vec![1.0],
                action: PolicyAction::Block,
                warning_cadence: None,
            }],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let first = ledger.authorize(Some(&policy), &request(6.0));
        let second = ledger.authorize(Some(&policy), &request(5.0));

        assert_eq!(first.outcome, DecisionOutcome::Allow);
        assert_eq!(second.outcome, DecisionOutcome::Deny);
        let conn = sqlite_conn(&ledger);
        let row = conn
            .query_row(
                "
                SELECT amount_usd, created_at
                FROM reservation_limit_scopes
                WHERE reservation_id = ?1
                ",
                [first.reservation.as_ref().expect("reservation").id.as_str()],
                |row| Ok((row.get::<_, f64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .expect("scope rollup row");
        assert_eq!(row.0, 6.0);
        assert!(row.1.is_some());
        let bucket_amount = conn
            .query_row(
                "
                SELECT COALESCE(SUM(amount_usd), 0)
                FROM rolling_spend_buckets
                WHERE rule_id = 'dev-budget'
                  AND limit_id = 'rolling'
                  AND scope_key = 'project:noether'
                ",
                [],
                |row| row.get::<_, f64>(0),
            )
            .expect("rolling bucket amount");
        assert_eq!(bucket_amount, 6.0);
    }

    #[test]
    fn sqlite_rolling_spend_uses_second_buckets_with_conservative_boundary() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("rolling-edges.sqlite");
        let ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        let conn = sqlite_conn(&ledger);
        let rule_id = "dev-budget";
        let limit_id = "rolling";
        let scope_key = "project:noether";
        let now =
            Utc.with_ymd_and_hms(2026, 5, 28, 12, 10, 30).unwrap() + Duration::milliseconds(750);
        let since =
            Utc.with_ymd_and_hms(2026, 5, 28, 12, 0, 30).unwrap() + Duration::milliseconds(750);
        let out_of_window = Utc.with_ymd_and_hms(2026, 5, 28, 12, 0, 10).unwrap();
        let edge_since = Utc.with_ymd_and_hms(2026, 5, 28, 12, 0, 30).unwrap();
        let middle = Utc.with_ymd_and_hms(2026, 5, 28, 12, 1, 0).unwrap();
        let edge_now = Utc.with_ymd_and_hms(2026, 5, 28, 12, 10, 30).unwrap();

        for (reservation_id, amount, created_at) in [
            ("outside", 9.0, out_of_window),
            ("edge-since", 4.0, edge_since),
            ("middle", 1.0, middle),
            ("edge-now", 2.0, edge_now),
        ] {
            let decision_id = format!("{reservation_id}-decision");
            conn.execute(
                "
                INSERT INTO decisions (
                    decision_id, outcome, action, explanations_json, metadata_json,
                    entities_json, created_at
                ) VALUES (?1, 'allow', 'allow', '[]', '{}', '[]', ?2)
                ",
                params![decision_id.as_str(), created_at.to_rfc3339()],
            )
            .expect("insert decision row");
            conn.execute(
                "
                INSERT INTO reservations (
                    id, decision_id, amount_usd, estimated_amount_usd, currency, status,
                    created_at, expires_at
                ) VALUES (?1, ?2, ?3, ?3, 'USD', 'active', ?4, ?4)
                ",
                params![
                    reservation_id,
                    decision_id.as_str(),
                    amount,
                    created_at.to_rfc3339()
                ],
            )
            .expect("insert reservation row");
            conn.execute(
                "
                INSERT INTO reservation_limit_scopes (
                    reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ",
                params![
                    reservation_id,
                    rule_id,
                    limit_id,
                    scope_key,
                    amount,
                    created_at.to_rfc3339()
                ],
            )
            .expect("insert scope row");
        }

        for (bucket_start, amount) in [
            (rolling_bucket_start(out_of_window), 9.0),
            (rolling_bucket_start(edge_since), 4.0),
            (rolling_bucket_start(middle), 1.0),
            (rolling_bucket_start(edge_now), 2.0),
        ] {
            conn.execute(
                "
                INSERT INTO rolling_spend_buckets (
                    rule_id, limit_id, scope_key, bucket_start, amount_usd
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    rule_id,
                    limit_id,
                    scope_key,
                    bucket_start.to_rfc3339(),
                    amount
                ],
            )
            .expect("insert rolling bucket");
        }

        let spend = recent_spend_usd(&ledger, rule_id, limit_id, scope_key, since, now);

        assert_eq!(spend, 7.0);
    }

    #[test]
    fn tumbling_spend_window_limit_warns_on_projected_bucket_spend() {
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![SpendWindowLimit {
                by: SpendWindowBy::Project,
                id: Some("daily-tumbling".to_owned()),
                window: "1d".to_owned(),
                mode: Some(SpendWindowMode::Tumbling),
                anchor: Some(WindowAnchorPolicy {
                    kind: WindowAnchorKind::FirstSeen,
                }),
                max_usd: 10.0,
                warn_at_fractions: vec![1.0],
                action: PolicyAction::Warn,
                warning_cadence: None,
            }],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::default();

        let first = ledger.authorize(Some(&policy), &request(6.0));
        let second = ledger.authorize(Some(&policy), &request(5.0));

        assert_eq!(first.outcome, DecisionOutcome::Allow);
        assert_eq!(second.outcome, DecisionOutcome::Warn);
        assert!(second.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.spend_window.daily-tumbling"
                && explanation.reason
                    == "projected spend $11.000000 exceeds 1d limit max $10.000000"
                && explanation.severity == DecisionSeverity::Warn
        }));
    }

    #[test]
    fn tumbling_spend_window_limit_denies_on_projected_bucket_spend() {
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![SpendWindowLimit {
                by: SpendWindowBy::Project,
                id: Some("daily-tumbling".to_owned()),
                window: "1d".to_owned(),
                mode: Some(SpendWindowMode::Tumbling),
                anchor: Some(WindowAnchorPolicy {
                    kind: WindowAnchorKind::FirstSeen,
                }),
                max_usd: 10.0,
                warn_at_fractions: vec![1.0],
                action: PolicyAction::Block,
                warning_cadence: None,
            }],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::default();

        ledger.authorize(Some(&policy), &request(6.0));
        let second = ledger.authorize(Some(&policy), &request(5.0));

        assert_eq!(second.outcome, DecisionOutcome::Deny);
        assert!(second.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.spend_window.daily-tumbling"
                && explanation.reason
                    == "projected spend $11.000000 exceeds enforced 1d limit max $10.000000"
                && explanation.severity == DecisionSeverity::Deny
        }));
    }

    #[test]
    fn tumbling_and_rolling_spend_limits_of_same_duration_can_coexist() {
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![
                SpendWindowLimit {
                    by: SpendWindowBy::Project,
                    id: Some("daily-tumbling".to_owned()),
                    window: "1d".to_owned(),
                    mode: Some(SpendWindowMode::Tumbling),
                    anchor: Some(WindowAnchorPolicy {
                        kind: WindowAnchorKind::FirstSeen,
                    }),
                    max_usd: 10.0,
                    warn_at_fractions: vec![1.0],
                    action: PolicyAction::Warn,
                    warning_cadence: None,
                },
                SpendWindowLimit {
                    by: SpendWindowBy::Project,
                    id: Some("daily-rolling".to_owned()),
                    window: "1d".to_owned(),
                    mode: Some(SpendWindowMode::Rolling),
                    anchor: None,
                    max_usd: 10.0,
                    warn_at_fractions: vec![1.0],
                    action: PolicyAction::Warn,
                    warning_cadence: None,
                },
            ],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::default();

        ledger.authorize(Some(&policy), &request(6.0));
        let second = ledger.authorize(Some(&policy), &request(5.0));

        assert_eq!(second.outcome, DecisionOutcome::Warn);
        assert!(second.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.spend_window.daily-tumbling"
        }));
        assert!(
            second.explanations.iter().any(|explanation| {
                explanation.rule_id == "dev-budget.spend_window.daily-rolling"
            })
        );
    }

    #[test]
    fn sqlite_persists_tumbling_limit_windows_across_reopen() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("limit-window.sqlite");
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![SpendWindowLimit {
                by: SpendWindowBy::Project,
                id: Some("daily-tumbling".to_owned()),
                window: "1d".to_owned(),
                mode: Some(SpendWindowMode::Tumbling),
                anchor: Some(WindowAnchorPolicy {
                    kind: WindowAnchorKind::FirstSeen,
                }),
                max_usd: 10.0,
                warn_at_fractions: vec![1.0],
                action: PolicyAction::Block,
                warning_cadence: None,
            }],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let first = ledger.authorize(Some(&policy), &request(6.0));
        assert_eq!(first.outcome, DecisionOutcome::Allow);
        let persisted = sqlite_conn(&ledger)
            .query_row(
                "
                SELECT used_usd
                FROM limit_window_states
                WHERE rule_id = ?1 AND limit_id = ?2 AND scope_key = ?3
                ",
                ["dev-budget", "daily-tumbling", "project:noether"],
                |row| row.get::<_, f64>(0),
            )
            .expect("limit window row");
        assert_eq!(persisted, 6.0);
        drop(ledger);

        let mut reopened = BudgetLedger::open_sqlite(&db_path).expect("reopen sqlite");
        let second = reopened.authorize(Some(&policy), &request(5.0));

        assert_eq!(second.outcome, DecisionOutcome::Deny);
        assert!(second.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.spend_window.daily-tumbling"
        }));
    }

    #[test]
    fn tumbling_spend_windows_advance_by_whole_multiples_after_idle_gap() {
        let mut ledger = BudgetLedger::default();
        let rule = budget("tumbling-budget", 10.0, 0, ["project:noether"]);
        let started_at = Utc
            .with_ymd_and_hms(2026, 5, 20, 12, 0, 0)
            .single()
            .expect("valid time");
        ledger.limit_windows.insert(
            (
                rule.id.clone(),
                "budget-cap".to_owned(),
                "project:noether".to_owned(),
            ),
            WindowState {
                started_at,
                used_usd: 4.0,
            },
        );

        let now = started_at + Duration::seconds(130);
        let window = ledger.limit_window(
            &rule,
            "budget-cap",
            Duration::seconds(60),
            "project:noether",
            now,
        );

        assert_eq!(window.started_at, started_at + Duration::seconds(120));
        assert_eq!(window.used_usd, 0.0);
    }

    #[test]
    fn explicit_valid_budget_wins_and_only_selected_budget_is_reserved() {
        let policy = routing_policy([
            budget("project-budget", 1.0, 0, ["project:noether"]),
            budget("team-budget", 1.0, 0, ["team:core"]),
        ]);
        let mut request = request(0.25);
        request.budget_id = Some("team-budget".to_owned());
        request.entities = vec!["project:noether".to_owned(), "team:core".to_owned()];
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert_eq!(budget_cap_used(&ledger, "team-budget", "team:core"), 0.25);
        assert!(!has_budget_cap(
            &ledger,
            "project-budget",
            "project:noether"
        ));
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "team-budget"
                && explanation.reason == "selected requested budget"
        }));
    }

    #[test]
    fn invalid_explicit_budget_falls_back_to_inferred_budget() {
        let policy = routing_policy([budget("project-budget", 1.0, 0, ["project:noether"])]);
        let mut request = request(0.25);
        request.budget_id = Some("missing-budget".to_owned());
        request.entities = vec!["project:noether".to_owned()];
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert_eq!(
            budget_cap_used(&ledger, "project-budget", "project:noether"),
            0.25
        );
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "missing-budget"
                && explanation.reason == "requested budget does not exist"
        }));
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "project-budget"
                && explanation.reason == "selected fallback budget for project:noether"
        }));
    }

    #[test]
    fn fallback_inference_prefers_specificity_before_priority() {
        let policy = routing_policy([
            budget("team-budget", 1.0, 100, ["team:core"]),
            budget("project-budget", 1.0, 0, ["project:noether"]),
        ]);
        let mut request = request(0.25);
        request.entities = vec!["team:core".to_owned(), "project:noether".to_owned()];
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert_eq!(
            budget_cap_used(&ledger, "project-budget", "project:noether"),
            0.25
        );
        assert!(!has_budget_cap(&ledger, "team-budget", "team:core"));
    }

    #[test]
    fn fallback_inference_uses_priority_pressure_then_stable_id() {
        let policy = routing_policy([
            budget("z-low-priority", 1.0, 1, ["project:noether"]),
            budget("z-high-tight", 0.5, 10, ["project:noether"]),
            budget("b-high-wide", 1.0, 10, ["project:noether"]),
            budget("a-high-wide", 1.0, 10, ["project:noether"]),
        ]);
        let mut request = request(0.25);
        request.entities = vec!["project:noether".to_owned()];
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert_eq!(
            budget_cap_used(&ledger, "a-high-wide", "project:noether"),
            0.25
        );
        assert!(!has_budget_cap(&ledger, "b-high-wide", "project:noether"));
        assert!(!has_budget_cap(&ledger, "z-high-tight", "project:noether"));
        assert!(!has_budget_cap(
            &ledger,
            "z-low-priority",
            "project:noether"
        ));
    }

    #[test]
    fn sqlite_persists_budget_routing_explanation_fields() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("routing.sqlite");
        let policy = routing_policy([budget("project-budget", 1.0, 0, ["project:noether"])]);
        let mut request = request(0.25);
        request.budget_id = Some("missing-budget".to_owned());
        request.entities = vec!["project:noether".to_owned()];
        request.provider = Some("openai".to_owned());
        request.model = Some("gpt-4.1".to_owned());
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let decision = ledger.authorize(Some(&policy), &request);

        let conn = sqlite_conn(&ledger);
        let row = conn
            .query_row(
                "
                SELECT selected_budget_id, matched_entity, selection_reason, rejected_budget_id,
                       rejected_budget_reason, model_check, budget_window_remaining_usd
                FROM decisions
                WHERE decision_id = ?1
                ",
                [decision.decision_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<f64>>(6)?,
                    ))
                },
            )
            .expect("decision routing row");

        assert_eq!(row.0.as_deref(), Some("project-budget"));
        assert_eq!(row.1.as_deref(), Some("project:noether"));
        assert_eq!(
            row.2.as_deref(),
            Some("selected fallback budget for project:noether")
        );
        assert_eq!(row.3.as_deref(), Some("missing-budget"));
        assert_eq!(row.4.as_deref(), Some("requested budget does not exist"));
        assert_eq!(row.5.as_deref(), Some("allowed:project-budget"));
        assert_eq!(row.6, Some(0.75));
    }

    #[test]
    fn report_items_include_structured_routing_fields() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("routing-report.sqlite");
        let policy = routing_policy([budget("project-budget", 1.0, 0, ["project:noether"])]);
        let mut request = request(0.25);
        request.budget_id = Some("missing-budget".to_owned());
        request.entities = vec!["project:noether".to_owned()];
        request.provider = Some("openai".to_owned());
        request.model = Some("gpt-4.1".to_owned());
        request.metadata.insert(
            "trace_id".to_owned(),
            Value::String("trace-report".to_owned()),
        );
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let decision = ledger.authorize(Some(&policy), &request);
        assert_eq!(decision.outcome, DecisionOutcome::Allow);

        let decisions = ledger.decisions_report().expect("decisions report");
        assert_eq!(decisions.len(), 1);
        let routing = decisions[0].routing.as_ref().expect("decision routing");
        assert_eq!(
            routing.selected_budget_id.as_deref(),
            Some("project-budget")
        );
        assert_eq!(routing.matched_entity.as_deref(), Some("project:noether"));
        assert_eq!(
            routing.selection_reason.as_deref(),
            Some("selected fallback budget for project:noether")
        );
        assert_eq!(
            routing.rejected_budget_id.as_deref(),
            Some("missing-budget")
        );
        assert_eq!(
            routing.rejected_budget_reason.as_deref(),
            Some("requested budget does not exist")
        );
        assert_eq!(
            routing.model_check.as_deref(),
            Some("allowed:project-budget")
        );
        assert_eq!(routing.budget_window_remaining_usd, Some(0.75));

        let trace = ledger.trace_report("trace-report").expect("trace report");
        let trace_routing = trace.items[0].routing.as_ref().expect("trace routing");
        assert_eq!(
            trace_routing.selected_budget_id.as_deref(),
            Some("project-budget")
        );
    }

    #[test]
    fn sqlite_schema_indexes_active_reservations() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("active-reservations-index.sqlite");
        let ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        let conn = sqlite_conn(&ledger);

        let sql = conn
            .query_row(
                "
                SELECT sql
                FROM sqlite_master
                WHERE type = 'index' AND name = 'idx_reservations_active'
                ",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("active reservations index");

        assert!(sql.contains("WHERE status = 'active'"));
    }

    #[test]
    #[ignore = "requires NOET_TEST_POSTGRES_URL and an isolated PostgreSQL database"]
    fn postgres_schema_indexes_active_reservations() {
        let database_url = std::env::var("NOET_TEST_POSTGRES_URL").expect("NOET_TEST_POSTGRES_URL");
        let schema = format!("noether_test_{}", Uuid::new_v4().simple());
        let mut admin =
            PostgresClient::connect(&database_url, NoTls).expect("postgres admin connection");
        admin
            .batch_execute(&format!(
                r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE; CREATE SCHEMA "{schema}";"#
            ))
            .expect("create test schema");
        let separator = if database_url.contains('?') { '&' } else { '?' };
        let scoped_url = format!("{database_url}{separator}options=-csearch_path%3D{schema}");

        let ledger = BudgetLedger::open_postgres(&scoped_url).expect("postgres ledger");
        drop(ledger);

        let has_index: bool = admin
            .query_one(
                "
                SELECT EXISTS (
                    SELECT 1
                    FROM pg_class index_class
                    JOIN pg_namespace index_namespace
                      ON index_namespace.oid = index_class.relnamespace
                    JOIN pg_index index_metadata
                      ON index_metadata.indexrelid = index_class.oid
                    WHERE index_namespace.nspname = $1
                      AND index_class.relname = 'idx_reservations_active'
                      AND pg_get_expr(index_metadata.indpred, index_metadata.indrelid)
                        LIKE '%status = ''active''::text%'
                )
                ",
                &[&schema],
            )
            .expect("query active reservations index")
            .get(0);

        assert!(has_index);
        admin
            .batch_execute(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE;"#))
            .expect("drop test schema");
    }

    #[test]
    fn decisions_report_for_run_page_limits_runs_not_raw_decisions() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("run-page.sqlite");
        let policy = policy(100.0, 1.0);
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        for run_index in 0..5 {
            for _ in 0..3 {
                let mut request = request(0.01);
                request.metadata.insert(
                    "agent_run_id".to_owned(),
                    Value::String(format!("run-{run_index}")),
                );
                ledger.authorize(Some(&policy), &request);
            }
        }

        let page = ledger
            .decisions_report_for_run_page(2, 0)
            .expect("run page report");
        let run_ids = page
            .iter()
            .filter_map(|decision| decision.agent_run_id.as_deref())
            .collect::<std::collections::BTreeSet<_>>();

        assert!(
            page.len() > 2,
            "page should include all decisions for selected runs"
        );
        assert_eq!(run_ids.len(), 2);
    }

    #[test]
    fn decisions_report_for_run_page_groups_untraced_decisions_like_app_runs() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("untraced-run-page.sqlite");
        let policy = policy(100.0, 1.0);
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        for _ in 0..3 {
            ledger.authorize(Some(&policy), &request(0.01));
        }

        let page = ledger
            .decisions_report_for_run_page(1, 0)
            .expect("run page report");
        let totals = ledger.run_totals_report().expect("run totals");

        assert_eq!(
            page.len(),
            3,
            "the first run page should include all rows in the selected untraced run"
        );
        assert_eq!(totals.runs, 1);
    }

    #[test]
    fn report_items_include_limit_hits() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("limit-hits-report.sqlite");
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 1_000,
                action: PolicyAction::Block,
                warning_cadence: None,
            }),
            spend: Vec::new(),
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut request = request(0.25);
        request.estimated_tokens = Some(1_200);
        request.metadata.insert(
            "trace_id".to_owned(),
            Value::String("trace-limit".to_owned()),
        );
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let decision = ledger.authorize(Some(&policy), &request);
        assert_eq!(decision.outcome, DecisionOutcome::Deny);

        let decisions = ledger.decisions_report().expect("decisions report");
        let limit_hits = decisions[0]
            .limit_hits
            .as_ref()
            .expect("decision limit hits");
        assert_eq!(limit_hits.len(), 1);
        assert_eq!(limit_hits[0].rule_id, "dev-budget.context_tokens");
        assert_eq!(
            limit_hits[0].reason,
            "estimated context tokens 1200 exceed enforced limit max 1000"
        );
        assert!(
            decisions[0]
                .summary
                .contains("limit_hits=dev-budget.context_tokens")
        );

        let trace = ledger.trace_report("trace-limit").expect("trace report");
        let trace_limit_hits = trace.items[0]
            .limit_hits
            .as_ref()
            .expect("trace limit hits");
        assert_eq!(trace_limit_hits[0].rule_id, "dev-budget.context_tokens");
    }

    #[test]
    fn explicit_budget_window_metadata_is_exposed_in_decision_reports() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("budget-window-report.sqlite");
        let policy = policy(5.0, 1.0);
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let decision = ledger.authorize(Some(&policy), &request(0.25));
        assert_eq!(decision.outcome, DecisionOutcome::Allow);

        let decisions = ledger.decisions_report().expect("decisions report");
        let routing = decisions[0].routing.as_ref().expect("routing report");
        assert_eq!(routing.budget_window_mode.as_deref(), Some("tumbling"));
        assert!(routing.budget_window_started_at.is_some());
        assert_eq!(
            routing.budget_window_ends_at,
            routing
                .budget_window_started_at
                .map(|started_at| started_at + Duration::seconds(60))
        );
        assert!(decisions[0].summary.contains("budget_window_mode=tumbling"));
        assert!(decisions[0].summary.contains("budget_window_start="));
        assert!(decisions[0].summary.contains("budget_window_end="));
    }

    #[test]
    fn tumbling_limit_window_metadata_is_exposed_in_decision_reports() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("limit-window-report.sqlite");
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![SpendWindowLimit {
                by: SpendWindowBy::Project,
                id: Some("daily-tumbling".to_owned()),
                window: "1d".to_owned(),
                mode: Some(SpendWindowMode::Tumbling),
                anchor: Some(WindowAnchorPolicy {
                    kind: WindowAnchorKind::FirstSeen,
                }),
                max_usd: 10.0,
                warn_at_fractions: vec![1.0],
                action: PolicyAction::Block,
                warning_cadence: None,
            }],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        ledger.authorize(Some(&policy), &request(6.0));
        let denied = ledger.authorize(Some(&policy), &request(5.0));
        assert_eq!(denied.outcome, DecisionOutcome::Deny);

        let decisions = ledger.decisions_report().expect("decisions report");
        let limit_hit = decisions[0]
            .limit_hits
            .as_ref()
            .and_then(|hits| hits.first())
            .expect("limit hit");
        let routing = decisions[0].routing.as_ref().expect("routing report");
        assert_eq!(routing.selected_budget_id.as_deref(), Some("dev-budget"));
        assert_eq!(routing.matched_entity.as_deref(), Some("project:noether"));
        assert_eq!(routing.budget_window_mode.as_deref(), Some("tumbling"));
        assert!(routing.budget_window_started_at.is_some());
        assert_eq!(
            routing.budget_window_ends_at,
            routing
                .budget_window_started_at
                .map(|started_at| started_at + Duration::days(1))
        );
        assert_eq!(limit_hit.rule_id, "dev-budget.spend_window.daily-tumbling");
        assert_eq!(limit_hit.window_id.as_deref(), Some("daily-tumbling"));
        assert_eq!(limit_hit.window_mode.as_deref(), Some("tumbling"));
        assert_eq!(limit_hit.projected_spend_usd, Some(11.0));
        assert_eq!(limit_hit.max_usd, Some(10.0));
        assert_eq!(limit_hit.scope_entity.as_deref(), Some("project:noether"));
        assert!(limit_hit.window_started_at.is_some());
        assert_eq!(
            limit_hit.window_ends_at,
            limit_hit
                .window_started_at
                .map(|started_at| started_at + Duration::days(1))
        );
        assert!(
            decisions[0]
                .summary
                .contains("limit_hits=dev-budget.spend_window.daily-tumbling")
        );
        assert!(decisions[0].summary.contains("limit_ids=daily-tumbling"));
        assert!(
            decisions[0]
                .summary
                .contains("binding_limit=daily-tumbling")
        );
        assert!(decisions[0].summary.contains("selected_budget=dev-budget"));
        assert!(decisions[0].summary.contains("budget_window_mode=tumbling"));
    }

    #[test]
    fn binding_limit_hit_prefers_largest_overflow_within_same_severity() {
        let smaller = DecisionLimitHitReport {
            rule_id: "dev-budget.spend_window.daily".to_owned(),
            reason: "smaller overflow".to_owned(),
            severity: DecisionSeverity::Deny,
            window_id: Some("daily".to_owned()),
            window_mode: Some("tumbling".to_owned()),
            window_started_at: None,
            window_ends_at: None,
            projected_spend_usd: Some(11.0),
            max_usd: Some(10.0),
            scope_entity: Some("project:noether".to_owned()),
        };
        let larger = DecisionLimitHitReport {
            rule_id: "dev-budget.spend_window.burst".to_owned(),
            reason: "larger overflow".to_owned(),
            severity: DecisionSeverity::Deny,
            window_id: Some("burst".to_owned()),
            window_mode: Some("rolling".to_owned()),
            window_started_at: None,
            window_ends_at: None,
            projected_spend_usd: Some(18.0),
            max_usd: Some(10.0),
            scope_entity: Some("project:noether".to_owned()),
        };

        let hits = [smaller, larger];
        let selected = binding_limit_hit(&hits).expect("binding limit");
        assert_eq!(selected.window_id.as_deref(), Some("burst"));
    }

    #[test]
    fn trace_report_includes_report_only_lifecycle_limit_detections() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("lifecycle-limits.sqlite");
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: Vec::new(),
            tool_calls: Some(1),
            agent_steps: Some(1),
            retries: Some(1),
        };
        let mut request = request(1.0);
        request.metadata.insert(
            "trace_id".to_owned(),
            Value::String("trace-lifecycle".to_owned()),
        );
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let decision = ledger.authorize(Some(&policy), &request);
        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        for kind in [
            "pi.provider_call.started",
            "pi.provider_call.started",
            "pi.provider_call.started",
            "pi.provider_call.started",
            "pi.tool_call",
            "pi.tool_call",
            "pi.turn_end",
            "pi.turn_end",
        ] {
            ledger
                .record_event(TraceEvent {
                    id: None,
                    trace_id: Some("trace-lifecycle".to_owned()),
                    occurred_at: None,
                    kind: kind.to_owned(),
                    payload: Value::Object(Default::default()),
                })
                .expect("record event");
        }

        let trace = ledger
            .trace_report("trace-lifecycle")
            .expect("trace report");
        assert!(
            trace
                .items
                .iter()
                .any(|item| item.kind == "limit.report_only.tool_calls")
        );
        assert!(
            trace
                .items
                .iter()
                .any(|item| item.kind == "limit.report_only.agent_steps")
        );
        assert!(
            trace
                .items
                .iter()
                .any(|item| item.kind == "limit.report_only.retries")
        );
    }

    #[test]
    fn sqlite_persists_protected_adoption_buckets_across_reopen() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("protected-adoption.sqlite");
        let policy = protected_adoption_policy("60s");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        let mut request = request(0.25);
        request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];

        let decision = ledger.authorize(Some(&policy), &request);
        assert_eq!(decision.outcome, DecisionOutcome::Allow);

        let conn = sqlite_conn(&ledger);
        let row = conn
            .query_row(
                "
                SELECT rule_id, entity_key, current_grant_usd, carryover_usd
                FROM budget_allocation_buckets
                WHERE rule_id = ?1 AND entity_key = ?2
                ",
                ["ai-adoption", "user:alice"],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                },
            )
            .expect("protected adoption bucket row");
        assert_eq!(row.0, "ai-adoption");
        assert_eq!(row.1, "user:alice");
        assert_eq!(row.2, 24.75);
        assert_eq!(row.3, 0.0);

        drop(ledger);

        let reopened = BudgetLedger::open_sqlite(&db_path).expect("reopen sqlite");
        let conn = sqlite_conn(&reopened);
        let persisted = conn
            .query_row(
                "
                SELECT current_grant_usd, carryover_usd
                FROM budget_allocation_buckets
                WHERE rule_id = ?1 AND entity_key = ?2
                ",
                ["ai-adoption", "user:alice"],
                |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)),
            )
            .expect("reloaded protected adoption bucket");
        assert_eq!(persisted.0, 24.75);
        assert_eq!(persisted.1, 0.0);
    }

    #[test]
    fn protected_adoption_buckets_are_tracked_per_entity() {
        let policy = protected_adoption_policy("60s");
        let mut ledger = BudgetLedger::default();
        let mut alice_request = request(0.25);
        alice_request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];
        let mut bob_request = request(0.25);
        bob_request.entities = vec!["org:example".to_owned(), "user:bob".to_owned()];

        let alice = ledger.authorize(Some(&policy), &alice_request);
        let bob = ledger.authorize(Some(&policy), &bob_request);

        assert_eq!(alice.outcome, DecisionOutcome::Allow);
        assert_eq!(bob.outcome, DecisionOutcome::Allow);
        let alice_bucket = ledger
            .allocation_buckets
            .get(&("ai-adoption".to_owned(), "user:alice".to_owned()))
            .expect("alice bucket");
        let bob_bucket = ledger
            .allocation_buckets
            .get(&("ai-adoption".to_owned(), "user:bob".to_owned()))
            .expect("bob bucket");
        assert_eq!(alice_bucket.current_grant_usd, 24.75);
        assert_eq!(alice_bucket.carryover_usd, 0.0);
        assert_eq!(bob_bucket.current_grant_usd, 24.75);
        assert_eq!(bob_bucket.carryover_usd, 0.0);
    }

    #[test]
    fn protected_adoption_allocation_blocks_when_entity_grant_is_exhausted() {
        let policy = protected_adoption_policy("60s");
        let mut ledger = BudgetLedger::default();
        let mut request = request(26.0);
        request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Deny);
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "ai-adoption.allocation"
                && explanation
                    .reason
                    .contains("protected allocation available")
        }));
    }

    #[test]
    fn requested_budget_falls_back_when_protected_allocation_is_exhausted() {
        let mut policy = protected_adoption_policy("60s");
        let mut fallback = policy.budgets[0].clone();
        fallback.id = "fallback".to_owned();
        fallback.priority = 1;
        fallback.allocation = None;
        policy.budgets.push(fallback);
        let mut ledger = BudgetLedger::default();
        let mut request = request(26.0);
        request.budget_id = Some("ai-adoption".to_owned());
        request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "ai-adoption"
                && explanation
                    .reason
                    .contains("protected allocation available")
        }));
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "fallback"
                && explanation.reason == "selected fallback budget for org:example"
        }));
    }

    #[test]
    fn protected_adoption_spend_consumes_carryover_before_current_grant() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("carryover-first.sqlite");
        let policy = protected_adoption_policy("60s");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        let mut initial_request = request(0.25);
        initial_request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];
        ledger.authorize(Some(&policy), &initial_request);
        sqlite_conn(&ledger)
            .execute(
                "
                UPDATE budget_allocation_buckets
                SET current_grant_usd = 25.0, carryover_usd = 10.0
                WHERE rule_id = 'ai-adoption' AND entity_key = 'user:alice'
                ",
                [],
            )
            .expect("seed carryover");
        drop(ledger);

        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("reopen sqlite");
        let mut spend_request = request(12.0);
        spend_request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];

        let decision = ledger.authorize(Some(&policy), &spend_request);
        assert_eq!(decision.outcome, DecisionOutcome::Allow);

        let bucket = ledger
            .allocation_buckets
            .get(&("ai-adoption".to_owned(), "user:alice".to_owned()))
            .expect("alice bucket");
        assert_eq!(bucket.carryover_usd, 0.0);
        assert_eq!(bucket.current_grant_usd, 23.0);
    }

    #[test]
    fn protected_adoption_rollover_applies_before_next_window_spend() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("carryover-rollover.sqlite");
        let policy = protected_adoption_policy("1s");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        let mut initial_request = request(0.25);
        initial_request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];
        ledger.authorize(Some(&policy), &initial_request);
        sqlite_conn(&ledger)
            .execute(
                "
                UPDATE budget_allocation_buckets
                SET current_grant_usd = 23.0, carryover_usd = 10.0, started_at = '2000-01-01T00:00:00Z'
                WHERE rule_id = 'ai-adoption' AND entity_key = 'user:alice'
                ",
                [],
            )
            .expect("seed expired bucket");
        drop(ledger);

        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("reopen sqlite");
        let mut spend_request = request(5.0);
        spend_request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];

        let decision = ledger.authorize(Some(&policy), &spend_request);
        assert_eq!(decision.outcome, DecisionOutcome::Allow);

        let bucket = ledger
            .allocation_buckets
            .get(&("ai-adoption".to_owned(), "user:alice".to_owned()))
            .expect("alice bucket");
        assert!((bucket.carryover_usd - 7.3).abs() < 0.000_001);
        assert_eq!(bucket.current_grant_usd, 25.0);
    }

    #[test]
    fn usage_report_distinguishes_unused_opportunity_and_adoption_levels() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("adoption-report.sqlite");
        let policy = protected_adoption_policy("60s");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        let mut alice_request = request(1.0);
        alice_request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];
        let mut bob_request = request(24.0);
        bob_request.entities = vec!["org:example".to_owned(), "user:bob".to_owned()];
        ledger.authorize(Some(&policy), &alice_request);
        ledger.authorize(Some(&policy), &bob_request);
        sqlite_conn(&ledger)
            .execute(
                "
                UPDATE budget_allocation_buckets
                SET carryover_usd = 5.0
                WHERE rule_id = 'ai-adoption' AND entity_key = 'user:bob'
                ",
                [],
            )
            .expect("seed carryover liability");
        drop(ledger);

        let ledger = BudgetLedger::open_sqlite(&db_path).expect("reopen sqlite");
        let report = ledger.usage_report().expect("usage report");
        let adoption = report
            .protected_adoption
            .as_ref()
            .expect("protected adoption summary");
        assert_eq!(adoption.unused_protected_opportunity_usd, 25.0);
        assert_eq!(adoption.carryover_liability_usd, 5.0);
        assert!(
            adoption
                .low_adopters
                .iter()
                .any(|entity| entity.entity_key == "user:alice")
        );
        assert!(
            adoption
                .high_adopters
                .iter()
                .any(|entity| entity.entity_key == "user:bob")
        );
    }

    #[test]
    fn finalize_is_idempotent() {
        let policy = policy(1.0, 0.8);
        let mut ledger = BudgetLedger::default();
        let decision = ledger.authorize(Some(&policy), &request(0.25));
        let reservation_id = decision.reservation.expect("reservation").id;
        let payload = FinalizeReservation {
            reservation_id: None,
            outcome: crate::contract::FinalizeOutcome::Success,
            usage: None,
            actual_cost_usd: Some(0.20),
            metadata: Default::default(),
        };

        let first = ledger
            .finalize(&reservation_id, &payload)
            .expect("first finalize");
        let second = ledger
            .finalize(&reservation_id, &payload)
            .expect("second finalize");

        assert_eq!(first.status, ReservationStatus::Finalized);
        assert_eq!(second.amount_usd, 0.20);
    }

    #[test]
    fn finalize_rejects_invalid_accounting_values() {
        let policy = policy(1.0, 0.8);
        let mut ledger = BudgetLedger::default();
        let decision = ledger.authorize(Some(&policy), &request(0.25));
        let reservation_id = decision.reservation.expect("reservation").id;

        let error = ledger
            .finalize(
                &reservation_id,
                &FinalizeReservation {
                    reservation_id: None,
                    outcome: crate::contract::FinalizeOutcome::Success,
                    usage: None,
                    actual_cost_usd: Some(-0.20),
                    metadata: Default::default(),
                },
            )
            .expect_err("invalid finalize should fail");

        assert!(error.to_string().contains("actual_cost_usd"));
    }

    #[test]
    #[ignore = "requires NOET_TEST_POSTGRES_URL and an isolated PostgreSQL database"]
    fn postgres_ledger_persists_decision_reservation_usage_and_events() {
        let database_url = std::env::var("NOET_TEST_POSTGRES_URL").expect("NOET_TEST_POSTGRES_URL");
        let schema = format!("noether_test_{}", Uuid::new_v4().simple());
        let mut admin =
            PostgresClient::connect(&database_url, NoTls).expect("postgres admin connection");
        admin
            .batch_execute(&format!(
                r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE; CREATE SCHEMA "{schema}";"#
            ))
            .expect("create test schema");
        let separator = if database_url.contains('?') { '&' } else { '?' };
        let scoped_url = format!("{database_url}{separator}options=-csearch_path%3D{schema}");

        let mut ledger = BudgetLedger::open_postgres(&scoped_url).expect("postgres ledger");
        let policy = policy(1.0, 0.8);
        let mut request = request(0.25);
        request.metadata.insert(
            "trace_id".to_owned(),
            serde_json::Value::String("trace-postgres".to_owned()),
        );
        request.metadata.insert(
            "request_id".to_owned(),
            serde_json::Value::String("request-1".to_owned()),
        );
        let decision = ledger
            .try_authorize(Some(&policy), &request)
            .expect("authorize");
        let reservation_id = decision.reservation.expect("reservation").id;
        ledger
            .finalize(
                &reservation_id,
                &FinalizeReservation {
                    reservation_id: None,
                    outcome: crate::contract::FinalizeOutcome::Success,
                    usage: Some(UsageObservation {
                        provider: Some("openai".to_owned()),
                        model: Some("gpt-test".to_owned()),
                        input_tokens: Some(10),
                        output_tokens: Some(5),
                        total_tokens: Some(15),
                        cost_usd: Some(0.20),
                        latency_ms: Some(123),
                        stop_reason: Some("stop".to_owned()),
                    }),
                    actual_cost_usd: Some(0.20),
                    metadata: request.metadata.clone(),
                },
            )
            .expect("finalize");
        ledger
            .record_event(TraceEvent {
                id: None,
                trace_id: Some("trace-postgres".to_owned()),
                occurred_at: None,
                kind: "tool.observed".to_owned(),
                payload: serde_json::json!({"name": "shell", "success": true}),
            })
            .expect("event");

        let usage = ledger.usage_report().expect("usage report");
        assert_eq!(usage.total_cost_usd, 0.20);
        let decisions = ledger.decisions_report().expect("decisions report");
        assert_eq!(decisions.len(), 1);
        let trace = ledger.trace_report("trace-postgres").expect("trace report");
        assert_eq!(trace.items.len(), 3);

        drop(ledger);
        admin
            .batch_execute(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE;"#))
            .expect("drop test schema");
    }

    fn routing_policy<const N: usize>(budgets: [BudgetRule; N]) -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: budgets.into_iter().collect(),
            policies: Vec::new(),
        }
    }

    fn budget<const N: usize>(
        id: &str,
        limit_usd: f64,
        priority: i64,
        entities: [&str; N],
    ) -> BudgetRule {
        BudgetRule {
            id: id.to_owned(),
            priority,
            models: BudgetModelPolicy::default(),
            limits: BudgetLimitPolicy {
                request_cost: None,
                context_tokens: None,
                spend: vec![SpendWindowLimit {
                    id: Some("budget-cap".to_owned()),
                    by: spend_by_for_entity(entities[0]),
                    window: "60s".to_owned(),
                    mode: Some(SpendWindowMode::Tumbling),
                    anchor: Some(WindowAnchorPolicy {
                        kind: WindowAnchorKind::FirstSeen,
                    }),
                    max_usd: limit_usd,
                    warn_at_fractions: vec![1.0],
                    action: PolicyAction::Block,
                    warning_cadence: None,
                }],
                tool_calls: None,
                agent_steps: None,
                retries: None,
            },
            allocation: None,
            rule_match: rule_match_for_entity(entities[0]),
        }
    }

    fn spend_by_for_entity(entity: &str) -> SpendWindowBy {
        match entity
            .split_once(':')
            .map(|(kind, _)| kind)
            .unwrap_or(entity)
        {
            "project" => SpendWindowBy::Project,
            "user" => SpendWindowBy::User,
            "team" => SpendWindowBy::Team,
            "group" => SpendWindowBy::Group,
            "org" => SpendWindowBy::Org,
            "workflow" => SpendWindowBy::Workflow,
            "surface" => SpendWindowBy::Surface,
            "global" => SpendWindowBy::Global,
            other => panic!("unsupported test entity kind {other}"),
        }
    }

    fn rule_match_for_entity(entity: &str) -> RuleMatch {
        let Some((kind, value)) = entity.split_once(':') else {
            if entity.eq_ignore_ascii_case("global") {
                return RuleMatch::default();
            }
            panic!("unsupported test entity {entity}");
        };
        match kind {
            "project" => RuleMatch {
                project: Some(value.to_owned()),
                ..RuleMatch::default()
            },
            "user" => RuleMatch {
                user: Some(value.to_owned()),
                ..RuleMatch::default()
            },
            "team" => RuleMatch {
                team: Some(value.to_owned()),
                ..RuleMatch::default()
            },
            "group" => RuleMatch {
                group: Some(value.to_owned()),
                ..RuleMatch::default()
            },
            "org" => RuleMatch {
                org: Some(value.to_owned()),
                ..RuleMatch::default()
            },
            "workflow" => RuleMatch {
                workflow: Some(value.to_owned()),
                ..RuleMatch::default()
            },
            "surface" => RuleMatch {
                surface: Some(value.to_owned()),
                ..RuleMatch::default()
            },
            _ => panic!("unsupported test entity kind {kind}"),
        }
    }
}
