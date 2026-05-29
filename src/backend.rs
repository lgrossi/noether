use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use uuid::Uuid;

use crate::contract::{AuthorizeDecision, AuthorizeRequest, FinalizeReservation, Reservation, TraceEvent};
use crate::error::NoetError;
use crate::ledger::{
    ConnMutex, DecisionLimitHitReport, DecisionRoutingReport,
    DecisionRoutingSummary, DecisionSummary, FinalizedUsageSummary, HistoricalAuthorizeRequest,
    HotSnapshot, ProtectedAdoptionEntityReport, ProtectedAdoptionReport, RoutingPersistenceFields,
    RuleStatsReport, RunTotalsReport, SpendScopeTotal, TraceReport,
    TraceReportItem, UsageActivityRecord, UsageReport, UsageReportRow, WindowState,
    action_text, agent_run_id_from_metadata_json, binding_limit_hit, decision_app_run_key,
    decision_routing_report, limit_hits_from_explanations_json, outcome_text,
    parse_decision_outcome, parse_entities_json, parse_optional_json, parse_time,
    persist_allocation_buckets, persist_decision as sqlite_persist_decision,
    persist_event, persist_finalization, persist_limit_windows,
    primary_explanation_rule_id, reservation_status_text, rolling_bucket_start,
    string_metadata, summarize_decision, summarize_event_payload,
    summarize_finalized_usage,
};

// ---------------------------------------------------------------------------
// PG schema DDL
// ---------------------------------------------------------------------------
// Every statement is idempotent (IF NOT EXISTS / IF NOT EXISTS for ALTER).
// JSON blobs from SQLite become JSONB so Phase 5 can use ->> operators.
// TEXT is kept for UUIDs and RFC-3339 timestamps — app code expects strings.
// INTEGER → BIGINT, REAL → DOUBLE PRECISION (lossless for all stored values).
// ---------------------------------------------------------------------------
const PG_SCHEMA_STATEMENTS: &[&str] = &[
    // ---- schema_migrations -------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS schema_migrations (
        version    BIGINT  PRIMARY KEY,
        applied_at TEXT    NOT NULL
    )"#,
    // Seed version 1 unconditionally (idempotent via ON CONFLICT DO NOTHING).
    r#"INSERT INTO schema_migrations (version, applied_at)
       VALUES (1, NOW()::TEXT)
       ON CONFLICT DO NOTHING"#,

    // ---- decisions ---------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS decisions (
        decision_id               TEXT             PRIMARY KEY,
        trace_id                  TEXT,
        session_id                TEXT,
        request_id                TEXT,
        subject                   TEXT,
        project                   TEXT,
        provider                  TEXT,
        model                     TEXT,
        estimated_tokens          BIGINT,
        estimated_cost_usd        DOUBLE PRECISION,
        outcome                   TEXT             NOT NULL,
        action                    TEXT             NOT NULL DEFAULT 'allow',
        explanations_json         JSONB            NOT NULL,
        metadata_json             JSONB            NOT NULL,
        entities_json             JSONB            NOT NULL DEFAULT '[]',
        selected_budget_id        TEXT,
        matched_entity            TEXT,
        selection_reason          TEXT,
        rejected_budget_id        TEXT,
        rejected_budget_reason    TEXT,
        model_check               TEXT,
        budget_window_remaining_usd DOUBLE PRECISION,
        routing_json              JSONB,
        limit_hits_json           JSONB,
        app_run_key               TEXT,
        created_at                TEXT             NOT NULL,
        max_tool_calls            BIGINT,
        max_agent_steps           BIGINT,
        max_retries               BIGINT
    )"#,

    // ---- reservations ------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS reservations (
        id                        TEXT             PRIMARY KEY,
        decision_id               TEXT             NOT NULL REFERENCES decisions(decision_id),
        amount_usd                DOUBLE PRECISION NOT NULL,
        estimated_amount_usd      DOUBLE PRECISION NOT NULL,
        actual_amount_usd         DOUBLE PRECISION,
        currency                  TEXT             NOT NULL,
        status                    TEXT             NOT NULL,
        created_at                TEXT             NOT NULL,
        expires_at                TEXT             NOT NULL,
        finalized_at              TEXT,
        budget_rule_ids_json      JSONB            NOT NULL DEFAULT '[]',
        limit_window_spends_json  JSONB            NOT NULL DEFAULT '[]',
        allocation_spends_json    JSONB            NOT NULL DEFAULT '[]'
    )"#,

    // ---- reservation_limit_scopes ------------------------------------------
    // No explicit PK in SQLite; the composite (rule_id, limit_id, scope_key,
    // reservation_id) is the natural key but was never declared as a PK there.
    r#"CREATE TABLE IF NOT EXISTS reservation_limit_scopes (
        reservation_id  TEXT             NOT NULL REFERENCES reservations(id),
        rule_id         TEXT             NOT NULL,
        limit_id        TEXT             NOT NULL,
        scope_key       TEXT             NOT NULL,
        amount_usd      DOUBLE PRECISION NOT NULL DEFAULT 0,
        created_at      TEXT
    )"#,

    // ---- rolling_spend_buckets ---------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS rolling_spend_buckets (
        rule_id       TEXT             NOT NULL,
        limit_id      TEXT             NOT NULL,
        scope_key     TEXT             NOT NULL,
        bucket_start  TEXT             NOT NULL,
        amount_usd    DOUBLE PRECISION NOT NULL,
        PRIMARY KEY (rule_id, limit_id, scope_key, bucket_start)
    )"#,

    // ---- usage_observations ------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS usage_observations (
        id              TEXT             PRIMARY KEY,
        reservation_id  TEXT             REFERENCES reservations(id),
        trace_id        TEXT,
        provider        TEXT,
        model           TEXT,
        input_tokens    BIGINT,
        output_tokens   BIGINT,
        total_tokens    BIGINT,
        cost_usd        DOUBLE PRECISION,
        latency_ms      BIGINT,
        stop_reason     TEXT,
        source          TEXT,
        metadata_json   JSONB            NOT NULL,
        created_at      TEXT             NOT NULL
    )"#,

    // ---- events ------------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS events (
        id           TEXT  PRIMARY KEY,
        trace_id     TEXT,
        kind         TEXT  NOT NULL,
        occurred_at  TEXT  NOT NULL,
        source       TEXT,
        payload_json JSONB NOT NULL
    )"#,

    // ---- budget_windows ----------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS budget_windows (
        rule_id     TEXT             PRIMARY KEY,
        started_at  TEXT             NOT NULL,
        used_usd    DOUBLE PRECISION NOT NULL
    )"#,

    // ---- limit_window_states -----------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS limit_window_states (
        rule_id    TEXT             NOT NULL,
        limit_id   TEXT             NOT NULL,
        scope_key  TEXT             NOT NULL,
        started_at TEXT             NOT NULL,
        used_usd   DOUBLE PRECISION NOT NULL,
        PRIMARY KEY (rule_id, limit_id, scope_key)
    )"#,

    // ---- budget_allocation_buckets -----------------------------------------
    r#"CREATE TABLE IF NOT EXISTS budget_allocation_buckets (
        rule_id               TEXT             NOT NULL,
        entity_key            TEXT             NOT NULL,
        started_at            TEXT,
        protected_amount_usd  DOUBLE PRECISION NOT NULL DEFAULT 0,
        current_grant_usd     DOUBLE PRECISION NOT NULL,
        carryover_usd         DOUBLE PRECISION NOT NULL,
        PRIMARY KEY (rule_id, entity_key)
    )"#,

    // ---- indexes -----------------------------------------------------------
    "CREATE INDEX IF NOT EXISTS idx_decisions_trace               ON decisions (trace_id)",
    "CREATE INDEX IF NOT EXISTS idx_decisions_created             ON decisions (created_at)",
    "CREATE INDEX IF NOT EXISTS idx_decisions_created_decision    ON decisions (created_at, decision_id)",
    "CREATE INDEX IF NOT EXISTS idx_decisions_app_run_key_created ON decisions (app_run_key, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_reservations_decision         ON reservations (decision_id)",
    "CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_lookup     ON reservation_limit_scopes (rule_id, limit_id, scope_key)",
    "CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_reservation ON reservation_limit_scopes (reservation_id)",
    "CREATE INDEX IF NOT EXISTS idx_reservation_limit_scopes_rolling    ON reservation_limit_scopes (rule_id, limit_id, scope_key, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_usage_trace  ON usage_observations (trace_id)",
    "CREATE INDEX IF NOT EXISTS idx_events_trace ON events (trace_id)",
    "CREATE INDEX IF NOT EXISTS idx_events_kind  ON events (kind)",

    // ---- Startup backfills (PG equivalents) --------------------------------
    //
    // backfill_reservation_limit_scope_rollups: fills amount_usd / created_at
    // that were NULL/0 in old rows.  Runs unconditionally on every startup.
    r#"UPDATE reservation_limit_scopes rls
       SET
           amount_usd = COALESCE(r.amount_usd, 0),
           created_at = r.created_at
       FROM reservations r
       WHERE r.id = rls.reservation_id
         AND (rls.created_at IS NULL OR rls.amount_usd = 0)"#,

    // backfill_decision_app_run_keys: derives app_run_key from metadata_json
    // (JSONB ->> operator).  Runs unconditionally on every startup.
    r#"UPDATE decisions
       SET app_run_key = CASE
           WHEN metadata_json->>'agent_run_id' IS NOT NULL
               THEN 'agent-run:' || (metadata_json->>'agent_run_id')
           WHEN trace_id IS NOT NULL
               THEN 'trace-fallback:' || trace_id
           ELSE 'untraced:' || outcome || ':' ||
                COALESCE(selected_budget_id, 'none') || ':' ||
                EXTRACT(EPOCH FROM created_at::timestamptz)::bigint / 60
           END
       WHERE app_run_key IS NULL OR app_run_key = ''"#,

    // backfill_rolling_spend_buckets: gated on version < 2.
    // Deletes stale per-minute buckets and rebuilds per-second buckets.
    // Uses INSERT … ON CONFLICT DO UPDATE to avoid duplicate-key errors on
    // re-runs within the same second (shouldn't happen, but keeps it safe).
    r#"DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM schema_migrations WHERE version = 2) THEN
        DELETE FROM rolling_spend_buckets;

        INSERT INTO rolling_spend_buckets (rule_id, limit_id, scope_key, bucket_start, amount_usd)
        SELECT
            rule_id,
            limit_id,
            scope_key,
            to_char(created_at::timestamptz AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS+00:00') AS bucket_start,
            SUM(amount_usd) AS amount_usd
        FROM reservation_limit_scopes
        WHERE created_at IS NOT NULL
        GROUP BY rule_id, limit_id, scope_key, bucket_start
        ON CONFLICT (rule_id, limit_id, scope_key, bucket_start)
        DO UPDATE SET amount_usd = rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd;

        INSERT INTO schema_migrations (version, applied_at)
        VALUES (2, NOW()::TEXT)
        ON CONFLICT DO NOTHING;
    END IF;
END;
$$"#,
];

