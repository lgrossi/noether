use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, BudgetRule, DecisionExplanation, DecisionOutcome,
    DecisionSeverity, EvalAnnotation, FinalizeReservation, PolicyEffect, Reservation,
    ReservationStatus, RuleMatch, ToolEvent, TraceEvent, UsageObservation,
};
use crate::error::NoetError;
use crate::policy::{
    PolicyFile, budget_model_allowed, budget_rule_matches, budget_scope_matches,
    matching_policy_explanations, specificity_order,
};

#[derive(Debug, Default)]
pub struct BudgetLedger {
    windows: HashMap<String, WindowState>,
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
    current_grant_usd: f64,
    carryover_usd: f64,
}

#[derive(Debug)]
struct StoredReservation {
    reservation: Reservation,
    estimated_cost_usd: f64,
    budget_rule_ids: Vec<String>,
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

#[derive(Default)]
struct RoutingPersistenceFields {
    selected_budget_id: Option<String>,
    matched_entity: Option<String>,
    selection_reason: Option<String>,
    rejected_budget_id: Option<String>,
    rejected_budget_reason: Option<String>,
    model_check: Option<String>,
    remaining_budget_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct UsageReport {
    pub total_cost_usd: f64,
    pub rows: Vec<UsageReportRow>,
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

#[derive(Debug, Serialize)]
pub struct TraceReport {
    pub trace_id: String,
    pub items: Vec<TraceReportItem>,
}

#[derive(Debug, Serialize)]
pub struct TraceReportItem {
    pub occurred_at: DateTime<Utc>,
    pub kind: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing: Option<DecisionRoutingReport>,
}

#[derive(Clone, Debug, Serialize)]
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
    pub remaining_budget_usd: Option<f64>,
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
        ledger.load_windows()?;
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
        let mut outcome = DecisionOutcome::Allow;
        let mut explanations = Vec::new();
        let mut selected_budget_id = None;

        if let Some(policy) = policy {
            for (effect, explanation) in matching_policy_explanations(policy, request) {
                outcome = merge_policy_outcome(outcome, effect);
                explanations.push(explanation);
            }

            if outcome != DecisionOutcome::Deny {
                selected_budget_id = self.evaluate_budget_rules(
                    policy,
                    request,
                    now,
                    &mut outcome,
                    &mut explanations,
                );
            }
        } else {
            explanations.push(DecisionExplanation {
                rule_id: "no_policy".to_owned(),
                reason: "no policy file configured; request allowed".to_owned(),
                severity: DecisionSeverity::Info,
            });
        }

        let reservation = if outcome == DecisionOutcome::Deny {
            None
        } else {
            Some(self.create_reservation(policy, request, now, selected_budget_id.as_deref()))
        };
        if reservation.is_some() {
            self.persist_allocation_buckets()?;
        }

        let decision = AuthorizeDecision {
            decision_id: Uuid::new_v4().to_string(),
            outcome,
            reservation,
            explanations,
            created_at: now,
        };
        self.persist_decision(policy, request, &decision)?;
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
            for rule_id in &stored.budget_rule_ids {
                if let Some(window) = self.windows.get_mut(rule_id) {
                    window.used_usd = (window.used_usd + delta).max(0.0);
                }
            }
            stored.reservation.amount_usd = actual_cost;
        }

