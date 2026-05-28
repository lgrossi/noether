use std::collections::BTreeMap;
use std::error::Error;
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use noether::contract::{AuthorizeRequest, FinalizeReservation};
use noether::ledger::BudgetLedger;
use noether::policy::parse_policy_bytes;
use serde_json::Value;
use tokio_postgres::{Client, NoTls};

const ROLLING_POLICY: &str = r#"
version: 0
budgets:
  - id: bench-project
    limits:
      spend:
        - id: bench-rolling-cap
          window: 1h
          mode: rolling
          max_usd: 1000000
          warn_at_fraction: 0.8
          action: block
    match:
      project: noether
policies: []
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = BenchConfig::from_env()?;

    println!(
        "noet-db-conn-bench iterations={} fresh_iterations={} database_url={}",
        config.iterations,
        config.fresh_iterations,
        redact_database_url(&config.database_url)
    );
    println!("name,count,min_ms,p50_ms,p95_ms,p99_ms,max_ms,avg_ms");

    bench_sqlite_authorize(config.iterations)?;
    bench_sqlite_finalize(config.iterations)?;
    bench_postgres_select_one(&config).await?;
    bench_postgres_empty_statement(&config).await?;
    bench_postgres_indexed_row_lookup(&config).await?;
    bench_postgres_heap_row_lookup(&config).await?;
    bench_postgres_minimal_insert(&config).await?;
    bench_postgres_minimal_update(&config).await?;
    bench_postgres_minimal_upsert_insert_path(&config).await?;
    bench_postgres_minimal_upsert_update_path(&config).await?;
    bench_postgres_insert_decision_only(&config).await?;
    bench_postgres_authorize_budget_critical_only(&config).await?;
    bench_postgres_authorize_counter_only(&config).await?;
    bench_postgres_reused_client(&config).await?;
    bench_postgres_prepared_single_statement(&config).await?;
    bench_postgres_finalize_reused_client(&config).await?;
    bench_postgres_finalize_prepared_single_statement(&config).await?;
    bench_postgres_fresh_connection(&config).await?;

    Ok(())
}

#[derive(Debug)]
struct BenchConfig {
    database_url: String,
    iterations: usize,
    fresh_iterations: usize,
}

impl BenchConfig {
    fn from_env() -> Result<Self, Box<dyn Error>> {
        let mut database_url = std::env::var("NOET_BENCH_POSTGRES_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| "postgres://noether:noether@127.0.0.1:55432/noether".to_owned());
        let mut iterations = 500;
        let mut fresh_iterations = 100;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--database-url" => {
                    database_url = args.next().ok_or("--database-url requires a value")?;
                }
                "--iterations" => {
                    iterations = args
                        .next()
                        .ok_or("--iterations requires a value")?
                        .parse::<usize>()?;
                }
                "--fresh-iterations" => {
                    fresh_iterations = args
                        .next()
                        .ok_or("--fresh-iterations requires a value")?
                        .parse::<usize>()?;
                }
                "--help" | "-h" => {
                    println!(
                        "Usage: cargo run --release --bin noet-db-conn-bench -- [--database-url URL] [--iterations N] [--fresh-iterations N]"
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other}").into()),
            }
        }

        Ok(Self {
            database_url,
            iterations,
            fresh_iterations,
        })
    }
}

fn bench_sqlite_authorize(iterations: usize) -> Result<(), Box<dyn Error>> {
    let policy = parse_policy_bytes(ROLLING_POLICY.as_bytes())?;
    let db_path = std::env::temp_dir().join(format!(
        "noether-db-conn-bench-{}.sqlite",
        std::process::id()
    ));
    remove_sqlite_files(&db_path);

    let mut ledger = BudgetLedger::open_sqlite(&db_path)?;
    let base_time = Utc::now();
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let request = authorize_request(index);
        let now = base_time + ChronoDuration::milliseconds(index as i64);
        let started = Instant::now();
        ledger.try_authorize_at(Some(&policy), &request, now)?;
        samples.push(started.elapsed());
    }

    print_summary("sqlite actual BudgetLedger::try_authorize rolling", &samples);
    remove_sqlite_files(&db_path);
    Ok(())
}

fn bench_sqlite_finalize(iterations: usize) -> Result<(), Box<dyn Error>> {
    let policy = parse_policy_bytes(ROLLING_POLICY.as_bytes())?;
    let db_path = std::env::temp_dir().join(format!(
        "noether-db-conn-bench-finalize-{}.sqlite",
        std::process::id()
    ));
    remove_sqlite_files(&db_path);

    let mut ledger = BudgetLedger::open_sqlite(&db_path)?;
    let base_time = Utc::now();
    let mut reservation_ids = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let request = authorize_request(index);
        let now = base_time + ChronoDuration::milliseconds(index as i64);
        let decision = ledger.try_authorize_at(Some(&policy), &request, now)?;
        let reservation_id = decision
            .reservation
            .ok_or("authorize response did not include reservation")?
            .id;
        reservation_ids.push(reservation_id);
    }

    let mut samples = Vec::with_capacity(iterations);
    for (index, reservation_id) in reservation_ids.iter().enumerate() {
        let payload = finalize_payload(index);
        let started = Instant::now();
        ledger.finalize(reservation_id, &payload)?;
        samples.push(started.elapsed());
    }

    print_summary("sqlite actual BudgetLedger::finalize", &samples);
    remove_sqlite_files(&db_path);
    Ok(())
}

