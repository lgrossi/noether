/// Phase 7 parity test infrastructure.
///
/// # CI usage
///
///   # SQLite only (default):
///   cargo test
///
///   # Both backends:
///   export NOET_TEST_PG_URL=postgres://noether:test@localhost:5433/noether
///   psql $NOET_TEST_PG_URL -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;"
///   cargo test
///
/// PG variants are silently skipped when NOET_TEST_PG_URL is unset.

use std::sync::Arc;
use noether::backend::Backend;
use noether::contract::DecisionMode;
use noether::policy::PolicyFile;
use noether::server::AppState;

// Re-exported so macro call sites compile.
pub use tempfile::TempDir;
pub use noether::contract::DecisionMode as _DecisionMode;

// ---------------------------------------------------------------------------
// SQLite helper
// ---------------------------------------------------------------------------

/// Returns a fresh tempfile-backed SQLite `Backend`.
///
/// The `TempDir` guard must be kept alive for the duration of the test.
pub fn make_sqlite_backend() -> (Backend, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("noether-test.sqlite");
    let ledger = noether::ledger::BudgetLedger::open_sqlite(&db_path)
        .expect("open_sqlite");
    let handler_conn = rusqlite::Connection::open(&db_path).expect("rusqlite open");
    drop(ledger);
    let conn = Arc::new(std::sync::Mutex::new(Some(handler_conn)));
    let db_url = noether::server::path_to_sqlite_url(&db_path);
    (Backend::sqlite_from_url(db_url, conn), dir)
}

// ---------------------------------------------------------------------------
// Postgres helpers
// ---------------------------------------------------------------------------

/// A live Postgres backend scoped to an isolated schema.
///
/// Drops the schema when this struct is dropped, so keep it alive for the
/// duration of the test.
pub struct PgTestGuard {
    pub schema: String,
    pub admin_url: String,
}

impl Drop for PgTestGuard {
    fn drop(&mut self) {
        let schema = self.schema.clone();
        let url = self.admin_url.clone();
        let drop_sql = format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cleanup runtime");
            rt.block_on(async move {
                if let Ok((client, conn)) =
                    tokio_postgres::connect(&url, tokio_postgres::NoTls).await
                {
                    tokio::spawn(async move { let _ = conn.await; });
                    client.batch_execute(&drop_sql).await.ok();
                }
            });
        })
        .join();
    }
}

/// Create a fresh isolated PG backend.
///
/// Returns `None` when `NOET_TEST_PG_URL` is not set — callers should skip.
pub async fn make_pg_backend(test_name: &str) -> Option<(Backend, PgTestGuard)> {
    let pg_url = match std::env::var("NOET_TEST_PG_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => return None,
    };

    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let safe_name: String = test_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(30)
        .collect();
    let schema = format!("test_{safe_name}_{epoch}");

    let pg_backend = noether::backend::create_simulation_pg_schema(&pg_url, &schema)
        .await
        .expect("create_simulation_pg_schema");

    let guard = PgTestGuard {
        schema,
        admin_url: pg_url,
    };
    Some((Backend::Postgres(pg_backend), guard))
}

// ---------------------------------------------------------------------------
// AppState factory
// ---------------------------------------------------------------------------

/// Build an `AppState` wired to the given backend.
pub fn make_state(
    backend: Backend,
    policy: Option<PolicyFile>,
    decision_mode: DecisionMode,
) -> AppState {
    noether::server::build_state_with_backend(
        Arc::new(backend),
        std::path::PathBuf::from(".noet/test-fixtures"),
        policy,
        decision_mode,
    )
}

// ---------------------------------------------------------------------------
// run_parity_test: run an async test body against both SQLite and PG.
// ---------------------------------------------------------------------------

/// Run an async closure against both SQLite and Postgres backends.
///
/// The closure receives an `AppState` wired to the respective backend.
/// The PG run is skipped silently if `NOET_TEST_PG_URL` is not set.
///
/// `test_name` is used to name the isolated PG schema.
pub async fn run_parity<F, Fut>(
    test_name: &str,
    policy: Option<PolicyFile>,
    mode: DecisionMode,
    body: F,
) where
    F: Fn(AppState) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // SQLite run.
    {
        let (backend, _dir) = make_sqlite_backend();
        let state = make_state(backend, policy.clone(), mode);
        body(state).await;
    }

    // PG run.
    match make_pg_backend(test_name).await {
        None => {
            eprintln!("{test_name} (PG): NOET_TEST_PG_URL not set — skipping");
        }
        Some((backend, _guard)) => {
            let state = make_state(backend, policy, mode);
            body(state).await;
        }
    }
}

// ---------------------------------------------------------------------------
// backend_test! macro
// ---------------------------------------------------------------------------
//
// Generates two #[tokio::test] functions:
//   {name}_sqlite  — always runs
//   {name}_pg      — skipped unless NOET_TEST_PG_URL is set
//
// The macro delegates to `run_parity` which handles guard lifetimes cleanly.
//
// Usage:
//
//   backend_test!(my_test, || async move |state: AppState| {
//       // assertions
//   });
//
//   // With explicit policy and mode:
//   backend_test!(my_test, my_policy(), DecisionMode::Enforce, |state| {
//       // body
//   });

#[macro_export]
macro_rules! backend_test {
    // Full form: name, policy, mode, |state| body
    ($name:ident, $policy:expr, $mode:expr, |$state:ident| $body:expr) => {
        paste::paste! {
            #[tokio::test]
            async fn [<$name _sqlite>]() {
                let (backend, _dir) = $crate::common::make_sqlite_backend();
                let $state = $crate::common::make_state(backend, $policy, $mode);
                $body
            }

            #[tokio::test]
            async fn [<$name _pg>]() {
                match $crate::common::make_pg_backend(stringify!($name)).await {
                    None => {
                        eprintln!(
                            concat!(stringify!($name), "_pg: NOET_TEST_PG_URL not set — skipping")
                        );
                    }
                    Some((backend, _guard)) => {
                        let $state = $crate::common::make_state(backend, $policy, $mode);
                        $body
                    }
                }
            }
        }
    };
    // Convenience form: name, |state| body  (None policy, DryRun mode)
    ($name:ident, |$state:ident| $body:expr) => {
        $crate::backend_test!(
            $name,
            None,
            noether::contract::DecisionMode::DryRun,
            |$state| $body
        );
    };
}