        stored.reservation.status = ReservationStatus::Finalized;
        let reservation = stored.reservation.clone();
        self.persist_finalization(&reservation, payload)?;
        self.persist_windows()?;
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
        })
    }

    pub fn decisions_report(&self) -> Result<Vec<TraceReportItem>, NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "
            SELECT created_at, outcome, decision_id, trace_id, request_id, provider, model,
                   estimated_tokens, estimated_cost_usd, metadata_json, selected_budget_id,
                   matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                   model_check, remaining_budget_usd
            FROM decisions
            ORDER BY created_at DESC
            ",
        )?;
        stmt.query_map([], decision_report_item_from_row)?
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
        let mut sql = "SELECT occurred_at, kind, payload_json FROM events".to_owned();
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
                routing: None,
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
                   estimated_tokens, estimated_cost_usd, metadata_json, selected_budget_id,
                   matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                   model_check, remaining_budget_usd
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
                routing: None,
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
                routing: None,
            })
        })? {
            items.push(row?);
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
        outcome: &mut DecisionOutcome,
        explanations: &mut Vec<DecisionExplanation>,
    ) -> Option<String> {
        let estimated_cost = request.estimated_cost();
        let candidate = self.select_budget_rule(policy, request, now, explanations);

        let Some(candidate) = candidate else {
            let exhausted_rules = self.exhausted_budget_rules(policy, request, now);
            if !exhausted_rules.is_empty() {
                *outcome = DecisionOutcome::Deny;
                for (rule_id, projected, limit_usd) in exhausted_rules {
                    explanations.push(DecisionExplanation {
                        rule_id,
                        reason: format!(
                            "estimated cost ${projected:.6} exceeds fixed-window limit ${limit_usd:.6}"
                        ),
                        severity: DecisionSeverity::Deny,
                    });
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
                *outcome = DecisionOutcome::Deny;
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
                *outcome = DecisionOutcome::Deny;
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
        if apply_budget_guards(rule, request, estimated_cost, outcome, explanations) {
            return Some(rule.id.clone());
        }
        let projected = self.window_used_usd(rule, now) + estimated_cost;
        if projected >= rule.limit_usd * rule.warn_at_fraction {
            *outcome = DecisionOutcome::Warn;
            explanations.push(DecisionExplanation {
                rule_id: rule.id.clone(),
                reason: format!(
                    "estimated cost ${projected:.6} reaches warning threshold ${:.6}",
                    rule.limit_usd * rule.warn_at_fraction
                ),
                severity: DecisionSeverity::Warn,
            });
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
                    reason: self.budget_rejection_reason(rule, request, now),
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
        if let Some(available_usd) = allocation_bucket_available_usd(self, rule, request, now)
            && estimated_cost > available_usd
        {
            return None;
        }
        let projected = self.window_used_usd(rule, now) + estimated_cost;
        if projected > rule.limit_usd {
            return None;
        }
        let (matched_entity, specificity_rank) =
            matched_entity_and_rank(rule, request, &specificity_order(policy));
        Some(BudgetCandidate {
            id: rule.id.clone(),
            matched_entity,
            specificity_rank,
            priority: rule.priority,
            pressure_micros: ((projected / rule.limit_usd) * 1_000_000.0).round() as u64,
        })
    }

    fn budget_rejection_reason(
        &mut self,
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
        if let Some(available_usd) = allocation_bucket_available_usd(self, rule, request, now)
            && request.estimated_cost() > available_usd
        {
            return format!(
                "estimated cost ${:.6} exceeds protected adoption balance ${available_usd:.6}",
                request.estimated_cost()
            );
        }
        let projected = self.window_used_usd(rule, now) + request.estimated_cost();
        if projected > rule.limit_usd {
            return format!(
                "requested budget would exceed fixed-window limit: projected ${projected:.6} > ${:.6}",
                rule.limit_usd
            );
        }
        "requested budget is not valid for the request".to_owned()
    }

    fn exhausted_budget_rules(
        &mut self,
        policy: &PolicyFile,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> Vec<(String, f64, f64)> {
        policy
            .budgets
            .iter()
            .filter(|rule| budget_rule_matches(rule, request))
            .filter_map(|rule| {
                let projected = self.window_used_usd(rule, now) + request.estimated_cost();
                (projected > rule.limit_usd).then(|| (rule.id.clone(), projected, rule.limit_usd))
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
        let mut allocation_spends = Vec::new();
        let expires_at = matching_rules
            .iter()
            .map(|rule| now + Duration::seconds(rule.window_seconds))
            .min()
            .unwrap_or_else(|| now + Duration::hours(1));

        for rule in matching_rules {
            self.window(rule, now).used_usd += amount_usd;
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
                allocation_spends,
            },
        );
        reservation
    }

    fn window(&mut self, rule: &BudgetRule, now: DateTime<Utc>) -> &mut WindowState {
        let window_seconds = Duration::seconds(rule.window_seconds);
        let window = self.windows.entry(rule.id.clone()).or_insert(WindowState {
            started_at: now,
            used_usd: 0.0,
        });

        if now - window.started_at >= window_seconds {
            window.started_at = now;
            window.used_usd = 0.0;
        }

        window
    }

    fn window_used_usd(&self, rule: &BudgetRule, now: DateTime<Utc>) -> f64 {
        let Some(window) = self.windows.get(&rule.id) else {
            return 0.0;
        };
        if now - window.started_at >= Duration::seconds(rule.window_seconds) {
            0.0
        } else {
            window.used_usd
        }
    }

    fn persist_decision(
        &self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        decision: &AuthorizeDecision,
    ) -> Result<(), NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(());
        };
        let trace_id = string_metadata(request, "trace_id");
        let session_id = string_metadata(request, "session_id");
        let request_id = string_metadata(request, "request_id");
        let routing = self.routing_persistence_fields(policy, request, decision);
        conn.execute(
            "
            INSERT INTO decisions (
                decision_id, trace_id, session_id, request_id, subject, project, provider, model,
                estimated_tokens, estimated_cost_usd, outcome, explanations_json, metadata_json,
                selected_budget_id, matched_entity, selection_reason, rejected_budget_id,
                rejected_budget_reason, model_check, remaining_budget_usd, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                ?19, ?20, ?21
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
                serde_json::to_string(&decision.explanations)?,
                serde_json::to_string(&request.metadata)?,
                routing.selected_budget_id.as_deref(),
                routing.matched_entity.as_deref(),
                routing.selection_reason.as_deref(),
                routing.rejected_budget_id.as_deref(),
                routing.rejected_budget_reason.as_deref(),
                routing.model_check.as_deref(),
                routing.remaining_budget_usd,
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
                    created_at, expires_at, budget_rule_ids_json, allocation_spends_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
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
    ) -> RoutingPersistenceFields {
        let selected_budget_id = decision
            .reservation
            .as_ref()
            .and_then(|reservation| self.reservations.get(&reservation.id))
            .and_then(|stored| stored.budget_rule_ids.first())
            .cloned();

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
                fields.remaining_budget_usd = Some(
                    (rule.limit_usd - self.window_used_usd(rule, decision.created_at)).max(0.0),
                );
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
        let Some(conn) = &self.conn else {
            return Ok(());
        };
        for (rule_id, window) in &self.windows {
            conn.execute(
                "
                INSERT INTO budget_windows (rule_id, started_at, used_usd)
                VALUES (?1, ?2, ?3)
                ON CONFLICT(rule_id) DO UPDATE SET
                    started_at = excluded.started_at,
                    used_usd = excluded.used_usd
                ",
                params![rule_id, window.started_at.to_rfc3339(), window.used_usd],
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
                    rule_id, entity_key, started_at, current_grant_usd, carryover_usd
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(rule_id, entity_key) DO UPDATE SET
                    started_at = excluded.started_at,
                    current_grant_usd = excluded.current_grant_usd,
                    carryover_usd = excluded.carryover_usd
                ",
                params![
                    rule_id,
                    entity_key,
                    bucket.started_at.to_rfc3339(),
                    bucket.current_grant_usd,
                    bucket.carryover_usd
                ],
            )?;
        }
        Ok(())
    }

    fn load_windows(&mut self) -> Result<(), NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(());
        };
        let mut stmt = conn.prepare("SELECT rule_id, started_at, used_usd FROM budget_windows")?;
        let windows: Vec<(String, WindowState)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    WindowState {
                        started_at: parse_time(row.get::<_, String>(1)?),
                        used_usd: row.get(2)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        self.windows = windows.into_iter().collect();
        Ok(())
    }

    fn load_allocation_buckets(&mut self) -> Result<(), NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(());
        };
        let mut stmt = conn.prepare(
            "
            SELECT rule_id, entity_key, started_at, current_grant_usd, carryover_usd
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
                        current_grant_usd: row.get(3)?,
                        carryover_usd: row.get(4)?,
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
                   budget_rule_ids_json, allocation_spends_json
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
                let allocation_spends_json: String = row.get(8)?;
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
                        allocation_spends,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        self.reservations = reservations.into_iter().collect();
        Ok(())
    }
}

fn merge_policy_outcome(current: DecisionOutcome, effect: PolicyEffect) -> DecisionOutcome {
    if current == DecisionOutcome::Deny || effect == PolicyEffect::Deny {
        DecisionOutcome::Deny
    } else if current == DecisionOutcome::Warn || effect == PolicyEffect::Warn {
        DecisionOutcome::Warn
    } else {
        DecisionOutcome::Allow
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
            explanations_json TEXT NOT NULL,
            metadata_json TEXT NOT NULL,
            selected_budget_id TEXT,
            matched_entity TEXT,
            selection_reason TEXT,
            rejected_budget_id TEXT,
            rejected_budget_reason TEXT,
            model_check TEXT,
            remaining_budget_usd REAL,
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

        CREATE TABLE IF NOT EXISTS budget_allocation_buckets (
            rule_id TEXT NOT NULL,
            entity_key TEXT NOT NULL,
            started_at TEXT,
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
    ensure_column(
        conn,
        "decisions",
        "remaining_budget_usd",
        "remaining_budget_usd REAL",
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

fn reservation_status_text(status: ReservationStatus) -> &'static str {
    match status {
        ReservationStatus::Active => "active",
        ReservationStatus::Finalized => "finalized",
    }
}

fn parse_time(value: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn decision_report_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceReportItem> {
    let outcome: String = row.get(1)?;
    let decision_id: String = row.get(2)?;
    let trace_id: Option<String> = row.get(3)?;
    let request_id: Option<String> = row.get(4)?;
    let provider: Option<String> = row.get(5)?;
    let model: Option<String> = row.get(6)?;
    let estimated_tokens: Option<i64> = row.get(7)?;
    let estimated_cost_usd: Option<f64> = row.get(8)?;
    let metadata_json: String = row.get(9)?;
    let selected_budget_id: Option<String> = row.get(10)?;
    let matched_entity: Option<String> = row.get(11)?;
    let selection_reason: Option<String> = row.get(12)?;
    let rejected_budget_id: Option<String> = row.get(13)?;
    let rejected_budget_reason: Option<String> = row.get(14)?;
    let model_check: Option<String> = row.get(15)?;
    let remaining_budget_usd: Option<f64> = row.get(16)?;
    let summary = DecisionSummary {
        decision_id: &decision_id,
        trace_id: trace_id.as_deref(),
        request_id: request_id.as_deref(),
        provider: provider.as_deref(),
        model: model.as_deref(),
        estimated_tokens,
        estimated_cost_usd,
        metadata_json: &metadata_json,
        routing: DecisionRoutingSummary {
            selected_budget_id: selected_budget_id.as_deref(),
            matched_entity: matched_entity.as_deref(),
            selection_reason: selection_reason.as_deref(),
            rejected_budget_id: rejected_budget_id.as_deref(),
            rejected_budget_reason: rejected_budget_reason.as_deref(),
            model_check: model_check.as_deref(),
            remaining_budget_usd,
        },
    };
    Ok(TraceReportItem {
        occurred_at: parse_time(row.get::<_, String>(0)?),
        kind: format!("decision.{outcome}"),
        summary: summarize_decision(summary),
        routing: decision_routing_report(
            selected_budget_id,
            matched_entity,
            selection_reason,
            rejected_budget_id,
            rejected_budget_reason,
            model_check,
            remaining_budget_usd,
        ),
    })
}

fn decision_routing_report(
    selected_budget_id: Option<String>,
    matched_entity: Option<String>,
    selection_reason: Option<String>,
    rejected_budget_id: Option<String>,
    rejected_budget_reason: Option<String>,
    model_check: Option<String>,
    remaining_budget_usd: Option<f64>,
) -> Option<DecisionRoutingReport> {
    let has_fields = selected_budget_id.is_some()
        || matched_entity.is_some()
        || selection_reason.is_some()
        || rejected_budget_id.is_some()
        || rejected_budget_reason.is_some()
        || model_check.is_some()
        || remaining_budget_usd.is_some();
    has_fields.then_some(DecisionRoutingReport {
        selected_budget_id,
        matched_entity,
        selection_reason,
        rejected_budget_id,
        rejected_budget_reason,
        model_check,
        remaining_budget_usd,
    })
}

fn apply_budget_guards(
    rule: &BudgetRule,
    request: &AuthorizeRequest,
    estimated_cost: f64,
    outcome: &mut DecisionOutcome,
    explanations: &mut Vec<DecisionExplanation>,
) -> bool {
    if let Some(guard) = &rule.guards.max_estimated_request_cost_usd
        && estimated_cost > guard.max_usd
        && push_guard_explanation(
            format!("{}.max_estimated_request_cost_usd", rule.id),
            format!(
                "estimated request cost ${estimated_cost:.6} exceeds guard max ${:.6}",
                guard.max_usd
            ),
            format!(
                "estimated request cost ${estimated_cost:.6} exceeds enforced guard max ${:.6}",
                guard.max_usd
            ),
            guard.effect,
            outcome,
            explanations,
        )
    {
        return true;
    }

    if let Some(guard) = &rule.guards.max_context_tokens
        && let Some(estimated_tokens) = request.estimated_tokens
        && estimated_tokens > guard.max_tokens
        && push_guard_explanation(
            format!("{}.max_context_tokens", rule.id),
            format!(
                "estimated context tokens {estimated_tokens} exceed guard max {}",
                guard.max_tokens
            ),
            format!(
                "estimated context tokens {estimated_tokens} exceed enforced guard max {}",
                guard.max_tokens
            ),
            guard.effect,
            outcome,
            explanations,
        )
    {
        return true;
    }

    false
}

fn push_guard_explanation(
    rule_id: String,
    warn_reason: String,
    deny_reason: String,
    effect: PolicyEffect,
    outcome: &mut DecisionOutcome,
    explanations: &mut Vec<DecisionExplanation>,
) -> bool {
    let (severity, reason, denied) = match effect {
        PolicyEffect::Warn => (DecisionSeverity::Warn, warn_reason, false),
        PolicyEffect::Deny => (DecisionSeverity::Deny, deny_reason, true),
        PolicyEffect::Allow => unreachable!("guard validation forbids allow effects"),
    };
    if denied {
        *outcome = DecisionOutcome::Deny;
    } else if *outcome != DecisionOutcome::Deny {
        *outcome = DecisionOutcome::Warn;
    }
    explanations.push(DecisionExplanation {
        rule_id,
        reason,
        severity,
    });
    denied
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
    if now - bucket.started_at < Duration::seconds(rule.window_seconds) {
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
    decision_id: &'a str,
    trace_id: Option<&'a str>,
    request_id: Option<&'a str>,
    provider: Option<&'a str>,
    model: Option<&'a str>,
    estimated_tokens: Option<i64>,
    estimated_cost_usd: Option<f64>,
    metadata_json: &'a str,
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
    remaining_budget_usd: Option<f64>,
}

fn summarize_decision(decision: DecisionSummary<'_>) -> String {
    let metadata = serde_json::from_str::<Value>(decision.metadata_json).unwrap_or(Value::Null);
    let mut parts = vec![format!("decision_id={}", decision.decision_id)];
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
    if let Some(remaining_budget_usd) = decision.routing.remaining_budget_usd {
        parts.push(format!("remaining_budget={remaining_budget_usd:.6}"));
    }
    let shape = summarize_request_shape(&metadata);
    if !shape.is_empty() {
        parts.push(format!("shape={}", shape.join(",")));
    }
    parts.join(" ")
}

fn summarize_event_payload(kind: &str, payload_json: &str) -> String {
    let payload = serde_json::from_str::<Value>(payload_json).unwrap_or(Value::Null);
    match kind {
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
    if let Some(summary) = payload.get("payload_summary") {
        let shape = summarize_request_shape(&serde_json::json!({ "payload_summary": summary }));
        if !shape.is_empty() {
            parts.push(format!("shape={}", shape.join(",")));
        }
    }
    summarize_attribution(&mut parts, payload);
    summarize_parts_or_kind(parts, "provider call")
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
    if let Some(usage) = payload.get("usage") {
        let usage_summary = summarize_usage_payload(usage);
        if usage_summary != "usage" {
            parts.push(format!("usage=({usage_summary})"));
        }
    }
    summarize_parts_or_kind(parts, "turn")
}

fn summarize_agent_payload(payload: &Value) -> String {
    let mut parts = Vec::new();
    push_value_u64(&mut parts, "messages", payload.get("message_count"));
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

fn summarize_parts_or_kind(parts: Vec<String>, fallback: &str) -> String {
    if parts.is_empty() {
        fallback.to_owned()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use crate::contract::{
        BudgetEligibility, BudgetGuardPolicy, BudgetModelPolicy, BudgetRule, MaxContextTokensGuard,
        MaxEstimatedRequestCostGuard, RuleMatch,
    };

    use super::*;

    fn policy(limit_usd: f64, warn_at_fraction: f64) -> PolicyFile {
        PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "dev-budget".to_owned(),
                limit_usd,
                priority: 0,
                warn_at_fraction,
                window_seconds: 60,
                eligible: Default::default(),
                models: Default::default(),
                guards: Default::default(),
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
            explanation.rule_id == "dev-budget" && explanation.severity == DecisionSeverity::Warn
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
    fn budget_guard_warns_on_expensive_single_request() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].guards = BudgetGuardPolicy {
            max_estimated_request_cost_usd: Some(MaxEstimatedRequestCostGuard {
                max_usd: 0.20,
                effect: PolicyEffect::Warn,
            }),
            max_context_tokens: None,
        };
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.25));

        assert_eq!(decision.outcome, DecisionOutcome::Warn);
        assert!(decision.reservation.is_some());
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.max_estimated_request_cost_usd"
                && explanation.reason
                    == "estimated request cost $0.250000 exceeds guard max $0.200000"
                && explanation.severity == DecisionSeverity::Warn
        }));
    }

    #[test]
    fn budget_guard_denies_expensive_single_request_when_enforced() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].guards = BudgetGuardPolicy {
            max_estimated_request_cost_usd: Some(MaxEstimatedRequestCostGuard {
                max_usd: 0.20,
                effect: PolicyEffect::Deny,
            }),
            max_context_tokens: None,
        };
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.25));

        assert_eq!(decision.outcome, DecisionOutcome::Deny);
        assert!(decision.reservation.is_none());
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.max_estimated_request_cost_usd"
                && explanation.reason
                    == "estimated request cost $0.250000 exceeds enforced guard max $0.200000"
                && explanation.severity == DecisionSeverity::Deny
        }));
    }

    #[test]
    fn budget_guard_warns_on_large_context_estimate() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].guards = BudgetGuardPolicy {
            max_estimated_request_cost_usd: None,
            max_context_tokens: Some(MaxContextTokensGuard {
                max_tokens: 1_000,
                effect: PolicyEffect::Warn,
            }),
        };
        let mut request = request(0.25);
        request.estimated_tokens = Some(1_200);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Warn);
        assert!(decision.reservation.is_some());
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.max_context_tokens"
                && explanation.reason == "estimated context tokens 1200 exceed guard max 1000"
                && explanation.severity == DecisionSeverity::Warn
        }));
    }

    #[test]
    fn budget_guard_denies_large_context_estimate_when_enforced() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].guards = BudgetGuardPolicy {
            max_estimated_request_cost_usd: None,
            max_context_tokens: Some(MaxContextTokensGuard {
                max_tokens: 1_000,
                effect: PolicyEffect::Deny,
            }),
        };
        let mut request = request(0.25);
        request.estimated_tokens = Some(1_200);
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request);

        assert_eq!(decision.outcome, DecisionOutcome::Deny);
        assert!(decision.reservation.is_none());
        assert!(decision.explanations.iter().any(|explanation| {
            explanation.rule_id == "dev-budget.max_context_tokens"
                && explanation.reason
                    == "estimated context tokens 1200 exceed enforced guard max 1000"
                && explanation.severity == DecisionSeverity::Deny
        }));
    }

    #[test]
    fn budget_guard_allows_missing_context_estimate() {
        let mut policy = policy(1.0, 0.8);
        policy.budgets[0].guards = BudgetGuardPolicy {
            max_estimated_request_cost_usd: None,
            max_context_tokens: Some(MaxContextTokensGuard {
                max_tokens: 1_000,
                effect: PolicyEffect::Deny,
            }),
        };
        let mut ledger = BudgetLedger::default();

        let decision = ledger.authorize(Some(&policy), &request(0.25));

        assert_eq!(decision.outcome, DecisionOutcome::Allow);
        assert!(decision.reservation.is_some());
        assert!(
            !decision
                .explanations
                .iter()
                .any(|explanation| { explanation.rule_id == "dev-budget.max_context_tokens" })
        );
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
        assert_eq!(ledger.windows.get("team-budget").unwrap().used_usd, 0.25);
        assert!(!ledger.windows.contains_key("project-budget"));
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
        assert_eq!(ledger.windows.get("project-budget").unwrap().used_usd, 0.25);
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
        assert_eq!(ledger.windows.get("project-budget").unwrap().used_usd, 0.25);
        assert!(!ledger.windows.contains_key("team-budget"));
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
        assert_eq!(ledger.windows.get("a-high-wide").unwrap().used_usd, 0.25);
        assert!(!ledger.windows.contains_key("b-high-wide"));
        assert!(!ledger.windows.contains_key("z-high-tight"));
        assert!(!ledger.windows.contains_key("z-low-priority"));
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
                       rejected_budget_reason, model_check, remaining_budget_usd
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
        assert_eq!(routing.remaining_budget_usd, Some(0.75));

        let trace = ledger.trace_report("trace-report").expect("trace report");
        let trace_routing = trace.items[0].routing.as_ref().expect("trace routing");
        assert_eq!(
            trace_routing.selected_budget_id.as_deref(),
            Some("project-budget")
        );
    }

    #[test]
    fn sqlite_persists_protected_adoption_buckets_across_reopen() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("protected-adoption.sqlite");
        let policy = PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "ai-adoption".to_owned(),
                limit_usd: 2000.0,
                priority: 0,
                warn_at_fraction: 1.0,
                window_seconds: 60,
                eligible: BudgetEligibility {
                    entities: vec!["org:example".to_owned()],
                },
                models: BudgetModelPolicy::default(),
                guards: BudgetGuardPolicy::default(),
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
        };
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
        let policy = PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "ai-adoption".to_owned(),
                limit_usd: 2000.0,
                priority: 0,
                warn_at_fraction: 1.0,
                window_seconds: 60,
                eligible: BudgetEligibility {
                    entities: vec!["org:example".to_owned()],
                },
                models: BudgetModelPolicy::default(),
                guards: BudgetGuardPolicy::default(),
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
        };
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
        let policy = PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "ai-adoption".to_owned(),
                limit_usd: 2000.0,
                priority: 0,
                warn_at_fraction: 1.0,
                window_seconds: 60,
                eligible: BudgetEligibility {
                    entities: vec!["org:example".to_owned()],
                },
                models: BudgetModelPolicy::default(),
                guards: BudgetGuardPolicy::default(),
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
        };
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
        let policy = PolicyFile {
            version: 0,
            routing: Default::default(),
            budgets: vec![BudgetRule {
                id: "ai-adoption".to_owned(),
                limit_usd: 2000.0,
                priority: 0,
                warn_at_fraction: 1.0,
                window_seconds: 1,
                eligible: BudgetEligibility {
                    entities: vec!["org:example".to_owned()],
                },
                models: BudgetModelPolicy::default(),
                guards: BudgetGuardPolicy::default(),
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
        };
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
            limit_usd,
            priority,
            warn_at_fraction: 1.0,
            window_seconds: 60,
            eligible: BudgetEligibility {
                entities: entities.iter().map(|entity| (*entity).to_owned()).collect(),
            },
            models: BudgetModelPolicy::default(),
            guards: BudgetGuardPolicy::default(),
            allocation: None,
            rule_match: RuleMatch::default(),
        }
    }
}
