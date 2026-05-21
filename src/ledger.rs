use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, BudgetRule, DecisionExplanation, DecisionOutcome,
    DecisionSeverity, EvalAnnotation, FinalizeReservation, PolicyAction, Reservation,
    ReservationStatus, RuleMatch, SpendWindowLimit, SpendWindowMode, ToolEvent, TraceEvent,
    UsageObservation,
};
use crate::error::NoetError;
use crate::policy::{
    PolicyFile, budget_model_allowed, budget_rule_matches, budget_scope_matches,
    matching_policy_explanations, specificity_order,
};

#[derive(Debug, Default)]
pub struct BudgetLedger {
    limit_windows: HashMap<(String, String, String), WindowState>,
    allocation_buckets: HashMap<(String, String), AllocationBucketState>,
    reservations: HashMap<String, StoredReservation>,
    events: Vec<TraceEvent>,
    conn: Option<Connection>,
}

#[derive(Debug)]
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

#[derive(Debug)]
struct StoredReservation {
    reservation: Reservation,
    estimated_cost_usd: f64,
    budget_rule_ids: Vec<String>,
    limit_window_spends: Vec<LimitWindowReservationSpend>,
    allocation_spends: Vec<AllocationReservationSpend>,
    matched_entity: Option<String>,
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
    projected_spend_usd: f64,
    max_usd: f64,
    warn_at_fraction: f64,
    scope_entity: Option<String>,
    window_seconds: Duration,
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

#[derive(Debug, Serialize)]
pub struct UsageReport {
    pub total_cost_usd: f64,
    pub rows: Vec<UsageReportRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected_adoption: Option<ProtectedAdoptionReport>,
}

#[derive(Debug, Serialize)]
pub struct UsageReportRow {
    pub subject: Option<String>,
    pub project: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub cache_read_cost_usd: f64,
    pub cache_write_cost_usd: f64,
    pub total_cost_usd: f64,
    pub reservations: u64,
    pub active_reservations: u64,
    pub finalized_reservations: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageActivityRecord {
    pub occurred_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_budget_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_entity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProtectedAdoptionReport {
    pub unused_protected_opportunity_usd: f64,
    pub carryover_liability_usd: f64,
    pub low_adopters: Vec<ProtectedAdoptionEntityReport>,
    pub high_adopters: Vec<ProtectedAdoptionEntityReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProtectedAdoptionEntityReport {
    pub budget_id: String,
    pub entity_key: String,
    pub protected_amount_usd: f64,
    pub current_grant_usd: f64,
    pub carryover_usd: f64,
    pub used_current_grant_usd: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TraceReport {
    pub trace_id: String,
    pub items: Vec<TraceReportItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TraceReportItem {
    pub occurred_at: DateTime<Utc>,
    pub kind: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing: Option<DecisionRoutingReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_hits: Option<Vec<DecisionLimitHitReport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_limit: Option<DecisionLimitHitReport>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DecisionRoutingReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_budget_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_budget_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_budget_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_window_remaining_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_window_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_window_started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_window_ends_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DecisionLimitHitReport {
    pub rule_id: String,
    pub reason: String,
    pub severity: DecisionSeverity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_ends_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_spend_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_entity: Option<String>,
}

impl BudgetLedger {
    pub fn open_sqlite(path: &Path) -> Result<Self, NoetError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        init_schema(&conn)?;
        let mut ledger = Self {
            conn: Some(conn),
            ..Self::default()
        };
        ledger.load_limit_windows()?;
        ledger.load_allocation_buckets()?;
        ledger.load_active_reservations()?;
        Ok(ledger)
    }

    pub fn authorize(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
    ) -> AuthorizeDecision {
        self.try_authorize(policy, request)
            .expect("authorize decision persistence")
    }

    pub fn try_authorize(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
    ) -> Result<AuthorizeDecision, NoetError> {
        let now = Utc::now();
        let mut action = PolicyAction::Allow;
        let mut explanations = Vec::new();
        let mut limit_hits = Vec::new();
        let mut selected_budget_id = None;

        if let Some(policy) = policy {
            for (policy_action, explanation) in matching_policy_explanations(policy, request) {
                action = merge_policy_action(action, policy_action);
                explanations.push(explanation);
            }

            if !action.halts_request() {
                selected_budget_id = self.evaluate_budget_rules(
                    policy,
                    request,
                    now,
                    &mut action,
                    &mut explanations,
                    &mut limit_hits,
                );
            }
        } else {
            explanations.push(DecisionExplanation {
                rule_id: "no_policy".to_owned(),
                reason: "no policy file configured; request allowed".to_owned(),
                severity: DecisionSeverity::Info,
            });
        }

        let reservation = if action.halts_request() {
            None
        } else {
            Some(self.create_reservation(policy, request, now, selected_budget_id.as_deref()))
        };
        if reservation.is_some() {
            self.persist_limit_windows()?;
            self.persist_allocation_buckets()?;
        }

        let decision = AuthorizeDecision {
            decision_id: Uuid::new_v4().to_string(),
            outcome: action.decision_outcome(),
            action,
            reservation,
            explanations,
            created_at: now,
        };
        self.persist_decision(
            policy,
            request,
            &decision,
            selected_budget_id.as_deref(),
            &limit_hits,
        )?;
        Ok(decision)
    }

    pub fn finalize(
        &mut self,
        reservation_id: &str,
        payload: &FinalizeReservation,
    ) -> Result<Reservation, NoetError> {
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
        self.persist_finalization(&reservation, payload)?;
        self.persist_windows()?;
        self.persist_limit_windows()?;
        Ok(reservation)
    }

    pub fn record_event(&mut self, event: TraceEvent) -> Result<(), NoetError> {
        validate_event_payload(&event)?;
        self.persist_event(&event)?;
        self.events.push(event);
        Ok(())
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn usage_report(&self) -> Result<UsageReport, NoetError> {
        let Some(conn) = &self.conn else {
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
        let Some(conn) = &self.conn else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "
            SELECT created_at, outcome, decision_id, trace_id, request_id, provider, model,
                   action,
                   estimated_tokens, estimated_cost_usd, explanations_json, metadata_json, entities_json,
                   selected_budget_id, matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                   model_check, budget_window_remaining_usd, routing_json, limit_hits_json
            FROM decisions
            ORDER BY created_at DESC
            ",
        )?;
        stmt.query_map([], decision_report_item_from_row)?
            .collect::<Result<_, _>>()
            .map_err(NoetError::from)
    }

    pub fn usage_activity_report(&self) -> Result<Vec<UsageActivityRecord>, NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "
            SELECT u.created_at, COALESCE(u.trace_id, d.trace_id), d.subject, d.project,
                   COALESCE(u.provider, d.provider), COALESCE(u.model, d.model),
                   d.selected_budget_id, d.matched_entity, d.entities_json,
                   COALESCE(u.input_tokens, 0), COALESCE(u.output_tokens, 0),
                   COALESCE(CAST(json_extract(u.metadata_json, '$.usage_details.cache_read_tokens') AS INTEGER), 0),
                   COALESCE(CAST(json_extract(u.metadata_json, '$.usage_details.cache_write_tokens') AS INTEGER), 0),
                   COALESCE(u.total_tokens, 0),
                   COALESCE(u.cost_usd, r.actual_amount_usd, r.amount_usd, 0)
            FROM usage_observations u
            LEFT JOIN reservations r ON r.id = u.reservation_id
            LEFT JOIN decisions d ON d.decision_id = r.decision_id
            ORDER BY u.created_at DESC
            ",
        )?;
        stmt.query_map([], |row| {
            Ok(UsageActivityRecord {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                trace_id: row.get(1)?,
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
        })?
        .collect::<Result<_, _>>()
        .map_err(NoetError::from)
    }

    pub fn observations_report(
        &self,
        kind_prefix: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<Vec<TraceReportItem>, NoetError> {
        let Some(conn) = &self.conn else {
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
        let Some(conn) = &self.conn else {
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
                   model_check, budget_window_remaining_usd, routing_json, limit_hits_json
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
        action: &mut PolicyAction,
        explanations: &mut Vec<DecisionExplanation>,
        limit_hits: &mut Vec<DecisionLimitHitReport>,
    ) -> Option<String> {
        let estimated_cost = request.estimated_cost();
        let candidate = self.select_budget_rule(policy, request, now, explanations);

        let Some(candidate) = candidate else {
            let exhausted_rules = self.exhausted_budget_rules(policy, request, now);
            if !exhausted_rules.is_empty() {
                *action = merge_policy_action(*action, PolicyAction::Block);
                for hit in exhausted_rules {
                    explanations.push(DecisionExplanation {
                        rule_id: hit.rule_id.clone(),
                        reason: hit.reason.clone(),
                        severity: hit.severity,
                    });
                    limit_hits.push(hit);
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
                *action = merge_policy_action(*action, PolicyAction::Block);
                for rule in scoped_rules
                    .into_iter()
                    .filter(|rule| !budget_model_allowed(rule, request))
                {
                    explanations.push(DecisionExplanation {
                        rule_id: rule.id.clone(),
                        reason: "requested provider/model is not allowed by budget".to_owned(),
                        severity: DecisionSeverity::Deny,
                    });
                }
                return None;
            }
            if explanations
                .iter()
                .any(|explanation| explanation.rule_id == "no_fallback_budget")
            {
                *action = merge_policy_action(*action, PolicyAction::Block);
                return None;
            }
            explanations.push(DecisionExplanation {
                rule_id: "no_budget_match".to_owned(),
                reason: "no matching budget rule; request allowed".to_owned(),
                severity: DecisionSeverity::Info,
            });
            return None;
        };

        let Some(rule) = policy.budgets.iter().find(|rule| rule.id == candidate.id) else {
            return None;
        };
        if apply_budget_limits(
            self,
            rule,
            request,
            candidate.matched_entity.as_deref(),
            estimated_cost,
            now,
            action,
            explanations,
            limit_hits,
        ) {
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
        let (matched_entity, specificity_rank) =
            matched_entity_and_rank(rule, request, &specificity_order(policy));
        let projections =
            spend_window_projections(self, rule, matched_entity.as_deref(), estimated_cost, now);
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
        policy: &PolicyFile,
        rule: &BudgetRule,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> String {
        if !budget_scope_matches(rule, request) {
            return "requested budget is not eligible for request entities".to_owned();
        }
        if !budget_model_allowed(rule, request) {
            return "requested provider/model is not allowed by requested budget".to_owned();
        }
        let matched_entity = matched_entity_and_rank(rule, request, &specificity_order(policy)).0;
        if let Some(hit) = spend_window_projections(
            self,
            rule,
            matched_entity.as_deref(),
            request.estimated_cost(),
            now,
        )
        .into_iter()
        .filter(|projection| {
            projection.projected_spend_usd > projection.max_usd
                && matches!(projection.action, PolicyAction::Ask | PolicyAction::Block)
        })
        .max_by_key(|projection| {
            ((projection.projected_spend_usd / projection.max_usd) * 1_000_000.0).round() as u64
        })
        .map(|projection| spend_limit_hit(&projection))
        {
            return hit.reason;
        }
        "requested budget is not valid for the request".to_owned()
    }

    fn exhausted_budget_rules(
        &mut self,
        policy: &PolicyFile,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> Vec<DecisionLimitHitReport> {
        policy
            .budgets
            .iter()
            .filter(|rule| budget_rule_matches(rule, request))
            .flat_map(|rule| {
                let matched_entity = matched_entity_and_rank(rule, request, &specificity_order(policy)).0;
                spend_window_projections(
                    self,
                    rule,
                    matched_entity.as_deref(),
                    request.estimated_cost(),
                    now,
                )
                .into_iter()
                .filter(|projection| {
                    projection.projected_spend_usd > projection.max_usd
                        && matches!(projection.action, PolicyAction::Ask | PolicyAction::Block)
                })
                .map(|projection| spend_limit_hit(&projection))
            })
            .collect()
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
        let matched_entity = selected_budget_id.and_then(|selected_budget_id| {
            policy
                .and_then(|policy| {
                    policy
                        .budgets
                        .iter()
                        .find(|rule| rule.id == selected_budget_id)
                })
                .and_then(|rule| {
                    policy.map(|policy| {
                        matched_entity_and_rank(rule, request, &specificity_order(policy)).0
                    })?
                })
        });
        let mut allocation_spends = Vec::new();
        let mut limit_window_spends = Vec::new();
        let expires_at = matching_rules
            .iter()
            .flat_map(|rule| {
                spend_window_projections(
                    self,
                    rule,
                    matched_entity.as_deref(),
                    amount_usd,
                    now,
                )
                .into_iter()
                .map(|projection| projection.window_ends_at.unwrap_or(now + projection.window_seconds))
            })
            .min()
            .unwrap_or_else(|| now + Duration::hours(1));

        for rule in matching_rules {
            for limit in &rule.limits.spend {
                if !matches!(limit.mode, Some(SpendWindowMode::Tumbling)) {
                    continue;
                }
                let Some(window) = crate::policy::parse_limit_window(&limit.window) else {
                    continue;
                };
                let limit_id = spend_limit_identifier(limit).to_owned();
                let scope_key = limit_scope_key(matched_entity.as_deref());
                self.limit_window(rule, &limit_id, window, &scope_key, now)
                    .used_usd += amount_usd;
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
                matched_entity,
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

    fn biggest_spend_window_projection_for_budget(
        &self,
        rule: &BudgetRule,
        matched_entity: Option<&str>,
        estimated_cost: f64,
        now: DateTime<Utc>,
    ) -> Option<SpendWindowProjection> {
        biggest_spend_window_projection(self, rule, matched_entity, estimated_cost, now)
    }

    fn persist_decision(
        &self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        decision: &AuthorizeDecision,
        selected_budget_id: Option<&str>,
        limit_hits: &[DecisionLimitHitReport],
    ) -> Result<(), NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(());
        };
        let trace_id = string_metadata(request, "trace_id");
        let session_id = string_metadata(request, "session_id");
        let request_id = string_metadata(request, "request_id");
        let routing =
            self.routing_persistence_fields(policy, request, decision, selected_budget_id);
        let routing_report = decision_routing_report(
            routing.selected_budget_id.clone(),
            routing.matched_entity.clone(),
            routing.selection_reason.clone(),
            routing.rejected_budget_id.clone(),
            routing.rejected_budget_reason.clone(),
            routing.model_check.clone(),
            routing.budget_window_remaining_usd,
            routing.budget_window_mode.clone(),
            routing.budget_window_started_at,
            routing.budget_window_ends_at,
        );
        conn.execute(
            "
            INSERT INTO decisions (
                decision_id, trace_id, session_id, request_id, subject, project, provider, model,
                estimated_tokens, estimated_cost_usd, outcome, action, explanations_json, metadata_json,
                entities_json, selected_budget_id, matched_entity, selection_reason, rejected_budget_id,
                rejected_budget_reason, model_check, budget_window_remaining_usd, routing_json,
                limit_hits_json, max_tool_calls, max_agent_steps, max_retries, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28
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
                outcome_text(decision.outcome),
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
                decision.created_at.to_rfc3339(),
            ],
        )?;
        if let Some(reservation) = &decision.reservation {
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
                    serde_json::to_string(
                        &self
                            .reservations
                            .get(&reservation.id)
                            .map(|stored| stored.limit_window_spends.as_slice())
                            .unwrap_or_default(),
                    )?,
                    serde_json::to_string(
                        &self
                            .reservations
                            .get(&reservation.id)
                            .map(|stored| stored.allocation_spends.as_slice())
                            .unwrap_or_default(),
                    )?,
                ],
            )?;
        }
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
                if let Some(projection) = biggest_spend_window_projection(
                    self,
                    rule,
                    fields.matched_entity.as_deref(),
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

        if let Some(requested_budget_id) = request.budget_id.as_deref() {
            if selected_budget_id.as_deref() != Some(requested_budget_id) {
                fields.rejected_budget_id = Some(requested_budget_id.to_owned());
                fields.rejected_budget_reason = decision
                    .explanations
                    .iter()
                    .find(|explanation| explanation.rule_id == requested_budget_id)
                    .map(|explanation| explanation.reason.clone());
            }
        }

        fields.model_check = routing_model_check(decision, selected_budget_id.as_deref());
        fields
    }

    fn lifecycle_limit_report_items(
        &self,
        trace_id: &str,
    ) -> Result<Option<Vec<TraceReportItem>>, NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        let config = conn
            .query_row(
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
            .optional()?;
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
                entities: Vec::new(),
                routing: None,
                limit_hits: None,
                binding_limit: None,
            });
        }
        Ok((!items.is_empty()).then_some(items))
    }

    fn event_count_for_trace(&self, trace_id: &str, kind: &str) -> Result<u64, NoetError> {
        let Some(conn) = &self.conn else {
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
        let Some(conn) = &self.conn else {
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

    fn persist_event(&self, event: &TraceEvent) -> Result<(), NoetError> {
        let Some(conn) = &self.conn else {
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
        let Some(conn) = &self.conn else {
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
        let Some(conn) = &self.conn else {
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

    fn load_windows(&mut self) -> Result<(), NoetError> {
        Ok(())
    }

    fn load_limit_windows(&mut self) -> Result<(), NoetError> {
        let Some(conn) = &self.conn else {
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
        let Some(conn) = &self.conn else {
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

    fn load_active_reservations(&mut self) -> Result<(), NoetError> {
        let Some(conn) = &self.conn else {
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
                        matched_entity: None,
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
    let routing = routing_json
        .as_deref()
        .and_then(parse_optional_json::<DecisionRoutingReport>)
        .or_else(|| {
            decision_routing_report(
                selected_budget_id.clone(),
                matched_entity.clone(),
                selection_reason.clone(),
                rejected_budget_id.clone(),
                rejected_budget_reason.clone(),
                model_check.clone(),
                budget_window_remaining_usd,
                None,
                None,
                None,
            )
        });
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
                .or(selected_budget_id.as_deref()),
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
        entities: parse_entities_json(entities_json),
        binding_limit: limit_hits.as_deref().and_then(binding_limit_hit).cloned(),
        routing,
        limit_hits,
    })
}

fn decision_routing_report(
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
) -> Option<DecisionRoutingReport> {
    let has_fields = selected_budget_id.is_some()
        || matched_entity.is_some()
        || selection_reason.is_some()
        || rejected_budget_id.is_some()
        || rejected_budget_reason.is_some()
        || model_check.is_some()
        || budget_window_remaining_usd.is_some()
        || budget_window_mode.is_some()
        || budget_window_started_at.is_some()
        || budget_window_ends_at.is_some();
    has_fields.then_some(DecisionRoutingReport {
        selected_budget_id,
        matched_entity,
        selection_reason,
        rejected_budget_id,
        rejected_budget_reason,
        model_check,
        budget_window_remaining_usd,
        budget_window_mode,
        budget_window_started_at,
        budget_window_ends_at,
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

fn is_limit_rule_id(rule_id: &str) -> bool {
    rule_id.contains(".request_cost")
        || rule_id.contains(".context_tokens")
        || rule_id.contains(".spend_window.")
}

fn apply_budget_limits(
    ledger: &BudgetLedger,
    rule: &BudgetRule,
    request: &AuthorizeRequest,
    matched_entity: Option<&str>,
    estimated_cost: f64,
    now: DateTime<Utc>,
    action: &mut PolicyAction,
    explanations: &mut Vec<DecisionExplanation>,
    limit_hits: &mut Vec<DecisionLimitHitReport>,
) -> bool {
    if let Some(limit) = &rule.limits.request_cost
        && estimated_cost > limit.max_usd
        && push_limit_explanation(
            format!("{}.request_cost", rule.id),
            format!(
                "estimated request cost ${estimated_cost:.6} exceeds limit max ${:.6}",
                limit.max_usd
            ),
            format!(
                "estimated request cost ${estimated_cost:.6} exceeds enforced limit max ${:.6}",
                limit.max_usd
            ),
            limit.action,
            action,
            explanations,
        )
    {
        return true;
    }

    if let Some(limit) = &rule.limits.context_tokens
        && let Some(estimated_tokens) = request.estimated_tokens
        && estimated_tokens > limit.max_tokens
        && push_limit_explanation(
            format!("{}.context_tokens", rule.id),
            format!(
                "estimated context tokens {estimated_tokens} exceed limit max {}",
                limit.max_tokens
            ),
            format!(
                "estimated context tokens {estimated_tokens} exceed enforced limit max {}",
                limit.max_tokens
            ),
            limit.action,
            action,
            explanations,
        )
    {
        return true;
    }

    for projection in spend_window_projections(ledger, rule, matched_entity, estimated_cost, now) {
        let warn_threshold = projection.max_usd * projection.warn_at_fraction;
        if projection.warn_at_fraction < 1.0 && projection.projected_spend_usd >= warn_threshold {
            *action = merge_policy_action(*action, PolicyAction::Warn);
            explanations.push(DecisionExplanation {
                rule_id: projection.rule_id.clone(),
                reason: format!(
                    "projected spend ${:.6} reaches warning threshold ${:.6} for {} window",
                    projection.projected_spend_usd, warn_threshold, projection.window_label
                ),
                severity: DecisionSeverity::Warn,
            });
        }
        if projection.projected_spend_usd > projection.max_usd {
            let hit = spend_limit_hit(&projection);
            let denied = push_limit_explanation(
                hit.rule_id.clone(),
                hit.reason.clone(),
                hit.reason.clone(),
                projection.action,
                action,
                explanations,
            );
            limit_hits.push(hit);
            if denied {
                return true;
            }
        }
    }

    false
}

fn spend_window_projections(
    ledger: &BudgetLedger,
    rule: &BudgetRule,
    matched_entity: Option<&str>,
    estimated_cost: f64,
    now: DateTime<Utc>,
) -> Vec<SpendWindowProjection> {
    rule.limits
        .spend
        .iter()
        .filter_map(|limit| {
            let window_seconds = crate::policy::parse_limit_window(&limit.window)?;
            let limit_id = spend_limit_identifier(limit).to_owned();
            let limit_mode = limit.mode?;
            let scope_key = limit_scope_key(matched_entity);
            let (current_spend, window_started_at, window_ends_at) = match limit_mode {
                SpendWindowMode::Rolling => (
                    recent_spend_usd(ledger, &rule.id, matched_entity, now - window_seconds, now),
                    Some(now - window_seconds),
                    Some(now),
                ),
                SpendWindowMode::Tumbling => {
                    let key = (rule.id.clone(), limit_id.clone(), scope_key.clone());
                    let started_at = ledger
                        .limit_windows
                        .get(&key)
                        .map(|state| {
                            if now - state.started_at >= window_seconds {
                                advance_tumbling_window_start(state.started_at, window_seconds, now)
                            } else {
                                state.started_at
                            }
                        })
                        .unwrap_or(now);
                    (
                        ledger.limit_window_used_usd(
                            rule,
                            &limit_id,
                            window_seconds,
                            &scope_key,
                            now,
                        ),
                        Some(started_at),
                        Some(started_at + window_seconds),
                    )
                }
            };
            Some(SpendWindowProjection {
                rule_id: format!("{}.spend_window.{}", rule.id, limit_id),
                limit_id,
                window_label: limit.window.clone(),
                action: limit.action,
                limit_mode,
                window_started_at,
                window_ends_at,
                projected_spend_usd: current_spend + estimated_cost,
                max_usd: limit.max_usd,
                warn_at_fraction: limit.warn_at_fraction,
                scope_entity: matched_entity.map(ToOwned::to_owned),
                window_seconds,
            })
        })
        .collect()
}

fn biggest_spend_window_projection(
    ledger: &BudgetLedger,
    rule: &BudgetRule,
    matched_entity: Option<&str>,
    estimated_cost: f64,
    now: DateTime<Utc>,
) -> Option<SpendWindowProjection> {
    spend_window_projections(ledger, rule, matched_entity, estimated_cost, now)
        .into_iter()
        .max_by_key(|projection| projection.window_seconds.num_seconds())
}

fn biggest_spend_window_duration(rule: &BudgetRule) -> Option<Duration> {
    rule.limits
        .spend
        .iter()
        .filter_map(|limit| crate::policy::parse_limit_window(&limit.window))
        .max_by_key(|window| window.num_seconds())
}

fn spend_limit_hit(projection: &SpendWindowProjection) -> DecisionLimitHitReport {
    let reason = match projection.action {
        PolicyAction::Warn => format!(
            "projected spend ${:.6} exceeds {} limit max ${:.6}",
            projection.projected_spend_usd, projection.window_label, projection.max_usd
        ),
        PolicyAction::Ask | PolicyAction::Block => format!(
            "projected spend ${:.6} exceeds enforced {} limit max ${:.6}",
            projection.projected_spend_usd, projection.window_label, projection.max_usd
        ),
        PolicyAction::Allow => unreachable!("limit validation forbids allow actions"),
    };
    DecisionLimitHitReport {
        rule_id: projection.rule_id.clone(),
        reason,
        severity: match projection.action {
            PolicyAction::Warn => DecisionSeverity::Warn,
            PolicyAction::Ask | PolicyAction::Block => DecisionSeverity::Deny,
            PolicyAction::Allow => unreachable!("limit validation forbids allow actions"),
        },
        window_id: Some(projection.limit_id.clone()),
        window_mode: Some(match projection.limit_mode {
            SpendWindowMode::Rolling => "rolling".to_owned(),
            SpendWindowMode::Tumbling => "tumbling".to_owned(),
        }),
        window_started_at: projection.window_started_at,
        window_ends_at: projection.window_ends_at,
        projected_spend_usd: Some(projection.projected_spend_usd),
        max_usd: Some(projection.max_usd),
        scope_entity: projection.scope_entity.clone(),
    }
}

fn push_limit_explanation(
    rule_id: String,
    warn_reason: String,
    deny_reason: String,
    action: PolicyAction,
    current_action: &mut PolicyAction,
    explanations: &mut Vec<DecisionExplanation>,
) -> bool {
    let (severity, reason, denied) = match action {
        PolicyAction::Warn => (DecisionSeverity::Warn, warn_reason, false),
        PolicyAction::Ask | PolicyAction::Block => (DecisionSeverity::Deny, deny_reason, true),
        PolicyAction::Allow => unreachable!("limit validation forbids allow actions"),
    };
    *current_action = merge_policy_action(*current_action, action);
    explanations.push(DecisionExplanation {
        rule_id,
        reason,
        severity,
    });
    denied
}

fn spend_limit_identifier(limit: &crate::contract::SpendWindowLimit) -> &str {
    limit.id.as_deref().unwrap_or(limit.window.as_str())
}

fn limit_hit_identifier(hit: &DecisionLimitHitReport) -> &str {
    hit.window_id.as_deref().unwrap_or(hit.rule_id.as_str())
}

fn limit_hit_overflow(hit: &DecisionLimitHitReport) -> Option<f64> {
    Some(hit.projected_spend_usd? - hit.max_usd?)
}

fn limit_hit_severity_rank(hit: &DecisionLimitHitReport) -> u8 {
    match hit.severity {
        DecisionSeverity::Deny => 0,
        DecisionSeverity::Warn => 1,
        DecisionSeverity::Info => 2,
    }
}

pub(crate) fn binding_limit_hit(
    hits: &[DecisionLimitHitReport],
) -> Option<&DecisionLimitHitReport> {
    hits.iter().min_by(|left, right| {
        limit_hit_severity_rank(left)
            .cmp(&limit_hit_severity_rank(right))
            .then_with(
                || match (limit_hit_overflow(left), limit_hit_overflow(right)) {
                    (Some(left_overflow), Some(right_overflow)) => {
                        right_overflow.total_cmp(&left_overflow)
                    }
                    _ => std::cmp::Ordering::Equal,
                },
            )
            .then_with(|| limit_hit_identifier(left).cmp(limit_hit_identifier(right)))
            .then_with(|| left.rule_id.cmp(&right.rule_id))
    })
}

fn limit_scope_key(matched_entity: Option<&str>) -> String {
    matched_entity.unwrap_or("").to_owned()
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
    matched_entity: Option<&str>,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> f64 {
    if let Some(conn) = &ledger.conn {
        let sql = if matched_entity.is_some() {
            "
            SELECT COALESCE(SUM(r.amount_usd), 0)
            FROM reservations r
            JOIN decisions d ON d.decision_id = r.decision_id
            WHERE d.selected_budget_id = ?1
              AND d.matched_entity = ?2
              AND d.created_at >= ?3
              AND d.created_at <= ?4
            "
        } else {
            "
            SELECT COALESCE(SUM(r.amount_usd), 0)
            FROM reservations r
            JOIN decisions d ON d.decision_id = r.decision_id
            WHERE d.selected_budget_id = ?1
              AND d.created_at >= ?2
              AND d.created_at <= ?3
            "
        };
        let value = if let Some(matched_entity) = matched_entity {
            conn.query_row(
                sql,
                params![
                    rule_id,
                    matched_entity,
                    since.to_rfc3339(),
                    now.to_rfc3339()
                ],
                |row| row.get::<_, f64>(0),
            )
        } else {
            conn.query_row(
                sql,
                params![rule_id, since.to_rfc3339(), now.to_rfc3339()],
                |row| row.get::<_, f64>(0),
            )
        };
        return value.unwrap_or(0.0);
    }

    ledger
        .reservations
        .values()
        .filter(|stored| {
            stored
                .budget_rule_ids
                .iter()
                .any(|stored_rule_id| stored_rule_id == rule_id)
                && stored.reservation.created_at >= since
                && stored.reservation.created_at <= now
                && matched_entity
                    .is_none_or(|entity| stored.matched_entity.as_deref() == Some(entity))
        })
        .map(|stored| stored.reservation.amount_usd)
        .sum()
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
    if !rule.eligible.entities.is_empty() {
        return rule
            .eligible
            .entities
            .iter()
            .filter(|entity| request_entity_matches(request, entity))
            .cloned()
            .collect();
    }

    let mut entities = Vec::new();
    if let Some(project) = rule.rule_match.project.as_deref() {
        entities.push(format!("project:{project}"));
    }
    if let Some(subject) = rule.rule_match.subject.as_deref() {
        entities.push(if subject.contains(':') {
            subject.to_owned()
        } else {
            format!("user:{subject}")
        });
    }
    if entities.is_empty() && rule.rule_match == RuleMatch::default() {
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

    use crate::contract::{
        BudgetEligibility, BudgetLimitPolicy, BudgetModelPolicy, BudgetRule, ContextTokenLimit,
        RequestCostLimit, RuleMatch, SpendWindowLimit, SpendWindowMode, WindowAnchorKind,
        WindowAnchorPolicy,
    };

    use super::*;

    fn policy(limit_usd: f64, warn_at_fraction: f64) -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "dev-budget".to_owned(),
                priority: 0,
                eligible: Default::default(),
                models: Default::default(),
                limits: BudgetLimitPolicy {
                    request_cost: None,
                    context_tokens: None,
                    spend: vec![SpendWindowLimit {
                        id: Some("budget-cap".to_owned()),
                        window: "60s".to_owned(),
                        mode: Some(SpendWindowMode::Tumbling),
                        anchor: Some(WindowAnchorPolicy {
                            kind: WindowAnchorKind::FirstSeen,
                        }),
                        max_usd: limit_usd,
                        warn_at_fraction,
                        action: PolicyAction::Block,
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

    fn protected_adoption_policy(cap_window: &str) -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "ai-adoption".to_owned(),
                priority: 0,
                eligible: BudgetEligibility {
                    entities: vec!["org:example".to_owned()],
                },
                models: BudgetModelPolicy::default(),
                limits: BudgetLimitPolicy {
                    request_cost: None,
                    context_tokens: None,
                    spend: vec![SpendWindowLimit {
                        id: Some("budget-cap".to_owned()),
                        window: cap_window.to_owned(),
                        mode: Some(SpendWindowMode::Tumbling),
                        anchor: Some(WindowAnchorPolicy {
                            kind: WindowAnchorKind::FirstSeen,
                        }),
                        max_usd: 2000.0,
                        warn_at_fraction: 1.0,
                        action: PolicyAction::Block,
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
                rule_match: RuleMatch::default(),
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
    fn budget_limit_denies_large_context_estimate_when_enforced() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 1_000,
                action: PolicyAction::Block,
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
                id: None,
                window: "5h".to_owned(),
                mode: Some(SpendWindowMode::Rolling),
                anchor: None,
                max_usd: 10.0,
                warn_at_fraction: 1.0,
                action: PolicyAction::Warn,
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
            explanation.rule_id == "dev-budget.spend_window.5h"
                && explanation.reason
                    == "projected spend $11.000000 exceeds 5h limit max $10.000000"
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
                id: None,
                window: "7d".to_owned(),
                mode: Some(SpendWindowMode::Rolling),
                anchor: None,
                max_usd: 10.0,
                warn_at_fraction: 1.0,
                action: PolicyAction::Block,
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
    fn tumbling_spend_window_limit_warns_on_projected_bucket_spend() {
        let mut policy = policy(20.0, 1.0);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: None,
            spend: vec![SpendWindowLimit {
                id: Some("daily-tumbling".to_owned()),
                window: "1d".to_owned(),
                mode: Some(SpendWindowMode::Tumbling),
                anchor: Some(WindowAnchorPolicy {
                    kind: WindowAnchorKind::FirstSeen,
                }),
                max_usd: 10.0,
                warn_at_fraction: 1.0,
                action: PolicyAction::Warn,
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
                id: Some("daily-tumbling".to_owned()),
                window: "1d".to_owned(),
                mode: Some(SpendWindowMode::Tumbling),
                anchor: Some(WindowAnchorPolicy {
                    kind: WindowAnchorKind::FirstSeen,
                }),
                max_usd: 10.0,
                warn_at_fraction: 1.0,
                action: PolicyAction::Block,
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
                    id: Some("daily-tumbling".to_owned()),
                    window: "1d".to_owned(),
                    mode: Some(SpendWindowMode::Tumbling),
                    anchor: Some(WindowAnchorPolicy {
                        kind: WindowAnchorKind::FirstSeen,
                    }),
                    max_usd: 10.0,
                    warn_at_fraction: 1.0,
                    action: PolicyAction::Warn,
                },
                SpendWindowLimit {
                    id: Some("daily-rolling".to_owned()),
                    window: "1d".to_owned(),
                    mode: Some(SpendWindowMode::Rolling),
                    anchor: None,
                    max_usd: 10.0,
                    warn_at_fraction: 1.0,
                    action: PolicyAction::Warn,
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
                id: Some("daily-tumbling".to_owned()),
                window: "1d".to_owned(),
                mode: Some(SpendWindowMode::Tumbling),
                anchor: Some(WindowAnchorPolicy {
                    kind: WindowAnchorKind::FirstSeen,
                }),
                max_usd: 10.0,
                warn_at_fraction: 1.0,
                action: PolicyAction::Block,
            }],
            tool_calls: None,
            agent_steps: None,
            retries: None,
        };
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");

        let first = ledger.authorize(Some(&policy), &request(6.0));
        assert_eq!(first.outcome, DecisionOutcome::Allow);
        let persisted = ledger
            .conn
            .as_ref()
            .expect("sqlite conn")
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
        assert!(!has_budget_cap(&ledger, "project-budget", "project:noether"));
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
        assert_eq!(budget_cap_used(&ledger, "project-budget", "project:noether"), 0.25);
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
        assert_eq!(budget_cap_used(&ledger, "project-budget", "project:noether"), 0.25);
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
        assert_eq!(budget_cap_used(&ledger, "a-high-wide", "project:noether"), 0.25);
        assert!(!has_budget_cap(&ledger, "b-high-wide", "project:noether"));
        assert!(!has_budget_cap(&ledger, "z-high-tight", "project:noether"));
        assert!(!has_budget_cap(&ledger, "z-low-priority", "project:noether"));
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

        let conn = ledger.conn.as_ref().expect("sqlite conn");
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
    fn report_items_include_limit_hits() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("limit-hits-report.sqlite");
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].limits = BudgetLimitPolicy {
            request_cost: None,
            context_tokens: Some(ContextTokenLimit {
                max_tokens: 1_000,
                action: PolicyAction::Block,
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
        let mut policy = policy(5.0, 1.0);
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
                id: Some("daily-tumbling".to_owned()),
                window: "1d".to_owned(),
                mode: Some(SpendWindowMode::Tumbling),
                anchor: Some(WindowAnchorPolicy {
                    kind: WindowAnchorKind::FirstSeen,
                }),
                max_usd: 10.0,
                warn_at_fraction: 1.0,
                action: PolicyAction::Block,
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

        let conn = ledger.conn.as_ref().expect("sqlite conn");
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
        let conn = reopened.conn.as_ref().expect("reopened sqlite conn");
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
    fn protected_adoption_spend_consumes_carryover_before_current_grant() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("carryover-first.sqlite");
        let policy = protected_adoption_policy("60s");
        let mut ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
        let mut initial_request = request(0.25);
        initial_request.entities = vec!["org:example".to_owned(), "user:alice".to_owned()];
        ledger.authorize(Some(&policy), &initial_request);
        ledger
            .conn
            .as_ref()
            .expect("sqlite conn")
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
        ledger
            .conn
            .as_ref()
            .expect("sqlite conn")
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
        ledger
            .conn
            .as_ref()
            .expect("sqlite conn")
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
            eligible: BudgetEligibility {
                entities: entities.iter().map(|entity| (*entity).to_owned()).collect(),
            },
            models: BudgetModelPolicy::default(),
            limits: BudgetLimitPolicy {
                request_cost: None,
                context_tokens: None,
                spend: vec![SpendWindowLimit {
                    id: Some("budget-cap".to_owned()),
                    window: "60s".to_owned(),
                    mode: Some(SpendWindowMode::Tumbling),
                    anchor: Some(WindowAnchorPolicy {
                        kind: WindowAnchorKind::FirstSeen,
                    }),
                    max_usd: limit_usd,
                    warn_at_fraction: 1.0,
                    action: PolicyAction::Block,
                }],
                tool_calls: None,
                agent_steps: None,
                retries: None,
            },
            allocation: None,
            rule_match: RuleMatch::default(),
        }
    }
}
