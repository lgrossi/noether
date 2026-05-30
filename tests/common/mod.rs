use std::path::PathBuf;
use std::sync::Arc;

use noether::contract::DecisionMode;
use noether::ledger::{AsyncPostgresLedger, BudgetLedger};
use noether::policy::PolicyFile;
use noether::server::{AppState, LedgerBackend};
use tokio::sync::Mutex;

pub fn sqlite_state(
    policy: Option<PolicyFile>,
    decision_mode: DecisionMode,
) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("noether-test.sqlite");
    let ledger = BudgetLedger::open_sqlite(&db_path).expect("sqlite ledger");
    let mut state = AppState::new(
        PathBuf::from(".noet/test-fixtures"),
        None,
        policy,
        decision_mode,
    );
    state.ledger = Arc::new(Mutex::new(ledger));
    state.ledger_backend = LedgerBackend::sqlite(db_path);
    (state, dir)
}

pub struct PostgresTestSchema {
    database_url: String,
    schema: String,
}

impl Drop for PostgresTestSchema {
    fn drop(&mut self) {
        let database_url = self.database_url.clone();
        let schema = self.schema.clone();
        let _ = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("postgres cleanup runtime");
            runtime.block_on(async move {
                if let Ok((admin, connection)) =
                    tokio_postgres::connect(&database_url, tokio_postgres::NoTls).await
                {
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });
                    let _ = admin
                        .batch_execute(&format!(
                            r#"
                            SET lock_timeout = '2s';
                            DROP SCHEMA IF EXISTS "{schema}" CASCADE;
                            "#
                        ))
                        .await;
                }
            });
        })
        .join();
    }
}

pub async fn postgres_state(
    policy: Option<PolicyFile>,
    decision_mode: DecisionMode,
) -> Option<(AppState, PostgresTestSchema)> {
    let database_url = match std::env::var("NOET_TEST_POSTGRES_URL") {
        Ok(value) if !value.is_empty() => value,
        _ => return None,
    };
    let schema = format!("noether_parity_{}", uuid::Uuid::new_v4().simple());
    let (admin, connection) = tokio_postgres::connect(&database_url, tokio_postgres::NoTls)
        .await
        .expect("postgres admin connection");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    admin
        .batch_execute(&format!(
            r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE; CREATE SCHEMA "{schema}";"#
        ))
        .await
        .expect("create postgres test schema");

    let separator = if database_url.contains('?') { '&' } else { '?' };
    let scoped_url = format!("{database_url}{separator}options=-csearch_path%3D{schema}");
    let postgres_ledger = AsyncPostgresLedger::connect(&scoped_url)
        .await
        .expect("postgres ledger");
    let mut state = AppState::new(
        PathBuf::from(".noet/test-fixtures"),
        None,
        policy,
        decision_mode,
    );
    state.ledger_backend = LedgerBackend::postgres(scoped_url, postgres_ledger);
    Some((
        state,
        PostgresTestSchema {
            database_url,
            schema,
        },
    ))
}

pub async fn run_server_parity<F, Fut>(
    name: &str,
    policy: Option<PolicyFile>,
    decision_mode: DecisionMode,
    body: F,
) where
    F: Fn(AppState) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (state, _sqlite_dir) = sqlite_state(policy.clone(), decision_mode);
    body(state).await;

    match postgres_state(policy, decision_mode).await {
        Some((state, postgres_schema)) => {
            body(state.clone()).await;
            drop(state);
            drop(postgres_schema);
        }
        None => eprintln!("{name}: NOET_TEST_POSTGRES_URL not set; skipped Postgres parity run"),
    }
}