/// Convert a filesystem path to a sqlite:// URL.
/// Resolves the path to absolute using the current working directory.
pub fn path_to_sqlite_url(p: &Path) -> String {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    };
    format!("sqlite://{}", abs.display())
}

/// Extract the filesystem path from a sqlite:// URL.
/// Returns None if the URL is empty or does not start with "sqlite://".
pub fn sqlite_url_to_path(url: &str) -> Option<PathBuf> {
    let tail = url.strip_prefix("sqlite://")?;
    if tail.is_empty() {
        return None;
    }
    Some(PathBuf::from(tail))
}

pub struct SqliteBackend {
    pub conn: Arc<ConnMutex>,
    pub db_url: String,
}

pub struct PostgresBackend {
    pub pool: Arc<Pool>,
    pub db_url: String,
    pub events_count: Arc<AtomicU64>,
}

pub enum Backend {
    Sqlite(SqliteBackend),
    Postgres(PostgresBackend),
}

impl Backend {
    pub fn sqlite_from_url(db_url: String, conn: Arc<ConnMutex>) -> Self {
        Backend::Sqlite(SqliteBackend { conn, db_url })
    }

    pub fn postgres_from_url(db_url: String) -> Result<Self, NoetError> {
        let pg_config: tokio_postgres::Config = db_url.parse().map_err(|e: tokio_postgres::Error| {
            NoetError::InvalidConfig(format!("invalid postgres URL: {e}"))
        })?;
        let mgr_config = deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        };
        let mgr = deadpool_postgres::Manager::from_config(
            pg_config,
            tokio_postgres::NoTls,
            mgr_config,
        );
        let pool = deadpool_postgres::Pool::builder(mgr)
            .max_size(16)
            .build()
            .map_err(|e| NoetError::InvalidConfig(format!("failed to build postgres pool: {e}")))?;
        Ok(Backend::Postgres(PostgresBackend {
            pool: Arc::new(pool),
            db_url,
            events_count: Arc::new(AtomicU64::new(0)),
        }))
    }

    pub fn sqlite_conn(&self) -> &Arc<ConnMutex> {
        match self {
            Backend::Sqlite(b) => &b.conn,
            Backend::Postgres(_) => panic!("Postgres backend not yet implemented"),
        }
    }

    pub fn postgres_pool(&self) -> &Arc<Pool> {
        match self {
            Backend::Sqlite(_) => panic!("postgres_pool called on Sqlite backend"),
            Backend::Postgres(b) => &b.pool,
        }
    }

    pub fn db_url(&self) -> &str {
        match self {
            Backend::Sqlite(b) => &b.db_url,
            Backend::Postgres(b) => &b.db_url,
        }
    }

    // -----------------------------------------------------------------------
    // Write hotpath — Phase 6
    // -----------------------------------------------------------------------

    pub(crate) async fn persist_authorize_writes(
        &self,
        snap: HotSnapshot,
        request: AuthorizeRequest,
        decision: AuthorizeDecision,
        routing: RoutingPersistenceFields,
    ) -> Result<(), NoetError> {
        match self {
            Backend::Sqlite(b) => b.persist_authorize_writes(snap, request, decision, routing).await,
            Backend::Postgres(b) => b.persist_authorize_writes(snap, request, decision, routing).await,
        }
    }

    pub(crate) async fn persist_finalize_writes(
        &self,
        reservation: Reservation,
        payload: FinalizeReservation,
        lw_snapshot: Vec<((String, String, String), WindowState)>,
    ) -> Result<(), NoetError> {
        match self {
            Backend::Sqlite(b) => b.persist_finalize_writes(reservation, payload, lw_snapshot).await,
            Backend::Postgres(b) => b.persist_finalize_writes(reservation, payload, lw_snapshot).await,
        }
    }

    pub(crate) async fn persist_event_write(&self, event: TraceEvent) -> Result<(), NoetError> {
        match self {
            Backend::Sqlite(b) => b.persist_event_write(event).await,
            Backend::Postgres(b) => b.persist_event_write(event).await,
        }
    }
}

// ---------------------------------------------------------------------------
// SqliteBackend — write hotpath implementations (thin async wrappers)
// ---------------------------------------------------------------------------

impl SqliteBackend {
    async fn persist_authorize_writes(
        &self,
        snap: HotSnapshot,
        request: AuthorizeRequest,
        decision: AuthorizeDecision,
        routing: RoutingPersistenceFields,
    ) -> Result<(), NoetError> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let conn_guard = conn.lock().expect("conn mutex poisoned");
            if let Some(c) = conn_guard.as_ref() {
                persist_limit_windows(c, &snap.limit_windows)?;
                persist_allocation_buckets(c, &snap.allocation_buckets)?;
                let mut reservations = HashMap::new();
                reservations.insert(snap.reservation_id.clone(), snap.stored);
                sqlite_persist_decision(c, &request, &decision, &snap.limit_hits, routing, &reservations)?;
            }
            Ok(())
        })
        .await
        .expect("sqlite authorize persist panicked")
    }

    async fn persist_finalize_writes(
        &self,
        reservation: Reservation,
        payload: FinalizeReservation,
        lw_snapshot: Vec<((String, String, String), WindowState)>,
    ) -> Result<(), NoetError> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let conn_guard = conn.lock().expect("conn mutex poisoned");
            if let Some(c) = conn_guard.as_ref() {
                persist_finalization(c, &reservation, &payload)?;
                if !lw_snapshot.is_empty() {
                    let lw_map: HashMap<_, _> = lw_snapshot.into_iter().collect();
                    persist_limit_windows(c, &lw_map)?;
                }
            }
            Ok(())
        })
        .await
        .expect("sqlite finalize persist panicked")
    }

    async fn persist_event_write(&self, event: TraceEvent) -> Result<(), NoetError> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let conn_guard = conn.lock().expect("conn mutex poisoned");
            if let Some(c) = conn_guard.as_ref() {
                persist_event(c, &event)?;
            }
            Ok(())
        })
        .await
        .expect("sqlite event persist panicked")
    }
}

// ---------------------------------------------------------------------------
// PostgresBackend — write hotpath implementations (native async, single tx)
// ---------------------------------------------------------------------------