async fn bench_postgres_indexed_row_lookup(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_row_lookup", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    client
        .batch_execute(&format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}".items (
                id BIGINT PRIMARY KEY,
                label TEXT NOT NULL,
                amount DOUBLE PRECISION NOT NULL,
                payload_json TEXT NOT NULL
            );
            INSERT INTO "{schema}".items (id, label, amount, payload_json)
            SELECT gs, 'item-' || gs, gs::float8 / 1000,
                   '{{"kind":"bench","value":' || gs || '}}'
            FROM generate_series(1, 10000) gs;
            "#
        ))
        .await?;
    let statement = client
        .prepare(&format!(
            r#"SELECT label, amount, payload_json FROM "{schema}".items WHERE id = $1"#
        ))
        .await?;
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let id = ((index % 10_000) + 1) as i64;
        let started = Instant::now();
        let _ = client.query_one(&statement, &[&id]).await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres prepared indexed row lookup", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_heap_row_lookup(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_heap_lookup", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    client
        .batch_execute(&format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}".items (
                id BIGINT NOT NULL,
                label TEXT NOT NULL,
                amount DOUBLE PRECISION NOT NULL,
                payload_json TEXT NOT NULL
            );
            INSERT INTO "{schema}".items (id, label, amount, payload_json)
            SELECT gs, 'item-' || gs, gs::float8 / 1000,
                   '{{"kind":"bench","value":' || gs || '}}'
            FROM generate_series(1, 10000) gs;
            "#
        ))
        .await?;
    let statement = client
        .prepare(&format!(
            r#"SELECT label, amount, payload_json FROM "{schema}".items WHERE id = $1"#
        ))
        .await?;
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let id = ((index % 100) + 1) as i64;
        let started = Instant::now();
        let _ = client.query_one(&statement, &[&id]).await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres prepared heap row lookup", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_select_one(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    let statement = client.prepare("SELECT 1").await?;
    let mut samples = Vec::with_capacity(config.iterations);
    for _ in 0..config.iterations {
        let started = Instant::now();
        let _ = client.query_one(&statement, &[]).await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres prepared SELECT 1", &samples);
    Ok(())
}

