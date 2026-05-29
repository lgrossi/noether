use std::path::{Path, PathBuf};
use std::sync::Arc;

use deadpool_postgres::Pool;

use crate::error::NoetError;
use crate::ledger::ConnMutex;

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
            .max_size(4)
            .build()
            .map_err(|e| NoetError::InvalidConfig(format!("failed to build postgres pool: {e}")))?;
        Ok(Backend::Postgres(PostgresBackend {
            pool: Arc::new(pool),
            db_url,
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