impl PostgresBackend {
    async fn persist_authorize_writes(
        &self,
        snap: HotSnapshot,
        request: AuthorizeRequest,
        decision: AuthorizeDecision,
        routing: RoutingPersistenceFields,
    ) -> Result<(), NoetError> {
        let client = self.pool.get().await?;
        client.batch_execute("BEGIN").await?;

        // Build decision fields.
        let trace_id = string_metadata(&request, "trace_id");
        let session_id = string_metadata(&request, "session_id");
        let request_id = string_metadata(&request, "request_id");
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
        let explanations_val: serde_json::Value = serde_json::to_value(&decision.explanations)?;
        let metadata_val: serde_json::Value = serde_json::to_value(&request.metadata)?;
        let entities_val: serde_json::Value = serde_json::to_value(&request.entities)?;
        let routing_val: serde_json::Value = serde_json::to_value(&routing_report)?;
        let limit_hits_val: serde_json::Value = serde_json::to_value(&snap.limit_hits)?;
        let estimated_tokens: Option<i64> = request.estimated_tokens.map(|v| v as i64);
        let tool_calls: Option<i64> = routing.tool_calls.map(|v| v as i64);
        let agent_steps: Option<i64> = routing.agent_steps.map(|v| v as i64);
        let retries: Option<i64> = routing.retries.map(|v| v as i64);

        let action = action_text(decision.action);
        let created_at_str = decision.created_at.to_rfc3339();
        client.execute(
            "INSERT INTO decisions (
                 decision_id, trace_id, session_id, request_id, subject, project, provider, model,
                 estimated_tokens, estimated_cost_usd, outcome, action, explanations_json,
                 metadata_json, entities_json, selected_budget_id, matched_entity, selection_reason,
                 rejected_budget_id, rejected_budget_reason, model_check,
                 budget_window_remaining_usd, routing_json, limit_hits_json,
                 max_tool_calls, max_agent_steps, max_retries, app_run_key, created_at
             ) VALUES (
                 $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13, $14, $15, $16, $17, $18, $19, $20, $21, $22,
                 $23, $24, $25, $26, $27, $28, $29
             )",
            &[
                &decision.decision_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &trace_id,
                &session_id,
                &request_id,
                &request.subject,
                &request.project,
                &request.provider,
                &request.model,
                &estimated_tokens,
                &request.estimated_cost_usd,
                &outcome,
                &action,
                &explanations_val,
                &metadata_val,
                &entities_val,
                &routing.selected_budget_id,
                &routing.matched_entity,
                &routing.selection_reason,
                &routing.rejected_budget_id,
                &routing.rejected_budget_reason,
                &routing.model_check,
                &routing.budget_window_remaining_usd,
                &routing_val,
                &limit_hits_val,
                &tool_calls,
                &agent_steps,
                &retries,
                &app_run_key,
                &created_at_str,
            ],
        ).await?;

        // Insert reservation and related rows if present.
        if let Some(reservation) = &decision.reservation {
            let stored = &snap.stored;
            let budget_rule_ids_json = serde_json::to_string(&stored.budget_rule_ids)?;
            let limit_window_spends_json = serde_json::to_string(&stored.limit_window_spends)?;
            let allocation_spends_json = serde_json::to_string(&stored.allocation_spends)?;

            let res_status = reservation_status_text(reservation.status);
            let res_created_at = reservation.created_at.to_rfc3339();
            let res_expires_at = reservation.expires_at.to_rfc3339();
            let budget_rule_ids_val: serde_json::Value = serde_json::to_value(&stored.budget_rule_ids)?;
            let limit_window_spends_val: serde_json::Value = serde_json::to_value(&stored.limit_window_spends)?;
            let allocation_spends_val: serde_json::Value = serde_json::to_value(&stored.allocation_spends)?;
            client.execute(
                "INSERT INTO reservations (
                     id, decision_id, amount_usd, estimated_amount_usd, currency, status,
                     created_at, expires_at, budget_rule_ids_json, limit_window_spends_json,
                     allocation_spends_json
                 ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                &[
                    &reservation.id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &decision.decision_id,
                    &reservation.amount_usd,
                    &reservation.amount_usd,
                    &reservation.currency,
                    &res_status,
                    &res_created_at,
                    &res_expires_at,
                    &budget_rule_ids_val,
                    &limit_window_spends_val,
                    &allocation_spends_val,
                ],
            ).await?;

            for spend in &stored.limit_window_spends {
                client.execute(
                    "INSERT INTO reservation_limit_scopes
                         (reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                    &[
                        &reservation.id,
                        &spend.rule_id,
                        &spend.limit_id,
                        &spend.scope_key,
                        &reservation.amount_usd,
                        &res_created_at,
                    ],
                ).await?;

                let bucket_start = rolling_bucket_start(reservation.created_at).to_rfc3339();
                client.execute(
                    "INSERT INTO rolling_spend_buckets
                         (rule_id, limit_id, scope_key, bucket_start, amount_usd)
                     VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (rule_id, limit_id, scope_key, bucket_start)
                     DO UPDATE SET amount_usd = rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd",
                    &[
                        &spend.rule_id,
                        &spend.limit_id,
                        &spend.scope_key,
                        &bucket_start,
                        &reservation.amount_usd,
                    ],
                ).await?;
            }
        }

        // Upsert limit windows — done last to minimize the hold time on the shared row lock.
        // With ON CONFLICT DO UPDATE, PG takes a row-level lock; keeping these at the end
        // reduces serialization pressure when concurrent transactions target the same scope.
        for ((rule_id, limit_id, scope_key), w) in &snap.limit_windows {
            let lw_started_at = w.started_at.to_rfc3339();
            client.execute(
                "INSERT INTO limit_window_states (rule_id, limit_id, scope_key, started_at, used_usd)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (rule_id, limit_id, scope_key) DO UPDATE SET
                     started_at = EXCLUDED.started_at,
                     used_usd = EXCLUDED.used_usd",
                &[rule_id, limit_id, scope_key, &lw_started_at, &w.used_usd],
            ).await?;
        }

        // Upsert allocation buckets — similarly last.
        for ((rule_id, entity_key), b) in &snap.allocation_buckets {
            let bucket_started_at = b.started_at.to_rfc3339();
            client.execute(
                "INSERT INTO budget_allocation_buckets
                     (rule_id, entity_key, started_at, protected_amount_usd, current_grant_usd, carryover_usd)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (rule_id, entity_key) DO UPDATE SET
                     started_at = EXCLUDED.started_at,
                     protected_amount_usd = EXCLUDED.protected_amount_usd,
                     current_grant_usd = EXCLUDED.current_grant_usd,
                     carryover_usd = EXCLUDED.carryover_usd",
                &[
                    rule_id,
                    entity_key,
                    &bucket_started_at,
                    &b.protected_amount_usd,
                    &b.current_grant_usd,
                    &b.carryover_usd,
                ],
            ).await?;
        }

        client.batch_execute("COMMIT").await?;
        Ok(())
    }

    async fn persist_finalize_writes(
        &self,
        reservation: Reservation,
        payload: FinalizeReservation,
        lw_snapshot: Vec<((String, String, String), WindowState)>,
    ) -> Result<(), NoetError> {
        let client = self.pool.get().await?;
        client.batch_execute("BEGIN").await?;

        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let status_str = reservation_status_text(reservation.status);

        if let Some(usage) = &payload.usage {
            // Single CTE eliminates the mid-transaction SELECT for trace_id.
            let metadata_val: serde_json::Value = serde_json::to_value(&payload.metadata)?;
            let input_tokens: Option<i64> = usage.input_tokens.map(|v| v as i64);
            let output_tokens: Option<i64> = usage.output_tokens.map(|v| v as i64);
            let total_tokens: Option<i64> = usage.total_tokens.map(|v| v as i64);
            let cost_usd: Option<f64> = usage.cost_usd.or(Some(reservation.amount_usd));
            let latency_ms: Option<i64> = usage.latency_ms.map(|v| v as i64);
            let obs_id = Uuid::new_v4().to_string();

            client.execute(
                "WITH upd AS (
                     UPDATE reservations
                     SET amount_usd = $2, actual_amount_usd = $2, status = $3, finalized_at = $4
                     WHERE id = $1
                     RETURNING decision_id
                 )
                 INSERT INTO usage_observations (
                     id, reservation_id, trace_id, provider, model,
                     input_tokens, output_tokens, total_tokens, cost_usd, latency_ms,
                     stop_reason, source, metadata_json, created_at
                 )
                 SELECT $5, $1, d.trace_id, $6, $7, $8, $9, $10, $11, $12, $13,
                        'reservation.finalize', $14, $4
                 FROM upd
                 JOIN decisions d ON d.decision_id = upd.decision_id",
                &[
                    &reservation.id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &reservation.amount_usd,
                    &status_str,
                    &now_str,
                    &obs_id,
                    &usage.provider,
                    &usage.model,
                    &input_tokens,
                    &output_tokens,
                    &total_tokens,
                    &cost_usd,
                    &latency_ms,
                    &usage.stop_reason,
                    &metadata_val,
                ],
            ).await?;
        } else {
            client.execute(
                "UPDATE reservations
                 SET amount_usd = $2, actual_amount_usd = $2, status = $3, finalized_at = $4
                 WHERE id = $1",
                &[
                    &reservation.id,
                    &reservation.amount_usd,
                    &status_str,
                    &now_str,
                ],
            ).await?;
        }