async fn bench_postgres_empty_statement(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    let statement = client.prepare("").await?;
    let mut samples = Vec::with_capacity(config.iterations);
    for _ in 0..config.iterations {
        let started = Instant::now();
        let _ = client.execute(&statement, &[]).await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres prepared empty statement", &samples);
    Ok(())
}

async fn bench_postgres_minimal_insert(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_minimal_insert", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    setup_postgres_minimal_table(&client, &schema).await?;
    let statement = client
        .prepare(&format!(
            r#"INSERT INTO "{schema}".items (id, amount) VALUES ($1, $2)"#
        ))
        .await?;
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let id = index as i64;
        let amount = index as f64 / 1000.0;
        let started = Instant::now();
        client.execute(&statement, &[&id, &amount]).await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres minimal insert pk", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_minimal_update(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_minimal_update", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    setup_postgres_minimal_table(&client, &schema).await?;
    client
        .execute(
            &format!(
                r#"INSERT INTO "{schema}".items (id, amount) SELECT gs, 0 FROM generate_series(0, {}) gs"#,
                config.iterations.saturating_sub(1)
            ),
            &[],
        )
        .await?;
    let statement = client
        .prepare(&format!(
            r#"UPDATE "{schema}".items SET amount = amount + $2 WHERE id = $1"#
        ))
        .await?;
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let id = index as i64;
        let amount = 0.001_f64;
        let started = Instant::now();
        client.execute(&statement, &[&id, &amount]).await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres minimal update pk", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_minimal_upsert_insert_path(
    config: &BenchConfig,
) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_minimal_upsert_insert", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    setup_postgres_minimal_table(&client, &schema).await?;
    let statement = client
        .prepare(&format!(
            r#"
            INSERT INTO "{schema}".items (id, amount) VALUES ($1, $2)
            ON CONFLICT(id) DO UPDATE SET amount = "{schema}".items.amount + EXCLUDED.amount
            "#
        ))
        .await?;
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let id = index as i64;
        let amount = 0.001_f64;
        let started = Instant::now();
        client.execute(&statement, &[&id, &amount]).await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres minimal upsert insert-path", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_minimal_upsert_update_path(
    config: &BenchConfig,
) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_minimal_upsert_update", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    setup_postgres_minimal_table(&client, &schema).await?;
    client
        .execute(
            &format!(
                r#"INSERT INTO "{schema}".items (id, amount) SELECT gs, 0 FROM generate_series(0, {}) gs"#,
                config.iterations.saturating_sub(1)
            ),
            &[],
        )
        .await?;
    let statement = client
        .prepare(&format!(
            r#"
            INSERT INTO "{schema}".items (id, amount) VALUES ($1, $2)
            ON CONFLICT(id) DO UPDATE SET amount = "{schema}".items.amount + EXCLUDED.amount
            "#
        ))
        .await?;
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let id = index as i64;
        let amount = 0.001_f64;
        let started = Instant::now();
        client.execute(&statement, &[&id, &amount]).await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres minimal upsert update-path", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_insert_decision_only(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_decision_only", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    setup_postgres_schema(&client, &schema).await?;
    let statement = client
        .prepare(&format!(
            r#"
            INSERT INTO "{schema}".decisions (
                decision_id, trace_id, request_id, project, provider, model,
                estimated_cost_usd, outcome, action, explanations_json, metadata_json,
                entities_json, selected_budget_id, matched_entity, limit_hits_json,
                app_run_key, created_at
            ) VALUES ($1,$2,$3,'noether','openai-codex','gpt-small-bench',$4,
                'allow','allow',
                '[{{"rule_id":"bench-project","reason":"selected fallback budget","severity":"info"}}]',
                $5,$6,'bench-project','project:noether','[]',$7,$8)
            "#
        ))
        .await?;
    let base_time = Utc::now();
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let cost = 0.001 + ((index % 200) as f64 / 100_000.0);
        let trace_id = format!("bench-pg-decision-only-trace-{index}");
        let request_id = format!("bench-pg-decision-only-request-{index}");
        let agent_run_id = format!("bench-pg-decision-only-run-{index}");
        let decision_id = format!("bench-pg-decision-only-decision-{index}");
        let subject = format!("user:bench-{}", index % 12);
        let metadata_json = format!(
            r#"{{"trace_id":"{trace_id}","request_id":"{request_id}","agent_run_id":"{agent_run_id}"}}"#
        );
        let entities_json = format!(r#"["project:noether","{subject}"]"#);
        let app_run_key = format!("agent-run:{agent_run_id}");
        let created_at = (base_time + ChronoDuration::milliseconds(index as i64)).to_rfc3339();
        let started = Instant::now();
        client
            .execute(
                &statement,
                &[
                    &decision_id,
                    &trace_id,
                    &request_id,
                    &cost,
                    &metadata_json,
                    &entities_json,
                    &app_run_key,
                    &created_at,
                ],
            )
            .await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres prepared insert decision only", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_authorize_budget_critical_only(
    config: &BenchConfig,
) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_budget_only", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    setup_postgres_budget_only_schema(&client, &schema).await?;
    let statement = client
        .prepare(&format!(
            r#"
            WITH inserted_reservation AS (
                INSERT INTO "{schema}".reservations (
                    id, amount_usd, status, created_at, expires_at
                ) VALUES ($1, $2, 'active', $3, $4)
                RETURNING id
            ),
            upserted_window AS (
                INSERT INTO "{schema}".limit_window_states (
                    rule_id, limit_id, scope_key, started_at, used_usd
                ) VALUES ('bench-project', 'bench-budget-cap', 'project:noether', $3, $2)
                ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
                    started_at = EXCLUDED.started_at,
                    used_usd = "{schema}".limit_window_states.used_usd + EXCLUDED.used_usd
                RETURNING 1
            ),
            upserted_bucket AS (
                INSERT INTO "{schema}".rolling_spend_buckets (
                    rule_id, limit_id, scope_key, bucket_start, amount_usd
                ) VALUES ('bench-project', 'bench-budget-cap', 'project:noether', $3, $2)
                ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
                    amount_usd = "{schema}".rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd
                RETURNING 1
            )
            SELECT 1 FROM inserted_reservation, upserted_window, upserted_bucket
            "#
        ))
        .await?;
    let base_time = Utc::now();
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let reservation_id = format!("bench-pg-budget-only-reservation-{index}");
        let cost = 0.001 + ((index % 200) as f64 / 100_000.0);
        let created_at = (base_time + ChronoDuration::milliseconds(index as i64)).to_rfc3339();
        let expires_at = (base_time + ChronoDuration::hours(1)).to_rfc3339();
        let started = Instant::now();
        client
            .execute(&statement, &[&reservation_id, &cost, &created_at, &expires_at])
            .await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres prepared auth budget-critical only", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_authorize_counter_only(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_counter_only", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });
    setup_postgres_budget_only_schema(&client, &schema).await?;
    let statement = client
        .prepare(&format!(
            r#"
            WITH upserted_window AS (
                INSERT INTO "{schema}".limit_window_states (
                    rule_id, limit_id, scope_key, started_at, used_usd
                ) VALUES ('bench-project', 'bench-budget-cap', 'project:noether', $1, $2)
                ON CONFLICT(rule_id, limit_id, scope_key) DO UPDATE SET
                    started_at = EXCLUDED.started_at,
                    used_usd = "{schema}".limit_window_states.used_usd + EXCLUDED.used_usd
                RETURNING 1
            ),
            upserted_bucket AS (
                INSERT INTO "{schema}".rolling_spend_buckets (
                    rule_id, limit_id, scope_key, bucket_start, amount_usd
                ) VALUES ('bench-project', 'bench-budget-cap', 'project:noether', $1, $2)
                ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
                    amount_usd = "{schema}".rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd
                RETURNING 1
            )
            SELECT 1 FROM upserted_window, upserted_bucket
            "#
        ))
        .await?;
    let base_time = Utc::now();
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let cost = 0.001 + ((index % 200) as f64 / 100_000.0);
        let created_at = (base_time + ChronoDuration::milliseconds(index as i64)).to_rfc3339();
        let started = Instant::now();
        client.execute(&statement, &[&created_at, &cost]).await?;
        samples.push(started.elapsed());
    }
    print_summary("postgres prepared auth counter only", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_reused_client(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_reused", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });

    setup_postgres_schema(&client, &schema).await?;

    let base_time = Utc::now();
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let now = base_time + ChronoDuration::milliseconds(index as i64);
        let started = Instant::now();
        postgres_hot_authorize_like_op(&client, &schema, index, now.to_rfc3339()).await?;
        samples.push(started.elapsed());
    }

    print_summary("postgres reused client hot-op", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_prepared_single_statement(
    config: &BenchConfig,
) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_prepared", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });

    setup_postgres_schema(&client, &schema).await?;
    let statement = client
        .prepare(&postgres_hot_authorize_like_single_statement_sql(&schema))
        .await?;

    let base_time = Utc::now();
    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let cost = 0.001 + ((index % 200) as f64 / 100_000.0);
        let trace_id = format!("bench-pg-prepared-trace-{index}");
        let request_id = format!("bench-pg-prepared-request-{index}");
        let agent_run_id = format!("bench-pg-prepared-run-{index}");
        let decision_id = format!("bench-pg-prepared-decision-{index}");
        let reservation_id = format!("bench-pg-prepared-reservation-{index}");
        let created_at = (base_time + ChronoDuration::milliseconds(index as i64)).to_rfc3339();
        let expires_at = (base_time
            + ChronoDuration::milliseconds(index as i64)
            + ChronoDuration::hours(1))
        .to_rfc3339();
        let subject = format!("user:bench-{}", index % 12);
        let metadata_json = format!(
            r#"{{"trace_id":"{trace_id}","request_id":"{request_id}","agent_run_id":"{agent_run_id}"}}"#
        );
        let entities_json = format!(r#"["project:noether","{subject}"]"#);
        let app_run_key = format!("agent-run:{agent_run_id}");

        let started = Instant::now();
        client
            .execute(
                &statement,
                &[
                    &decision_id,
                    &trace_id,
                    &request_id,
                    &cost,
                    &metadata_json,
                    &entities_json,
                    &app_run_key,
                    &created_at,
                    &reservation_id,
                    &expires_at,
                ],
            )
            .await?;
        samples.push(started.elapsed());
    }

    print_summary("postgres prepared single-statement hot-op", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_finalize_reused_client(
    config: &BenchConfig,
) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_finalize_reused", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });

    setup_postgres_schema(&client, &schema).await?;
    seed_postgres_finalizable_reservations(&client, &schema, config.iterations).await?;

    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let reservation_id = format!("bench-pg-finalize-reservation-{index}");
        let started = Instant::now();
        postgres_finalize_like_op(&client, &schema, index, &reservation_id).await?;
        samples.push(started.elapsed());
    }

    print_summary("postgres reused client finalize-op", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_finalize_prepared_single_statement(
    config: &BenchConfig,
) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_finalize_prepared", std::process::id());
    let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("postgres connection error: {error}");
        }
    });

    setup_postgres_schema(&client, &schema).await?;
    seed_postgres_finalizable_reservations(&client, &schema, config.iterations).await?;
    let statement = client
        .prepare(&postgres_finalize_like_single_statement_sql(&schema))
        .await?;

    let mut samples = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let reservation_id = format!("bench-pg-finalize-reservation-{index}");
        let cost = 0.001 + ((index % 200) as f64 / 100_000.0);
        let now = Utc::now().to_rfc3339();
        let metadata_json = format!(
            r#"{{"trace_id":"bench-pg-finalize-trace-{index}","request_id":"bench-pg-finalize-request-{index}","agent_run_id":"bench-pg-finalize-run-{index}"}}"#
        );
        let usage_id = format!("bench-pg-finalize-usage-{index}");
        let input_tokens = 500 + (index % 1000) as i64;
        let output_tokens = 100 + (index % 300) as i64;
        let total_tokens = input_tokens + output_tokens;
        let latency_ms = 900 + (index % 300) as i64;

        let started = Instant::now();
        client
            .execute(
                &statement,
                &[
                    &reservation_id,
                    &cost,
                    &now,
                    &usage_id,
                    &input_tokens,
                    &output_tokens,
                    &total_tokens,
                    &latency_ms,
                    &metadata_json,
                ],
            )
            .await?;
        samples.push(started.elapsed());
    }

    print_summary("postgres prepared single-statement finalize-op", &samples);
    drop_postgres_schema(&client, &schema).await?;
    Ok(())
}

