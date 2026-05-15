use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use uuid::Uuid;

use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, BudgetRule, DecisionExplanation, DecisionOutcome,
    DecisionSeverity, EvalAnnotation, FinalizeReservation, PolicyEffect, Reservation,
    ReservationStatus, ToolEvent, TraceEvent, UsageObservation,
};
use crate::error::NoetError;
use crate::policy::{PolicyFile, matching_policy_explanations, rule_match_matches};

#[derive(Debug, Default)]
pub struct BudgetLedger {
    windows: HashMap<String, WindowState>,
    reservations: HashMap<String, StoredReservation>,
    events: Vec<TraceEvent>,
    conn: Option<Connection>,
}

#[derive(Debug)]
struct WindowState {
    started_at: DateTime<Utc>,
    used_usd: f64,
}

#[derive(Debug)]
struct StoredReservation {
    reservation: Reservation,
    estimated_cost_usd: f64,
    budget_rule_ids: Vec<String>,
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
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub reservations: u64,
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

        if let Some(policy) = policy {
            for (effect, explanation) in matching_policy_explanations(policy, request) {
                outcome = merge_policy_outcome(outcome, effect);
                explanations.push(explanation);
            }

            if outcome != DecisionOutcome::Deny {
                self.evaluate_budget_rules(policy, request, now, &mut outcome, &mut explanations);
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
            Some(self.create_reservation(policy, request, now))
        };

        let decision = AuthorizeDecision {
            decision_id: Uuid::new_v4().to_string(),
            outcome,
            reservation,
            explanations,
            created_at: now,
        };
        self.persist_decision(request, &decision)?;
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
                   COALESCE(SUM(u.total_tokens), 0), COALESCE(SUM(r.amount_usd), 0), COUNT(r.id)
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
                    total_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    total_cost_usd: row.get(5)?,
                    reservations: row.get::<_, i64>(6)?.max(0) as u64,
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
            SELECT created_at, outcome, decision_id
            FROM decisions
            ORDER BY created_at DESC
            ",
        )?;
        stmt.query_map([], |row| {
            let outcome: String = row.get(1)?;
            let decision_id: String = row.get(2)?;
            Ok(TraceReportItem {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                kind: format!("decision.{outcome}"),
                summary: decision_id,
            })
        })?
        .collect::<Result<_, _>>()
        .map_err(NoetError::from)
    }

    pub fn observations_report(
        &self,
        kind_prefix: Option<&str>,
    ) -> Result<Vec<TraceReportItem>, NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(Vec::new());
        };
        let mut stmt = if kind_prefix.is_some() {
            conn.prepare(
                "
                SELECT occurred_at, kind, payload_json
                FROM events
                WHERE kind LIKE ?1
                ORDER BY occurred_at DESC
                ",
            )?
        } else {
            conn.prepare(
                "
                SELECT occurred_at, kind, payload_json
                FROM events
                ORDER BY occurred_at DESC
                ",
            )?
        };
        let prefix = kind_prefix.map(|prefix| format!("{prefix}%"));
        let mapper = |row: &rusqlite::Row<'_>| {
            Ok(TraceReportItem {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                kind: row.get(1)?,
                summary: row.get(2)?,
            })
        };
        match prefix {
            Some(prefix) => stmt
                .query_map([prefix], mapper)?
                .collect::<Result<_, _>>()
                .map_err(NoetError::from),
            None => stmt
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
            SELECT created_at, outcome, decision_id
            FROM decisions
            WHERE trace_id = ?1
            ORDER BY created_at
            ",
        )?;
        for row in decisions.query_map([trace_id], |row| {
            let outcome: String = row.get(1)?;
            let decision_id: String = row.get(2)?;
            Ok(TraceReportItem {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                kind: format!("decision.{outcome}"),
                summary: decision_id,
            })
        })? {
            items.push(row?);
        }

        let mut usage = conn.prepare(
            "
            SELECT created_at, provider, model, total_tokens, cost_usd
            FROM usage_observations
            WHERE trace_id = ?1
            ORDER BY created_at
            ",
        )?;
        for row in usage.query_map([trace_id], |row| {
            let provider: Option<String> = row.get(1)?;
            let model: Option<String> = row.get(2)?;
            let tokens: Option<i64> = row.get(3)?;
            let cost: Option<f64> = row.get(4)?;
            Ok(TraceReportItem {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                kind: "usage.finalized".to_owned(),
                summary: format!(
                    "{} {} tokens={} cost={:.6}",
                    provider.unwrap_or_else(|| "unknown".to_owned()),
                    model.unwrap_or_else(|| "unknown".to_owned()),
                    tokens.unwrap_or_default(),
                    cost.unwrap_or_default()
                ),
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
            Ok(TraceReportItem {
                occurred_at: parse_time(row.get::<_, String>(0)?),
                kind: row.get(1)?,
                summary: row.get(2)?,
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
    ) {
        let estimated_cost = request.estimated_cost();
        let matching_rules: Vec<&BudgetRule> = policy
            .budgets
            .iter()
            .filter(|rule| rule_match_matches(&rule.rule_match, request))
            .collect();

        if matching_rules.is_empty() {
            explanations.push(DecisionExplanation {
                rule_id: "no_budget_match".to_owned(),
                reason: "no matching budget rule; request allowed".to_owned(),
                severity: DecisionSeverity::Info,
            });
            return;
        }

        for rule in matching_rules {
            let window = self.window(rule, now);
            let projected = window.used_usd + estimated_cost;
            if projected > rule.limit_usd {
                *outcome = DecisionOutcome::Deny;
                explanations.push(DecisionExplanation {
                    rule_id: rule.id.clone(),
                    reason: format!(
                        "estimated cost ${projected:.6} exceeds fixed-window limit ${:.6}",
                        rule.limit_usd
                    ),
                    severity: DecisionSeverity::Deny,
                });
            } else if projected >= rule.limit_usd * rule.warn_at_fraction
                && *outcome != DecisionOutcome::Deny
            {
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
        }
    }

    fn create_reservation(
        &mut self,
        policy: Option<&PolicyFile>,
        request: &AuthorizeRequest,
        now: DateTime<Utc>,
    ) -> Reservation {
        let amount_usd = request.estimated_cost();
        let matching_rules: Vec<&BudgetRule> = policy
            .map(|policy| {
                policy
                    .budgets
                    .iter()
                    .filter(|rule| rule_match_matches(&rule.rule_match, request))
                    .collect()
            })
            .unwrap_or_default();
        let budget_rule_ids: Vec<String> =
            matching_rules.iter().map(|rule| rule.id.clone()).collect();
        let expires_at = matching_rules
            .iter()
            .map(|rule| now + Duration::seconds(rule.window_seconds))
            .min()
            .unwrap_or_else(|| now + Duration::hours(1));

        for rule in matching_rules {
            self.window(rule, now).used_usd += amount_usd;
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

    fn persist_decision(
        &self,
        request: &AuthorizeRequest,
        decision: &AuthorizeDecision,
    ) -> Result<(), NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(());
        };
        let trace_id = string_metadata(request, "trace_id");
        let session_id = string_metadata(request, "session_id");
        let request_id = string_metadata(request, "request_id");
        conn.execute(
            "
            INSERT INTO decisions (
                decision_id, trace_id, session_id, request_id, subject, project, provider, model,
                estimated_tokens, estimated_cost_usd, outcome, explanations_json, metadata_json,
                created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
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
                    created_at, expires_at, budget_rule_ids_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
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
                ],
            )?;
        }
        Ok(())
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

    fn load_active_reservations(&mut self) -> Result<(), NoetError> {
        let Some(conn) = &self.conn else {
            return Ok(());
        };
        let mut stmt = conn.prepare(
            "
            SELECT id, amount_usd, estimated_amount_usd, currency, status, created_at, expires_at,
                   budget_rule_ids_json
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
            budget_rule_ids_json TEXT NOT NULL DEFAULT '[]'
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
        ",
    )?;
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

#[cfg(test)]
mod tests {
    use crate::contract::{BudgetRule, RuleMatch};

    use super::*;

    fn policy(limit_usd: f64, warn_at_fraction: f64) -> PolicyFile {
        PolicyFile {
            version: 0,
            budgets: vec![BudgetRule {
                id: "dev-budget".to_owned(),
                limit_usd,
                warn_at_fraction,
                window_seconds: 60,
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
}