        // Upsert changed limit windows.
        for ((rule_id, limit_id, scope_key), w) in &lw_snapshot {
            let lw_started_at = w.started_at.to_rfc3339();
            client.execute(
                "INSERT INTO limit_window_states (rule_id, limit_id, scope_key, started_at, used_usd)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (rule_id, limit_id, scope_key) DO UPDATE SET
                     started_at = EXCLUDED.started_at,
                     used_usd = EXCLUDED.used_usd",
                &[rule_id, limit_id, scope_key, &lw_started_at, &w.used_usd],
            ).await?;
        }

        client.batch_execute("COMMIT").await?;
        Ok(())
    }

    async fn persist_event_write(&self, event: TraceEvent) -> Result<(), NoetError> {
        let client = self.pool.get().await?;
        let occurred_at_str = event.occurred_at.unwrap_or_else(Utc::now).to_rfc3339();
        let source = event
            .payload
            .as_object()
            .and_then(|p| p.get("source"))
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let id = event.id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
        let payload_val = event.payload.clone();

        // Single auto-committed statement — no explicit transaction needed.
        client.execute(
            "INSERT INTO events (id, trace_id, kind, occurred_at, source, payload_json)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &event.trace_id,
                &event.kind,
                &occurred_at_str,
                &source,
                &payload_val,
            ],
        ).await?;
        Ok(())
    }
}

impl PostgresBackend {
    /// Create (or verify) the full schema in a single transaction.
    ///
    /// All statements are idempotent: CREATE … IF NOT EXISTS, ALTER … IF NOT
    /// EXISTS, INSERT … ON CONFLICT DO NOTHING.  Calling this twice on the
    /// same database is safe — the second call is a no-op.
    pub async fn init_schema(&self) -> Result<(), NoetError> {
        let client = self.pool.get().await?;
        // Run everything inside a single explicit transaction so a partial
        // failure leaves the DB clean.
        client.batch_execute("BEGIN").await?;
        for stmt in PG_SCHEMA_STATEMENTS {
            client.batch_execute(stmt).await.map_err(|e| {
                NoetError::Database(format!("PG schema init failed on statement: {e}\nSQL: {stmt}"))
            })?;
        }
        client.batch_execute("COMMIT").await?;
        Ok(())
    }
}

/// Return the URL scheme ("sqlite" or "postgres") without the trailing "://",
/// or None if the URL contains no "://" separator.
pub fn url_scheme(url: &str) -> Option<&str> {
    let end = url.find("://")?;
    Some(&url[..end])
}

// ---------------------------------------------------------------------------
// PostgresBackend — read-only reporting queries (Phase 5)
// ---------------------------------------------------------------------------

impl PostgresBackend {
    pub async fn usage_report(&self) -> Result<UsageReport, NoetError> {
        let client = self.pool.get().await?;
        let pg_rows = client
            .query(
                "SELECT d.subject, d.project, COALESCE(u.provider, d.provider), COALESCE(u.model, d.model),
                        COALESCE(SUM(u.input_tokens), 0),
                        COALESCE(SUM(u.output_tokens), 0),
                        COALESCE(SUM((u.metadata_json->'usage_details'->>'cache_read_tokens')::BIGINT), 0),
                        COALESCE(SUM((u.metadata_json->'usage_details'->>'cache_write_tokens')::BIGINT), 0),
                        COALESCE(SUM(u.total_tokens), 0),
                        COALESCE(SUM((u.metadata_json->'usage_details'->>'cache_read_cost_usd')::DOUBLE PRECISION), 0),
                        COALESCE(SUM((u.metadata_json->'usage_details'->>'cache_write_cost_usd')::DOUBLE PRECISION), 0),
                        COALESCE(SUM(r.amount_usd), 0),
                        COUNT(r.id),
                        COALESCE(SUM(CASE WHEN r.status = 'active' THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN r.status = 'finalized' THEN 1 ELSE 0 END), 0)
                 FROM reservations r
                 JOIN decisions d ON d.decision_id = r.decision_id
                 LEFT JOIN usage_observations u ON u.reservation_id = r.id
                 GROUP BY d.subject, d.project, COALESCE(u.provider, d.provider), COALESCE(u.model, d.model)
                 ORDER BY COALESCE(SUM(r.amount_usd), 0) DESC",
                &[],
            )
            .await?;
        let rows: Vec<UsageReportRow> = pg_rows
            .iter()
            .map(|row| {
                Ok(UsageReportRow {
                    subject: row.try_get(0)?,
                    project: row.try_get(1)?,
                    provider: row.try_get(2)?,
                    model: row.try_get(3)?,
                    input_tokens: row.try_get::<_, i64>(4)?.max(0) as u64,
                    output_tokens: row.try_get::<_, i64>(5)?.max(0) as u64,
                    cache_read_tokens: row.try_get::<_, i64>(6)?.max(0) as u64,
                    cache_write_tokens: row.try_get::<_, i64>(7)?.max(0) as u64,
                    total_tokens: row.try_get::<_, i64>(8)?.max(0) as u64,
                    cache_read_cost_usd: row.try_get(9)?,
                    cache_write_cost_usd: row.try_get(10)?,
                    total_cost_usd: row.try_get(11)?,
                    reservations: row.try_get::<_, i64>(12)?.max(0) as u64,
                    active_reservations: row.try_get::<_, i64>(13)?.max(0) as u64,
                    finalized_reservations: row.try_get::<_, i64>(14)?.max(0) as u64,
                })
            })
            .collect::<Result<_, tokio_postgres::Error>>()?;
        let protected_adoption = self.protected_adoption_report().await?;
        Ok(UsageReport {
            total_cost_usd: rows.iter().map(|row| row.total_cost_usd).sum(),
            rows,
            protected_adoption,
        })
    }