async fn bench_postgres_fresh_connection(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let schema = format!("noether_bench_{}_fresh", std::process::id());
    let (setup_client, setup_connection) =
        tokio_postgres::connect(&config.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = setup_connection.await {
            eprintln!("postgres setup connection error: {error}");
        }
    });
    setup_postgres_schema(&setup_client, &schema).await?;

    let base_time = Utc::now();
    let mut samples = Vec::with_capacity(config.fresh_iterations);
    for index in 0..config.fresh_iterations {
        let now = base_time + ChronoDuration::milliseconds(index as i64);
        let started = Instant::now();
        let (client, connection) = tokio_postgres::connect(&config.database_url, NoTls).await?;
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                eprintln!("postgres fresh connection error: {error}");
            }
        });
        postgres_hot_authorize_like_op(&client, &schema, index, now.to_rfc3339()).await?;
        samples.push(started.elapsed());
    }

    print_summary("postgres fresh connection hot-op", &samples);
    drop_postgres_schema(&setup_client, &schema).await?;
    Ok(())
}

async fn setup_postgres_schema(client: &Client, schema: &str) -> Result<(), Box<dyn Error>> {
    client
        .batch_execute(&format!(
            r#"
            DROP SCHEMA IF EXISTS "{schema}" CASCADE;
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}".decisions (
                decision_id TEXT PRIMARY KEY,
                trace_id TEXT,
                request_id TEXT,
                project TEXT,
                provider TEXT,
                model TEXT,
                estimated_cost_usd DOUBLE PRECISION,
                outcome TEXT NOT NULL,
                action TEXT NOT NULL,
                explanations_json TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                entities_json TEXT NOT NULL,
                selected_budget_id TEXT,
                matched_entity TEXT,
                limit_hits_json TEXT,
                app_run_key TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX idx_decisions_created ON "{schema}".decisions(created_at);
            CREATE TABLE "{schema}".reservations (
                id TEXT PRIMARY KEY,
                decision_id TEXT NOT NULL REFERENCES "{schema}".decisions(decision_id),
                amount_usd DOUBLE PRECISION NOT NULL,
                estimated_amount_usd DOUBLE PRECISION NOT NULL,
                actual_amount_usd DOUBLE PRECISION,
                currency TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                finalized_at TEXT,
                budget_rule_ids_json TEXT NOT NULL,
                limit_window_spends_json TEXT NOT NULL,
                allocation_spends_json TEXT NOT NULL
            );
            CREATE INDEX idx_reservations_decision ON "{schema}".reservations(decision_id);
            CREATE TABLE "{schema}".reservation_limit_scopes (
                reservation_id TEXT NOT NULL REFERENCES "{schema}".reservations(id),
                rule_id TEXT NOT NULL,
                limit_id TEXT NOT NULL,
                scope_key TEXT NOT NULL,
                amount_usd DOUBLE PRECISION NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX idx_reservation_limit_scopes_rolling
                ON "{schema}".reservation_limit_scopes(rule_id, limit_id, scope_key, created_at);
            CREATE TABLE "{schema}".rolling_spend_buckets (
                rule_id TEXT NOT NULL,
                limit_id TEXT NOT NULL,
                scope_key TEXT NOT NULL,
                bucket_start TEXT NOT NULL,
                amount_usd DOUBLE PRECISION NOT NULL,
                PRIMARY KEY (rule_id, limit_id, scope_key, bucket_start)
            );
            CREATE TABLE "{schema}".usage_observations (
                id TEXT PRIMARY KEY,
                reservation_id TEXT REFERENCES "{schema}".reservations(id),
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
            CREATE INDEX idx_usage_trace ON "{schema}".usage_observations(trace_id);
            "#
        ))
        .await?;
    Ok(())
}

async fn setup_postgres_budget_only_schema(
    client: &Client,
    schema: &str,
) -> Result<(), Box<dyn Error>> {
    client
        .batch_execute(&format!(
            r#"
            DROP SCHEMA IF EXISTS "{schema}" CASCADE;
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}".reservations (
                id TEXT PRIMARY KEY,
                amount_usd DOUBLE PRECISION NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );
            CREATE TABLE "{schema}".limit_window_states (
                rule_id TEXT NOT NULL,
                limit_id TEXT NOT NULL,
                scope_key TEXT NOT NULL,
                started_at TEXT NOT NULL,
                used_usd DOUBLE PRECISION NOT NULL,
                PRIMARY KEY (rule_id, limit_id, scope_key)
            );
            CREATE TABLE "{schema}".rolling_spend_buckets (
                rule_id TEXT NOT NULL,
                limit_id TEXT NOT NULL,
                scope_key TEXT NOT NULL,
                bucket_start TEXT NOT NULL,
                amount_usd DOUBLE PRECISION NOT NULL,
                PRIMARY KEY (rule_id, limit_id, scope_key, bucket_start)
            );
            "#
        ))
        .await?;
    Ok(())
}

async fn setup_postgres_minimal_table(
    client: &Client,
    schema: &str,
) -> Result<(), Box<dyn Error>> {
    client
        .batch_execute(&format!(
            r#"
            DROP SCHEMA IF EXISTS "{schema}" CASCADE;
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}".items (
                id BIGINT PRIMARY KEY,
                amount DOUBLE PRECISION NOT NULL
            );
            "#
        ))
        .await?;
    Ok(())
}

async fn drop_postgres_schema(client: &Client, schema: &str) -> Result<(), Box<dyn Error>> {
    client
        .batch_execute(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE;"#))
        .await?;
    Ok(())
}

async fn postgres_hot_authorize_like_op(
    client: &Client,
    schema: &str,
    index: usize,
    created_at: String,
) -> Result<(), Box<dyn Error>> {
    let cost = 0.001 + ((index % 200) as f64 / 100_000.0);
    let trace_id = format!("bench-pg-trace-{index}");
    let request_id = format!("bench-pg-request-{index}");
    let agent_run_id = format!("bench-pg-run-{index}");
    let decision_id = format!("bench-pg-decision-{index}");
    let reservation_id = format!("bench-pg-reservation-{index}");
    let bucket_start = created_at.clone();
    let expires_at = (Utc::now() + ChronoDuration::hours(1)).to_rfc3339();
    let subject = format!("user:bench-{}", index % 12);
    let metadata_json = format!(
        r#"{{"trace_id":"{trace_id}","request_id":"{request_id}","agent_run_id":"{agent_run_id}"}}"#
    );
    let entities_json = format!(r#"["project:noether","{subject}"]"#);
    let app_run_key = format!("agent-run:{agent_run_id}");

    client.batch_execute("BEGIN").await?;
    let sum_sql = format!(
        r#"
        SELECT COALESCE(SUM(amount_usd), 0)
        FROM "{schema}".rolling_spend_buckets
        WHERE rule_id = $1
          AND limit_id = $2
          AND scope_key = $3
          AND bucket_start >= $4
          AND bucket_start <= $5
        "#
    );
    let _current_spend: f64 = client
        .query_one(
            &sum_sql,
            &[
                &"bench-project",
                &"bench-rolling-cap",
                &"project:noether",
                &created_at,
                &created_at,
            ],
        )
        .await?
        .get(0);
    let insert_decision_sql = format!(
        r#"
        INSERT INTO "{schema}".decisions (
            decision_id, trace_id, request_id, project, provider, model,
            estimated_cost_usd, outcome, action, explanations_json, metadata_json, entities_json,
            selected_budget_id, matched_entity, limit_hits_json, app_run_key, created_at
        ) VALUES ($1,$2,$3,'noether','openai-codex','gpt-small-bench',$4,'allow','allow',
            '[{{"rule_id":"bench-project","reason":"selected fallback budget","severity":"info"}}]',
            $5,$6,'bench-project','project:noether','[]',$7,$8)
        "#
    );
    client
        .execute(
            &insert_decision_sql,
            &[
                &decision_id,
                &trace_id,
                &request_id,
                &cost,
                &metadata_json,
                &entities_json,
                &app_run_key,
                &created_at,
            ],
        )
        .await?;
    let insert_reservation_sql = format!(
        r#"
        INSERT INTO "{schema}".reservations (
            id, decision_id, amount_usd, estimated_amount_usd, actual_amount_usd, currency, status,
            created_at, expires_at, finalized_at, budget_rule_ids_json, limit_window_spends_json, allocation_spends_json
        ) VALUES ($1,$2,$3,$3,NULL,'USD','active',$4,$5,NULL,'["bench-project"]',
            '[{{"rule_id":"bench-project","limit_id":"bench-rolling-cap","scope_key":"project:noether"}}]',
            '[]')
        "#
    );
    client
        .execute(
            &insert_reservation_sql,
            &[&reservation_id, &decision_id, &cost, &created_at, &expires_at],
        )
        .await?;
    let insert_scope_sql = format!(
        r#"
        INSERT INTO "{schema}".reservation_limit_scopes (
            reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at
        ) VALUES ($1,'bench-project','bench-rolling-cap','project:noether',$2,$3)
        "#
    );
    client
        .execute(&insert_scope_sql, &[&reservation_id, &cost, &created_at])
        .await?;
    let upsert_bucket_sql = format!(
        r#"
        INSERT INTO "{schema}".rolling_spend_buckets (
            rule_id, limit_id, scope_key, bucket_start, amount_usd
        ) VALUES ('bench-project','bench-rolling-cap','project:noether',$1,$2)
        ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
            amount_usd = "{schema}".rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd
        "#
    );
    client
        .execute(&upsert_bucket_sql, &[&bucket_start, &cost])
        .await?;
    client.batch_execute("COMMIT").await?;
    Ok(())
}

async fn seed_postgres_finalizable_reservations(
    client: &Client,
    schema: &str,
    iterations: usize,
) -> Result<(), Box<dyn Error>> {
    let base_time = Utc::now();
    let insert_decision = client
        .prepare(&format!(
            r#"
            INSERT INTO "{schema}".decisions (
                decision_id, trace_id, request_id, project, provider, model,
                estimated_cost_usd, outcome, action, explanations_json, metadata_json, entities_json,
                selected_budget_id, matched_entity, limit_hits_json, app_run_key, created_at
            ) VALUES ($1,$2,$3,'noether','openai-codex','gpt-small-bench',$4,'allow','allow',
                '[{{"rule_id":"bench-project","reason":"selected fallback budget","severity":"info"}}]',
                $5,$6,'bench-project','project:noether','[]',$7,$8)
            "#
        ))
        .await?;
    let insert_reservation = client
        .prepare(&format!(
            r#"
            INSERT INTO "{schema}".reservations (
                id, decision_id, amount_usd, estimated_amount_usd, actual_amount_usd, currency, status,
                created_at, expires_at, finalized_at, budget_rule_ids_json, limit_window_spends_json, allocation_spends_json
            ) VALUES ($4,$1,$2,$2,NULL,'USD','active',$3,$5,NULL,'["bench-project"]',
                '[{{"rule_id":"bench-project","limit_id":"bench-rolling-cap","scope_key":"project:noether"}}]',
                '[]')
            "#
        ))
        .await?;
    for index in 0..iterations {
        let cost = 0.001 + ((index % 200) as f64 / 100_000.0);
        let trace_id = format!("bench-pg-finalize-trace-{index}");
        let request_id = format!("bench-pg-finalize-request-{index}");
        let agent_run_id = format!("bench-pg-finalize-run-{index}");
        let decision_id = format!("bench-pg-finalize-decision-{index}");
        let reservation_id = format!("bench-pg-finalize-reservation-{index}");
        let created_at = (base_time + ChronoDuration::milliseconds(index as i64)).to_rfc3339();
        let expires_at = (base_time
            + ChronoDuration::milliseconds(index as i64)
            + ChronoDuration::hours(1))
        .to_rfc3339();
        let subject = format!("user:bench-{}", index % 12);
        let metadata_json = format!(
            r#"{{"trace_id":"{trace_id}","request_id":"{request_id}","agent_run_id":"{agent_run_id}"}}"#
        );
        let entities_json = format!(r#"["project:noether","{subject}"]"#);
        let app_run_key = format!("agent-run:{agent_run_id}");
        client
            .execute(
                &insert_decision,
                &[
                    &decision_id,
                    &trace_id,
                    &request_id,
                    &cost,
                    &metadata_json,
                    &entities_json,
                    &app_run_key,
                    &created_at,
                ],
            )
            .await?;
        client
            .execute(
                &insert_reservation,
                &[
                    &decision_id,
                    &cost,
                    &created_at,
                    &reservation_id,
                    &expires_at,
                ],
            )
            .await?;
    }
    Ok(())
}

async fn postgres_finalize_like_op(
    client: &Client,
    schema: &str,
    index: usize,
    reservation_id: &str,
) -> Result<(), Box<dyn Error>> {
    let cost = 0.001 + ((index % 200) as f64 / 100_000.0);
    let now = Utc::now().to_rfc3339();
    let metadata_json = format!(
        r#"{{"trace_id":"bench-pg-finalize-trace-{index}","request_id":"bench-pg-finalize-request-{index}","agent_run_id":"bench-pg-finalize-run-{index}"}}"#
    );
    let usage_id = format!("bench-pg-finalize-usage-{index}");
    let input_tokens = 500 + (index % 1000) as i64;
    let output_tokens = 100 + (index % 300) as i64;
    let total_tokens = input_tokens + output_tokens;
    let latency_ms = 900 + (index % 300) as i64;

    client.batch_execute("BEGIN").await?;
    client
        .execute(
            &format!(
                r#"
                UPDATE "{schema}".reservations
                SET amount_usd = $2, actual_amount_usd = $2, status = 'finalized', finalized_at = $3
                WHERE id = $1
                "#
            ),
            &[&reservation_id, &cost, &now],
        )
        .await?;
    let trace_id: Option<String> = client
        .query_one(
            &format!(
                r#"
                SELECT d.trace_id
                FROM "{schema}".reservations r
                JOIN "{schema}".decisions d ON d.decision_id = r.decision_id
                WHERE r.id = $1
                "#
            ),
            &[&reservation_id],
        )
        .await?
        .get(0);
    client
        .execute(
            &format!(
                r#"
                INSERT INTO "{schema}".usage_observations (
                    id, reservation_id, trace_id, provider, model, input_tokens, output_tokens,
                    total_tokens, cost_usd, latency_ms, stop_reason, source, metadata_json, created_at
                ) VALUES ($1,$2,$3,'openai-codex','gpt-small-bench',$4,$5,$6,$7,$8,'stop','reservation.finalize',$9,$10)
                "#
            ),
            &[
                &usage_id,
                &reservation_id,
                &trace_id,
                &input_tokens,
                &output_tokens,
                &total_tokens,
                &cost,
                &latency_ms,
                &metadata_json,
                &now,
            ],
        )
        .await?;
    client.batch_execute("COMMIT").await?;
    Ok(())
}

fn postgres_hot_authorize_like_single_statement_sql(schema: &str) -> String {
    format!(
        r#"
        WITH spend AS (
            SELECT COALESCE(SUM(amount_usd), 0) AS amount_usd
            FROM "{schema}".rolling_spend_buckets
            WHERE rule_id = 'bench-project'
              AND limit_id = 'bench-rolling-cap'
              AND scope_key = 'project:noether'
              AND bucket_start >= $8
              AND bucket_start <= $8
        ),
        inserted_decision AS (
            INSERT INTO "{schema}".decisions (
                decision_id, trace_id, request_id, project, provider, model,
                estimated_cost_usd, outcome, action, explanations_json, metadata_json, entities_json,
                selected_budget_id, matched_entity, limit_hits_json, app_run_key, created_at
            ) VALUES ($1,$2,$3,'noether','openai-codex','gpt-small-bench',$4,'allow','allow',
                '[{{"rule_id":"bench-project","reason":"selected fallback budget","severity":"info"}}]',
                $5,$6,'bench-project','project:noether','[]',$7,$8)
            RETURNING decision_id
        ),
        inserted_reservation AS (
            INSERT INTO "{schema}".reservations (
                id, decision_id, amount_usd, estimated_amount_usd, actual_amount_usd, currency, status,
                created_at, expires_at, finalized_at, budget_rule_ids_json, limit_window_spends_json, allocation_spends_json
            ) VALUES ($9,$1,$4,$4,NULL,'USD','active',$8,$10,NULL,'["bench-project"]',
                '[{{"rule_id":"bench-project","limit_id":"bench-rolling-cap","scope_key":"project:noether"}}]',
                '[]')
            RETURNING id
        ),
        inserted_scope AS (
            INSERT INTO "{schema}".reservation_limit_scopes (
                reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at
            ) VALUES ($9,'bench-project','bench-rolling-cap','project:noether',$4,$8)
        ),
        upserted_bucket AS (
            INSERT INTO "{schema}".rolling_spend_buckets (
                rule_id, limit_id, scope_key, bucket_start, amount_usd
            ) VALUES ('bench-project','bench-rolling-cap','project:noether',$8,$4)
            ON CONFLICT(rule_id, limit_id, scope_key, bucket_start) DO UPDATE SET
                amount_usd = "{schema}".rolling_spend_buckets.amount_usd + EXCLUDED.amount_usd
        )
        SELECT amount_usd FROM spend
        "#
    )
}

fn postgres_finalize_like_single_statement_sql(schema: &str) -> String {
    format!(
        r#"
        WITH updated_reservation AS (
            UPDATE "{schema}".reservations
            SET amount_usd = $2, actual_amount_usd = $2, status = 'finalized', finalized_at = $3
            WHERE id = $1
            RETURNING id, decision_id
        ),
        decision_trace AS (
            SELECT d.trace_id
            FROM updated_reservation r
            JOIN "{schema}".decisions d ON d.decision_id = r.decision_id
        ),
        inserted_usage AS (
            INSERT INTO "{schema}".usage_observations (
                id, reservation_id, trace_id, provider, model, input_tokens, output_tokens,
                total_tokens, cost_usd, latency_ms, stop_reason, source, metadata_json, created_at
            )
            SELECT $4, $1, trace_id, 'openai-codex', 'gpt-small-bench', $5, $6, $7, $2, $8,
                   'stop', 'reservation.finalize', $9, $3
            FROM decision_trace
            RETURNING id
        )
        SELECT id FROM inserted_usage
        "#
    )
}

fn authorize_request(index: usize) -> AuthorizeRequest {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "trace_id".to_owned(),
        Value::String(format!("bench-sqlite-trace-{index}")),
    );
    metadata.insert(
        "request_id".to_owned(),
        Value::String(format!("bench-sqlite-request-{index}")),
    );
    metadata.insert(
        "agent_run_id".to_owned(),
        Value::String(format!("bench-sqlite-run-{index}")),
    );
    AuthorizeRequest {
        budget_id: None,
        entities: vec![
            "project:noether".to_owned(),
            format!("user:bench-{}", index % 12),
        ],
        subject: Some(format!("user:bench-{}", index % 12)),
        project: Some("noether".to_owned()),
        provider: Some("openai-codex".to_owned()),
        model: Some("gpt-small-bench".to_owned()),
        estimated_tokens: Some(600 + (index % 2_000) as u64),
        estimated_cost_usd: Some(0.001 + ((index % 200) as f64 / 100_000.0)),
        metadata,
    }
}

fn finalize_payload(index: usize) -> FinalizeReservation {
    let input_tokens = 500 + (index % 1000);
    let output_tokens = 100 + (index % 300);
    serde_json::from_value(serde_json::json!({
        "outcome": "success",
        "actual_cost_usd": 0.001 + ((index % 200) as f64 / 100000.0),
        "usage": {
            "provider": "openai-codex",
            "model": "gpt-small-bench",
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
            "cost_usd": 0.001 + ((index % 200) as f64 / 100000.0),
            "latency_ms": 900 + (index % 300)
        },
        "metadata": {
            "trace_id": format!("bench-sqlite-trace-{index}"),
            "request_id": format!("bench-sqlite-request-{index}"),
            "agent_run_id": format!("bench-sqlite-run-{index}")
        }
    }))
    .expect("static finalize payload is valid")
}

fn print_summary(name: &str, samples: &[Duration]) {
    let mut values = samples
        .iter()
        .map(|sample| sample.as_secs_f64() * 1000.0)
        .collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    let min = values.first().copied().unwrap_or_default();
    let max = values.last().copied().unwrap_or_default();
    let p50 = percentile(&values, 0.50);
    let p95 = percentile(&values, 0.95);
    let p99 = percentile(&values, 0.99);
    let avg = if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    };
    println!(
        "{name},{},{min:.3},{p50:.3},{p95:.3},{p99:.3},{max:.3},{avg:.3}",
        values.len()
    );
}

fn percentile(values: &[f64], percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = ((values.len() - 1) as f64 * percentile).round() as usize;
    values[index]
}

fn redact_database_url(database_url: &str) -> String {
    let Some(at) = database_url.rfind('@') else {
        return database_url.to_owned();
    };
    let Some(scheme) = database_url.find("://") else {
        return database_url.to_owned();
    };
    format!("{}://<redacted>@{}", &database_url[..scheme], &database_url[at + 1..])
}

fn remove_sqlite_files(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}