    async fn protected_adoption_report(&self) -> Result<Option<ProtectedAdoptionReport>, NoetError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT rule_id, entity_key, protected_amount_usd, current_grant_usd, carryover_usd
                 FROM budget_allocation_buckets
                 WHERE protected_amount_usd > 0
                 ORDER BY rule_id, entity_key",
                &[],
            )
            .await?;

        if rows.is_empty() {
            return Ok(None);
        }

        let entities: Vec<ProtectedAdoptionEntityReport> = rows
            .iter()
            .map(|row| {
                let protected_amount_usd: f64 = row.try_get(2)?;
                let current_grant_usd: f64 = row.try_get(3)?;
                let carryover_usd: f64 = row.try_get(4)?;
                Ok(ProtectedAdoptionEntityReport {
                    budget_id: row.try_get(0)?,
                    entity_key: row.try_get(1)?,
                    protected_amount_usd,
                    current_grant_usd,
                    carryover_usd,
                    used_current_grant_usd: (protected_amount_usd - current_grant_usd).max(0.0),
                })
            })
            .collect::<Result<_, tokio_postgres::Error>>()?;

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
            unused_protected_opportunity_usd: entities.iter().map(|e| e.current_grant_usd).sum(),
            carryover_liability_usd: entities.iter().map(|e| e.carryover_usd).sum(),
            low_adopters,
            high_adopters,
        }))
    }

    pub async fn decisions_report(&self) -> Result<Vec<TraceReportItem>, NoetError> {
        self.decisions_report_since(None).await
    }

    pub async fn decisions_report_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<TraceReportItem>, NoetError> {
        let client = self.pool.get().await?;
        let rows = if let Some(since) = since {
            let sql = "
                SELECT created_at, outcome, decision_id, trace_id, request_id, provider, model,
                       action,
                       estimated_tokens, estimated_cost_usd, explanations_json, metadata_json, entities_json,
                       selected_budget_id, matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                       model_check, budget_window_remaining_usd, routing_json, limit_hits_json,
                       metadata_json->>'agent_run_id'
                FROM decisions
                WHERE created_at >= $1
                ORDER BY created_at DESC
            ";
            client.query(sql, &[&since.to_rfc3339()]).await?
        } else {
            let sql = "
                SELECT created_at, outcome, decision_id, trace_id, request_id, provider, model,
                       action,
                       estimated_tokens, estimated_cost_usd, explanations_json, metadata_json, entities_json,
                       selected_budget_id, matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                       model_check, budget_window_remaining_usd, routing_json, limit_hits_json,
                       metadata_json->>'agent_run_id'
                FROM decisions
                ORDER BY created_at DESC
            ";
            client.query(sql, &[]).await?
        };
        rows.iter()
            .map(pg_decision_row_to_trace_item)
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(NoetError::from)
    }

    pub async fn decisions_report_for_run_page(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<TraceReportItem>, NoetError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
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
                       d.metadata_json->>'agent_run_id'
                FROM decisions d
                JOIN run_page p ON d.app_run_key = p.app_run_key
                ORDER BY p.latest_at DESC, d.created_at DESC
                ",
                &[&(limit as i64), &(offset as i64)],
            )
            .await?;
        rows.iter()
            .map(pg_decision_row_to_trace_item)
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(NoetError::from)
    }

    pub async fn run_totals_report(&self) -> Result<RunTotalsReport, NoetError> {
        self.run_totals_report_since(None).await
    }

    pub async fn run_totals_report_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<RunTotalsReport, NoetError> {
        let client = self.pool.get().await?;
        let since_str = since.map(|dt| dt.to_rfc3339());

        let runs: u64 = if let Some(ref s) = since_str {
            let row = client
                .query_one(
                    "SELECT COUNT(DISTINCT app_run_key) FROM decisions WHERE created_at >= $1",
                    &[s],
                )
                .await?;
            row.try_get::<_, i64>(0)?.max(0) as u64
        } else {
            let row = client
                .query_one("SELECT COUNT(DISTINCT app_run_key) FROM decisions", &[])
                .await?;
            row.try_get::<_, i64>(0)?.max(0) as u64
        };

        let outcome_rows = if let Some(ref s) = since_str {
            client
                .query(
                    "SELECT outcome, COUNT(*) FROM decisions WHERE created_at >= $1 GROUP BY outcome",
                    &[s],
                )
                .await?
        } else {
            client
                .query("SELECT outcome, COUNT(*) FROM decisions GROUP BY outcome", &[])
                .await?
        };
        let mut allow: u64 = 0;
        let mut warn: u64 = 0;
        let mut deny: u64 = 0;
        let mut ask: u64 = 0;
        for row in &outcome_rows {
            let outcome: String = row.try_get(0)?;
            let count: i64 = row.try_get::<_, i64>(1)?.max(0);
            match outcome.as_str() {
                "allow" => allow += count as u64,
                "warn" => warn += count as u64,
                "deny" => deny += count as u64,
                "ask" => ask += count as u64,
                _ => {}
            }
        }

        let limit_hits: u64 = if let Some(ref s) = since_str {
            let row = client
                .query_one(
                    "SELECT COALESCE(SUM(COALESCE(jsonb_array_length(limit_hits_json), 0)), 0) \
                     FROM decisions WHERE created_at >= $1",
                    &[s],
                )
                .await?;
            row.try_get::<_, i64>(0)?.max(0) as u64
        } else {
            let row = client
                .query_one(
                    "SELECT COALESCE(SUM(COALESCE(jsonb_array_length(limit_hits_json), 0)), 0) FROM decisions",
                    &[],
                )
                .await?;
            row.try_get::<_, i64>(0)?.max(0) as u64
        };

        let usage_row = if let Some(ref s) = since_str {
            client
                .query_one(
                    "SELECT COALESCE(SUM(total_tokens), 0), COALESCE(SUM(cost_usd), 0) \
                     FROM usage_observations WHERE created_at >= $1",
                    &[s],
                )
                .await?
        } else {
            client
                .query_one(
                    "SELECT COALESCE(SUM(total_tokens), 0), COALESCE(SUM(cost_usd), 0) FROM usage_observations",
                    &[],
                )
                .await?
        };
        let tokens: u64 = usage_row.try_get::<_, i64>(0)?.max(0) as u64;
        let spend_usd: f64 = usage_row.try_get(1)?;

        Ok(RunTotalsReport { runs, allow, warn, deny, ask, limit_hits, spend_usd, tokens })
    }

    pub async fn rule_stats_report(&self) -> Result<Vec<RuleStatsReport>, NoetError> {
        let client = self.pool.get().await?;
        let count_rows = client
            .query(
                "SELECT COALESCE(selected_budget_id, explanations_json->0->>'rule_id', 'unattributed'),
                        outcome,
                        COUNT(*),
                        COALESCE(SUM(COALESCE(jsonb_array_length(limit_hits_json), 0)), 0)
                 FROM decisions
                 GROUP BY 1, 2",
                &[],
            )
            .await?;

        let mut stats = std::collections::HashMap::<String, RuleStatsReport>::new();
        for row in &count_rows {
            let rule: String = row.try_get(0)?;
            let outcome: String = row.try_get(1)?;
            let count: i64 = row.try_get::<_, i64>(2)?.max(0);
            let limit_hits: i64 = row.try_get::<_, i64>(3)?.max(0);
            let count = count as u64;
            let limit_hits = limit_hits as u64;
            let stat = stats.entry(rule.clone()).or_insert_with(|| RuleStatsReport {
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

        let deny_rows = client
            .query(
                "SELECT COALESCE(selected_budget_id, explanations_json->0->>'rule_id', 'unattributed'),
                        explanations_json::TEXT,
                        provider,
                        model,
                        limit_hits_json::TEXT
                 FROM decisions
                 WHERE outcome = 'deny'
                    OR COALESCE(jsonb_array_length(limit_hits_json), 0) > 0",
                &[],
            )
            .await?;

        let mut reasons = std::collections::HashMap::<String, std::collections::HashMap<String, u64>>::new();
        let mut models = std::collections::HashMap::<String, std::collections::HashMap<String, u64>>::new();
        for row in &deny_rows {
            let rule: String = row.try_get(0).unwrap_or_else(|_| "unattributed".to_owned());
            let explanations_json: String = row.try_get(1).unwrap_or_default();
            let provider: Option<String> = row.try_get(2).ok().flatten();
            let model: Option<String> = row.try_get(3).ok().flatten();
            let limit_hits_json: Option<String> = row.try_get(4).ok().flatten();

            let reason = limit_hits_json
                .as_deref()
                .and_then(parse_optional_json::<Vec<DecisionLimitHitReport>>)
                .and_then(|hits| hits.into_iter().next())
                .map(|hit| hit.reason)
                .or_else(|| {
                    serde_json::from_str::<Vec<crate::contract::DecisionExplanation>>(&explanations_json)
                        .ok()?
                        .into_iter()
                        .find(|e| e.severity == crate::contract::DecisionSeverity::Deny)
                        .or_else(|| {
                            serde_json::from_str::<Vec<crate::contract::DecisionExplanation>>(&explanations_json)
                                .ok()?
                                .into_iter()
                                .next()
                        })
                        .map(|e| e.reason)
                });
            if let Some(reason) = reason {
                *reasons.entry(rule.clone()).or_default().entry(reason).or_default() += 1;
            }
            if let Some(model) = model {
                let model_ref = provider
                    .map(|p| format!("{p}/{model}"))
                    .unwrap_or(model);
                *models.entry(rule).or_default().entry(model_ref).or_default() += 1;
            }
        }

        let mut result = stats.into_values().collect::<Vec<_>>();
        for stat in &mut result {
            stat.top_reason = most_common_count_pg(reasons.get(&stat.rule));
            stat.top_model = most_common_count_pg(models.get(&stat.rule));
        }
        result.sort_by(|a, b| a.rule.cmp(&b.rule));
        Ok(result)
    }

    pub async fn historical_authorize_requests(
        &self,
    ) -> Result<Vec<HistoricalAuthorizeRequest>, NoetError> {
        self.historical_authorize_requests_since(None).await
    }

    pub async fn historical_authorize_requests_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<HistoricalAuthorizeRequest>, NoetError> {
        let client = self.pool.get().await?;
        let since_str: Option<String> = since.map(|dt| dt.to_rfc3339());
        let rows = client
            .query(
                "SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                        provider, model, estimated_tokens, estimated_cost_usd, metadata_json
                 FROM decisions
                 WHERE ($1::TEXT IS NULL OR created_at >= $1)
                 ORDER BY created_at ASC",
                &[&since_str],
            )
            .await?;
        rows.iter()
            .map(|row| {
                let entities_json: serde_json::Value = row.try_get(4)?;
                let metadata_json: serde_json::Value = row.try_get(11)?;
                Ok(HistoricalAuthorizeRequest {
                    occurred_at: parse_time(row.try_get::<_, String>(0)?),
                    decision_id: row.try_get(1)?,
                    baseline_outcome: parse_decision_outcome(
                        row.try_get::<_, String>(2)?.as_str(),
                    ),
                    request: AuthorizeRequest {
                        budget_id: metadata_json
                            .get("budget_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                        entities: serde_json::from_value::<Vec<String>>(entities_json)
                            .unwrap_or_default(),
                        subject: row.try_get(5)?,
                        project: row.try_get(6)?,
                        provider: row.try_get(7)?,
                        model: row.try_get(8)?,
                        estimated_tokens: row
                            .try_get::<_, Option<i64>>(9)?
                            .map(|v| v.max(0) as u64),
                        estimated_cost_usd: row.try_get(10)?,
                        metadata: serde_json::from_value(metadata_json).unwrap_or_default(),
                    },
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(NoetError::from)
    }

    pub async fn historical_authorize_request_count_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<usize, NoetError> {
        let client = self.pool.get().await?;
        let count: i64 = if let Some(since) = since {
            let rows = client
                .query(
                    "SELECT COUNT(*) FROM decisions WHERE created_at >= $1",
                    &[&since.to_rfc3339()],
                )
                .await?;
            rows[0].try_get::<_, i64>(0)?
        } else {
            let rows = client
                .query("SELECT COUNT(*) FROM decisions", &[])
                .await?;
            rows[0].try_get::<_, i64>(0)?
        };
        Ok(count.max(0) as usize)
    }

    pub async fn latest_historical_authorize_requests_since(
        &self,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<HistoricalAuthorizeRequest>, NoetError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let client = self.pool.get().await?;
        let rows = if let Some(since) = since {
            let sql = "
                SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                       provider, model, estimated_tokens, estimated_cost_usd, metadata_json
                FROM (
                    SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                           provider, model, estimated_tokens, estimated_cost_usd
                    FROM decisions
                    WHERE created_at >= $1
                    ORDER BY created_at DESC LIMIT $2
                ) subq ORDER BY created_at ASC
            ";
            client.query(sql, &[&since.to_rfc3339(), &(limit as i64)]).await?
        } else {
            let sql = "
                SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                       provider, model, estimated_tokens, estimated_cost_usd, metadata_json
                FROM (
                    SELECT created_at, decision_id, outcome, metadata_json, entities_json, subject, project,
                           provider, model, estimated_tokens, estimated_cost_usd
                    FROM decisions
                    ORDER BY created_at DESC LIMIT $1
                ) subq ORDER BY created_at ASC
            ";
            client.query(sql, &[&(limit as i64)]).await?
        };
        rows.iter()
            .map(|row| {
                let entities_json: serde_json::Value = row.try_get(4)?;
                let metadata_json: serde_json::Value = row.try_get(11)?;
                let metadata_str = metadata_json.to_string();
                Ok(HistoricalAuthorizeRequest {
                    occurred_at: parse_time(row.try_get::<_, String>(0)?),
                    decision_id: row.try_get(1)?,
                    baseline_outcome: parse_decision_outcome(
                        row.try_get::<_, String>(2)?.as_str(),
                    ),
                    request: AuthorizeRequest {
                        budget_id: metadata_json
                            .get("budget_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                        entities: parse_entities_json(entities_json.to_string()),
                        subject: row.try_get(5)?,
                        project: row.try_get(6)?,
                        provider: row.try_get(7)?,
                        model: row.try_get(8)?,
                        estimated_tokens: row
                            .try_get::<_, Option<i64>>(9)?
                            .map(|v| v.max(0) as u64),
                        estimated_cost_usd: row.try_get(10)?,
                        metadata: serde_json::from_str(&metadata_str).unwrap_or_default(),
                    },
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(NoetError::from)
    }

    pub async fn spend_scope_totals(
        &self,
        rule_id: &str,
        limit_id: &str,
        since: DateTime<Utc>,
        before: DateTime<Utc>,
    ) -> Result<Vec<SpendScopeTotal>, NoetError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT scope_key, COALESCE(SUM(amount_usd), 0)
                 FROM reservation_limit_scopes
                 WHERE rule_id = $1
                   AND limit_id = $2
                   AND created_at >= $3
                   AND created_at < $4
                 GROUP BY scope_key
                 HAVING COALESCE(SUM(amount_usd), 0) > 0",
                &[&rule_id, &limit_id, &since.to_rfc3339(), &before.to_rfc3339()],
            )
            .await?;
        rows.iter()
            .map(|r| {
                Ok(SpendScopeTotal {
                    scope_key: r.try_get::<_, String>(0)?,
                    amount_usd: r.try_get::<_, f64>(1)?,
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(NoetError::from)
    }

    pub async fn usage_activity_report(&self) -> Result<Vec<UsageActivityRecord>, NoetError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT u.created_at,
                        COALESCE(u.trace_id, d.trace_id),
                        d.subject,
                        d.project,
                        COALESCE(u.provider, d.provider),
                        COALESCE(u.model, d.model),
                        d.selected_budget_id,
                        d.matched_entity,
                        d.entities_json::text,
                        COALESCE(u.input_tokens, 0),
                        COALESCE(u.output_tokens, 0),
                        COALESCE((u.metadata_json->'usage_details'->>'cache_read_tokens')::bigint, 0),
                        COALESCE((u.metadata_json->'usage_details'->>'cache_write_tokens')::bigint, 0),
                        COALESCE(u.total_tokens, 0),
                        COALESCE(u.cost_usd, r.actual_amount_usd, r.amount_usd, 0),
                        COALESCE(u.metadata_json->>'agent_run_id', d.metadata_json->>'agent_run_id'),
                        COALESCE(u.metadata_json->>'request_id', d.request_id)
                 FROM usage_observations u
                 LEFT JOIN reservations r ON r.id = u.reservation_id
                 LEFT JOIN decisions d ON d.decision_id = r.decision_id
                 ORDER BY u.created_at DESC",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| {
                let entities_json: String = row.try_get(8)?;
                Ok(UsageActivityRecord {
                    occurred_at: parse_time(row.try_get::<_, String>(0)?),
                    trace_id: row.try_get(1)?,
                    subject: row.try_get(2)?,
                    project: row.try_get(3)?,
                    provider: row.try_get(4)?,
                    model: row.try_get(5)?,
                    selected_budget_id: row.try_get(6)?,
                    matched_entity: row.try_get(7)?,
                    entities: parse_entities_json(entities_json),
                    input_tokens: row.try_get::<_, i64>(9)?.max(0) as u64,
                    output_tokens: row.try_get::<_, i64>(10)?.max(0) as u64,
                    cache_read_tokens: row.try_get::<_, i64>(11)?.max(0) as u64,
                    cache_write_tokens: row.try_get::<_, i64>(12)?.max(0) as u64,
                    total_tokens: row.try_get::<_, i64>(13)?.max(0) as u64,
                    cost_usd: row.try_get(14)?,
                    agent_run_id: row.try_get(15)?,
                    request_id: row.try_get(16)?,
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(NoetError::from)
    }

    pub async fn usage_activity_report_for_agent_runs(
        &self,
        agent_run_ids: &[String],
    ) -> Result<Vec<UsageActivityRecord>, NoetError> {
        if agent_run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (1..=agent_run_ids.len())
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT u.created_at, COALESCE(u.trace_id, d.trace_id), d.subject, d.project,
                    COALESCE(u.provider, d.provider), COALESCE(u.model, d.model),
                    d.selected_budget_id, d.matched_entity, d.entities_json::text,
                    COALESCE(u.input_tokens, 0), COALESCE(u.output_tokens, 0),
                    COALESCE((u.metadata_json->'usage_details'->>'cache_read_tokens')::bigint, 0),
                    COALESCE((u.metadata_json->'usage_details'->>'cache_write_tokens')::bigint, 0),
                    COALESCE(u.total_tokens, 0),
                    COALESCE(u.cost_usd, r.actual_amount_usd, r.amount_usd, 0),
                    COALESCE(u.metadata_json->>'agent_run_id', d.metadata_json->>'agent_run_id'),
                    COALESCE(u.metadata_json->>'request_id', d.request_id)
             FROM usage_observations u
             LEFT JOIN reservations r ON r.id = u.reservation_id
             LEFT JOIN decisions d ON d.decision_id = r.decision_id
             WHERE COALESCE(u.metadata_json->>'agent_run_id', d.metadata_json->>'agent_run_id') IN ({placeholders})
             ORDER BY u.created_at DESC"
        );
        let client = self.pool.get().await?;
        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            agent_run_ids.iter().map(|id| id as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = client.query(sql.as_str(), params.as_slice()).await?;
        rows.iter()
            .map(|row| {
                let entities_json: String = row.try_get(8)?;
                Ok(UsageActivityRecord {
                    occurred_at: parse_time(row.try_get::<_, String>(0)?),
                    trace_id: row.try_get(1)?,
                    subject: row.try_get(2)?,
                    project: row.try_get(3)?,
                    provider: row.try_get(4)?,
                    model: row.try_get(5)?,
                    selected_budget_id: row.try_get(6)?,
                    matched_entity: row.try_get(7)?,
                    entities: parse_entities_json(entities_json),
                    input_tokens: row.try_get::<_, i64>(9)?.max(0) as u64,
                    output_tokens: row.try_get::<_, i64>(10)?.max(0) as u64,
                    cache_read_tokens: row.try_get::<_, i64>(11)?.max(0) as u64,
                    cache_write_tokens: row.try_get::<_, i64>(12)?.max(0) as u64,
                    total_tokens: row.try_get::<_, i64>(13)?.max(0) as u64,
                    cost_usd: row.try_get(14)?,
                    agent_run_id: row.try_get(15)?,
                    request_id: row.try_get(16)?,
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(NoetError::from)
    }

    pub async fn observations_report(
        &self,
        kind_prefix: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<Vec<TraceReportItem>, NoetError> {
        let client = self.pool.get().await?;
        let mut sql = "SELECT occurred_at, kind, payload_json::text, trace_id FROM events".to_owned();
        let mut clauses: Vec<String> = Vec::new();
        let mut param_idx: u8 = 1;
        let mut kind_param_idx: u8 = 0;
        let mut trace_param_idx: u8 = 0;
        if kind_prefix.is_some() {
            kind_param_idx = param_idx;
            clauses.push(format!("kind LIKE ${param_idx}"));
            param_idx += 1;
        }
        if trace_id.is_some() {
            trace_param_idx = param_idx;
            clauses.push(format!("trace_id = ${param_idx}"));
            param_idx += 1;
        }
        let _ = (param_idx, kind_param_idx, trace_param_idx);
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY occurred_at DESC");

        let prefix_owned = kind_prefix.map(|p| format!("{p}%"));

        let rows = match (prefix_owned.as_deref(), trace_id) {
            (Some(prefix), Some(tid)) => {
                client
                    .query(sql.as_str(), &[&prefix as &(dyn tokio_postgres::types::ToSql + Sync), &tid])
                    .await?
            }
            (Some(prefix), None) => {
                client
                    .query(sql.as_str(), &[&prefix as &(dyn tokio_postgres::types::ToSql + Sync)])
                    .await?
            }
            (None, Some(tid)) => {
                client
                    .query(sql.as_str(), &[&tid as &(dyn tokio_postgres::types::ToSql + Sync)])
                    .await?
            }
            (None, None) => client.query(sql.as_str(), &[]).await?,
        };

        rows.iter()
            .map(|row| {
                let occurred_at_str: String = row.try_get(0)?;
                let kind: String = row.try_get(1)?;
                let payload_json: String = row.try_get(2)?;
                let trace_id_col: Option<String> = row.try_get(3)?;
                Ok(TraceReportItem {
                    occurred_at: parse_time(occurred_at_str),
                    summary: summarize_event_payload(&kind, &payload_json),
                    kind,
                    trace_id: trace_id_col,
                    agent_run_id: None,
                    entities: Vec::new(),
                    routing: None,
                    limit_hits: None,
                    binding_limit: None,
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(NoetError::from)
    }

    pub async fn trace_report(&self, trace_id: &str) -> Result<TraceReport, NoetError> {
        let mut items = Vec::new();

        let decision_items = self.trace_report__decisions(trace_id).await?;
        items.extend(decision_items);

        let usage_items = self.trace_report__usage(trace_id).await?;
        items.extend(usage_items);

        let event_items = self.trace_report__events(trace_id).await?;
        items.extend(event_items);

        if let Some(limit_items) = self.trace_report__lifecycle_limits(trace_id).await? {
            items.extend(limit_items);
        }

        items.sort_by_key(|item| item.occurred_at);
        Ok(TraceReport {
            trace_id: trace_id.to_owned(),
            items,
        })
    }

    async fn trace_report__decisions(&self, trace_id: &str) -> Result<Vec<TraceReportItem>, NoetError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT created_at, outcome, decision_id, trace_id, request_id, provider, model,
                        action,
                        estimated_tokens, estimated_cost_usd,
                        explanations_json::text, metadata_json::text, entities_json::text,
                        selected_budget_id, matched_entity, selection_reason, rejected_budget_id, rejected_budget_reason,
                        model_check, budget_window_remaining_usd,
                        routing_json::text, limit_hits_json::text,
                        metadata_json->>'agent_run_id'
                 FROM decisions
                 WHERE trace_id = $1
                 ORDER BY created_at",
                &[&trace_id],
            )
            .await?;

        rows.iter()
            .map(|row| {
                let outcome: String = row.try_get(1)?;
                let decision_id: String = row.try_get(2)?;
                let trace_id_col: Option<String> = row.try_get(3)?;
                let request_id: Option<String> = row.try_get(4)?;
                let provider: Option<String> = row.try_get(5)?;
                let model: Option<String> = row.try_get(6)?;
                let action: String = row.try_get(7)?;
                let estimated_tokens: Option<i64> = row.try_get(8)?;
                let estimated_cost_usd: Option<f64> = row.try_get(9)?;
                let explanations_json: String = row.try_get(10)?;
                let metadata_json: String = row.try_get(11)?;
                let entities_json: String = row.try_get(12)?;
                let selected_budget_id: Option<String> = row.try_get(13)?;
                let matched_entity: Option<String> = row.try_get(14)?;
                let selection_reason: Option<String> = row.try_get(15)?;
                let rejected_budget_id: Option<String> = row.try_get(16)?;
                let rejected_budget_reason: Option<String> = row.try_get(17)?;
                let model_check: Option<String> = row.try_get(18)?;
                let budget_window_remaining_usd: Option<f64> = row.try_get(19)?;
                let routing_json: Option<String> = row.try_get(20)?;
                let limit_hits_json: Option<String> = row.try_get(21)?;
                let agent_run_id: Option<String> = row.try_get(22)?;

                let primary_rule_id = selected_budget_id
                    .clone()
                    .or_else(|| primary_explanation_rule_id(&explanations_json));
                let mut routing = routing_json
                    .as_deref()
                    .and_then(parse_optional_json::<DecisionRoutingReport>)
                    .or_else(|| {
                        decision_routing_report(
                            primary_rule_id.clone(),
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
                    trace_id: trace_id_col.as_deref(),
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
                            .and_then(|r| r.selected_budget_id.as_deref())
                            .or(primary_rule_id.as_deref()),
                        matched_entity: routing
                            .as_ref()
                            .and_then(|r| r.matched_entity.as_deref())
                            .or(matched_entity.as_deref()),
                        selection_reason: routing
                            .as_ref()
                            .and_then(|r| r.selection_reason.as_deref())
                            .or(selection_reason.as_deref()),
                        rejected_budget_id: routing
                            .as_ref()
                            .and_then(|r| r.rejected_budget_id.as_deref())
                            .or(rejected_budget_id.as_deref()),
                        rejected_budget_reason: routing
                            .as_ref()
                            .and_then(|r| r.rejected_budget_reason.as_deref())
                            .or(rejected_budget_reason.as_deref()),
                        model_check: routing
                            .as_ref()
                            .and_then(|r| r.model_check.as_deref())
                            .or(model_check.as_deref()),
                        budget_window_remaining_usd: routing
                            .as_ref()
                            .and_then(|r| r.budget_window_remaining_usd)
                            .or(budget_window_remaining_usd),
                        budget_window_mode: routing
                            .as_ref()
                            .and_then(|r| r.budget_window_mode.as_deref()),
                        budget_window_started_at: routing
                            .as_ref()
                            .and_then(|r| r.budget_window_started_at),
                        budget_window_ends_at: routing
                            .as_ref()
                            .and_then(|r| r.budget_window_ends_at),
                    },
                };
                Ok(TraceReportItem {
                    occurred_at: parse_time(row.try_get::<_, String>(0)?),
                    kind: format!("decision.{outcome}"),
                    summary: summarize_decision(summary),
                    trace_id: trace_id_col,
                    agent_run_id,
                    entities: parse_entities_json(entities_json),
                    binding_limit: limit_hits.as_deref().and_then(binding_limit_hit).cloned(),
                    routing,
                    limit_hits,
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(NoetError::from)
    }

    async fn trace_report__usage(&self, trace_id: &str) -> Result<Vec<TraceReportItem>, NoetError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT created_at, provider, model, input_tokens, output_tokens, total_tokens, cost_usd,
                        stop_reason, metadata_json::TEXT
                 FROM usage_observations
                 WHERE trace_id = $1
                 ORDER BY created_at",
                &[&trace_id],
            )
            .await?;

        let items = rows
            .iter()
            .map(|row| {
                let provider: Option<String> = row.try_get(1).ok().flatten();
                let model: Option<String> = row.try_get(2).ok().flatten();
                let input_tokens: Option<i64> = row.try_get(3).ok().flatten();
                let output_tokens: Option<i64> = row.try_get(4).ok().flatten();
                let tokens: Option<i64> = row.try_get(5).ok().flatten();
                let cost: Option<f64> = row.try_get(6).ok().flatten();
                let stop_reason: Option<String> = row.try_get(7).ok().flatten();
                let metadata_json: String = row.try_get(8).unwrap_or_else(|_| "{}".to_owned());
                TraceReportItem {
                    occurred_at: parse_time(row.try_get::<_, String>(0).unwrap_or_default()),
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
                }
            })
            .collect();

        Ok(items)
    }

    async fn trace_report__events(&self, trace_id: &str) -> Result<Vec<TraceReportItem>, NoetError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT occurred_at, kind, payload_json
                 FROM events
                 WHERE trace_id = $1
                 ORDER BY occurred_at",
                &[&trace_id],
            )
            .await?;
        let items = rows
            .iter()
            .map(|r| {
                let occurred_at_str: String = r.try_get(0)?;
                let kind: String = r.try_get(1)?;
                let payload_value: serde_json::Value = r.try_get(2)?;
                let payload_json = payload_value.to_string();
                Ok(TraceReportItem {
                    occurred_at: parse_time(occurred_at_str),
                    summary: summarize_event_payload(&kind, &payload_json),
                    kind,
                    trace_id: Some(trace_id.to_owned()),
                    agent_run_id: agent_run_id_from_metadata_json(&payload_json),
                    entities: Vec::new(),
                    routing: None,
                    limit_hits: None,
                    binding_limit: None,
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
        Ok(items)
    }

    async fn trace_report__lifecycle_limits(
        &self,
        trace_id: &str,
    ) -> Result<Option<Vec<TraceReportItem>>, NoetError> {
        let client = self.pool.get().await?;

        let rows = client
            .query(
                "SELECT created_at, max_tool_calls, max_agent_steps, max_retries
                 FROM decisions
                 WHERE trace_id = $1
                 ORDER BY created_at DESC
                 LIMIT 1",
                &[&trace_id],
            )
            .await?;

        let Some(row) = rows.first() else {
            return Ok(None);
        };

        let occurred_at = parse_time(row.try_get::<_, String>(0)?);
        let max_tool_calls: Option<u64> = row
            .try_get::<_, Option<i64>>(1)?
            .map(|v| v.max(0) as u64);
        let max_agent_steps: Option<u64> = row
            .try_get::<_, Option<i64>>(2)?
            .map(|v| v.max(0) as u64);
        let max_retries: Option<u64> = row
            .try_get::<_, Option<i64>>(3)?
            .map(|v| v.max(0) as u64);

        let tool_calls = self.event_count_for_trace(trace_id, "pi.tool_call").await?;
        let agent_steps = self.event_count_for_trace(trace_id, "pi.turn_end").await?;
        let provider_calls = self.event_count_for_trace(trace_id, "pi.provider_call.started").await?;
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

    async fn event_count_for_trace(&self, trace_id: &str, kind: &str) -> Result<u64, NoetError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT COUNT(*) FROM events WHERE trace_id = $1 AND kind = $2",
                &[&trace_id, &kind],
            )
            .await?;
        let row = rows.first().ok_or_else(|| NoetError::Database("event_count_for_trace: no row returned".into()))?;
        let count: i64 = row.try_get::<_, i64>(0)?;
        Ok(count.max(0) as u64)
    }

    pub fn event_count(&self) -> usize {
        self.events_count.load(Ordering::Relaxed) as usize
    }
}

// ---------------------------------------------------------------------------
// Shared row-mapping helper for decisions queries
// ---------------------------------------------------------------------------

fn pg_decision_row_to_trace_item(
    row: &tokio_postgres::Row,
) -> Result<TraceReportItem, tokio_postgres::Error> {
    let outcome: String = row.try_get(1)?;
    let decision_id: String = row.try_get(2)?;
    let trace_id: Option<String> = row.try_get(3)?;
    let request_id: Option<String> = row.try_get(4)?;
    let provider: Option<String> = row.try_get(5)?;
    let model: Option<String> = row.try_get(6)?;
    let action: String = row.try_get(7)?;
    let estimated_tokens: Option<i64> = row.try_get(8)?;
    let estimated_cost_usd: Option<f64> = row.try_get(9)?;
    let explanations_json: serde_json::Value = row.try_get(10)?;
    let metadata_json: serde_json::Value = row.try_get(11)?;
    let entities_json: serde_json::Value = row.try_get(12)?;
    let selected_budget_id: Option<String> = row.try_get(13)?;
    let matched_entity: Option<String> = row.try_get(14)?;
    let selection_reason: Option<String> = row.try_get(15)?;
    let rejected_budget_id: Option<String> = row.try_get(16)?;
    let rejected_budget_reason: Option<String> = row.try_get(17)?;
    let model_check: Option<String> = row.try_get(18)?;
    let budget_window_remaining_usd: Option<f64> = row.try_get(19)?;
    let routing_json: Option<serde_json::Value> = row.try_get(20)?;
    let limit_hits_json: Option<serde_json::Value> = row.try_get(21)?;
    let agent_run_id: Option<String> = row.try_get(22)?;

    let explanations_json_str = explanations_json.to_string();
    let metadata_json_str = metadata_json.to_string();
    let entities_json_str = entities_json.to_string();
    let routing_json_str = routing_json.as_ref().map(|v| v.to_string());
    let limit_hits_json_str = limit_hits_json.as_ref().map(|v| v.to_string());

    let primary_rule_id = selected_budget_id
        .clone()
        .or_else(|| primary_explanation_rule_id(&explanations_json_str));
    let mut routing = routing_json_str
        .as_deref()
        .and_then(parse_optional_json::<DecisionRoutingReport>)
        .or_else(|| {
            decision_routing_report(
                primary_rule_id.clone(),
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
    if let Some(routing) = routing.as_mut()
        && routing.selected_budget_id.is_none()
    {
        routing.selected_budget_id = primary_rule_id.clone();
    }
    let limit_hits = limit_hits_json_str
        .as_deref()
        .and_then(parse_optional_json::<Vec<DecisionLimitHitReport>>)
        .filter(|hits| !hits.is_empty())
        .or_else(|| limit_hits_from_explanations_json(&explanations_json_str));
    let summary = DecisionSummary {
        action: &action,
        decision_id: &decision_id,
        trace_id: trace_id.as_deref(),
        request_id: request_id.as_deref(),
        provider: provider.as_deref(),
        model: model.as_deref(),
        estimated_tokens,
        estimated_cost_usd,
        metadata_json: &metadata_json_str,
        limit_hits: limit_hits.as_deref(),
        routing: DecisionRoutingSummary {
            selected_budget_id: routing
                .as_ref()
                .and_then(|r| r.selected_budget_id.as_deref())
                .or(primary_rule_id.as_deref()),
            matched_entity: routing
                .as_ref()
                .and_then(|r| r.matched_entity.as_deref())
                .or(matched_entity.as_deref()),
            selection_reason: routing
                .as_ref()
                .and_then(|r| r.selection_reason.as_deref())
                .or(selection_reason.as_deref()),
            rejected_budget_id: routing
                .as_ref()
                .and_then(|r| r.rejected_budget_id.as_deref())
                .or(rejected_budget_id.as_deref()),
            rejected_budget_reason: routing
                .as_ref()
                .and_then(|r| r.rejected_budget_reason.as_deref())
                .or(rejected_budget_reason.as_deref()),
            model_check: routing
                .as_ref()
                .and_then(|r| r.model_check.as_deref())
                .or(model_check.as_deref()),
            budget_window_remaining_usd: routing
                .as_ref()
                .and_then(|r| r.budget_window_remaining_usd)
                .or(budget_window_remaining_usd),
            budget_window_mode: routing
                .as_ref()
                .and_then(|r| r.budget_window_mode.as_deref()),
            budget_window_started_at: routing
                .as_ref()
                .and_then(|r| r.budget_window_started_at),
            budget_window_ends_at: routing
                .as_ref()
                .and_then(|r| r.budget_window_ends_at),
        },
    };
    Ok(TraceReportItem {
        occurred_at: parse_time(row.try_get::<_, String>(0)?),
        kind: format!("decision.{outcome}"),
        summary: summarize_decision(summary),
        trace_id,
        agent_run_id,
        entities: parse_entities_json(entities_json_str),
        binding_limit: limit_hits.as_deref().and_then(binding_limit_hit).cloned(),
        routing,
        limit_hits,
    })
}

fn most_common_count_pg(values: Option<&std::collections::HashMap<String, u64>>) -> Option<String> {
    values?
        .iter()
        .max_by(|(lv, lc), (rv, rc)| lc.cmp(rc).then_with(|| rv.cmp(lv)))
        .map(|(v, _)| v.clone())
}
