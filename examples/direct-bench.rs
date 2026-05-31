//! Direct DB-layer benchmark for current ledger APIs.
//!
//! This intentionally does not use the PR #49 HotState/backend-dispatch API.
//! It measures the current durable ledger entry points without the Axum router:
//!
//!   cargo run --release --example direct-bench -- --backend sqlite --iterations 500
//!   cargo run --release --example direct-bench -- --backend postgres --db-url "$NOET_TEST_POSTGRES_URL"

use std::collections::BTreeMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use noether::contract::{AuthorizeRequest, FinalizeReservation, UsageObservation};
use noether::ledger::{AsyncPostgresLedger, AsyncPostgresLedgerOptions, BudgetLedger};
use noether::policy::parse_policy_bytes;
use serde_json::Value;

const BENCH_POLICY: &str = r#"
version: 0
budgets:
  - id: bench-project
    limits:
      spend:
        - id: bench-budget-cap
          window: 1d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1000000
          warn_at_fraction: 0.8
          action: block
    match:
      project: noether
policies:
  - id: require-project
    action: block
    reason: project is required for budget attribution
    when:
      missing: project
"#;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = BenchConfig::from_args()?;
    match config.backend.as_str() {
        "sqlite" => bench_sqlite(&config)?,
        "postgres" => bench_postgres(&config).await?,
        other => {
            return Err(
                format!("unsupported backend {other:?}; expected sqlite or postgres").into(),
            );
        }
    }
    Ok(())
}

struct BenchConfig {
    backend: String,
    iterations: usize,
    db_path: PathBuf,
    db_url: Option<String>,
    postgres_profile: String,
}

impl BenchConfig {
    fn from_args() -> Result<Self, Box<dyn Error>> {
        let mut backend = "sqlite".to_owned();
        let mut iterations = 1_000;
        let mut db_path = std::env::temp_dir().join(format!(
            "noether-direct-bench-{}.sqlite",
            std::process::id()
        ));
        let mut db_url = None;
        let mut postgres_profile = "strict".to_owned();

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--backend" => backend = args.next().ok_or("--backend requires sqlite|postgres")?,
                "--iterations" => {
                    iterations = args
                        .next()
                        .ok_or("--iterations requires a value")?
                        .parse()?;
                }
                "--db-path" => {
                    db_path = PathBuf::from(args.next().ok_or("--db-path requires a value")?);
                }
                "--db-url" => db_url = Some(args.next().ok_or("--db-url requires a value")?),
                "--postgres-profile" => {
                    postgres_profile = args
                        .next()
                        .ok_or("--postgres-profile requires strict|performance")?;
                }
                "-h" | "--help" => {
                    println!(
                        "direct-bench --backend sqlite|postgres [--iterations N] [--db-path PATH] [--db-url URL] [--postgres-profile strict|performance]"
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument {other:?}").into()),
            }
        }

        if iterations == 0 {
            return Err("--iterations must be greater than zero".into());
        }
        Ok(Self {
            backend,
            iterations,
            db_path,
            db_url,
            postgres_profile,
        })
    }
}

fn bench_sqlite(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    remove_sqlite_files(&config.db_path);
    let policy = parse_policy_bytes(BENCH_POLICY.as_bytes())?;
    let mut ledger = BudgetLedger::open_sqlite(&config.db_path)?;

    for index in 0..50 {
        run_sqlite_cycle(&mut ledger, &policy, index)?;
    }

    let mut authorize = Vec::with_capacity(config.iterations);
    let mut finalize = Vec::with_capacity(config.iterations);
    let mut combined = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let request = authorize_request(1_000_000 + index);
        let cycle_started = Instant::now();
        let authorize_started = Instant::now();
        let decision = ledger.try_authorize_at(Some(&policy), &request, Utc::now())?;
        authorize.push(authorize_started.elapsed());

        let reservation_id = decision
            .reservation
            .as_ref()
            .ok_or("authorize did not create a reservation")?
            .id
            .clone();
        let finalize_started = Instant::now();
        ledger.finalize(&reservation_id, &finalize_payload(1_000_000 + index))?;
        finalize.push(finalize_started.elapsed());
        combined.push(cycle_started.elapsed());
    }

    println!(
        "direct-bench backend=sqlite db_path={} iterations={}",
        config.db_path.display(),
        config.iterations
    );
    print_summary("authorize", &authorize);
    print_summary("finalize", &finalize);
    print_summary("combined", &combined);
    remove_sqlite_files(&config.db_path);
    Ok(())
}

async fn bench_postgres(config: &BenchConfig) -> Result<(), Box<dyn Error>> {
    let database_url = config
        .db_url
        .as_deref()
        .ok_or("--db-url is required for --backend postgres")?;
    let policy = Arc::new(parse_policy_bytes(BENCH_POLICY.as_bytes())?);
    let ledger = AsyncPostgresLedger::connect_with_options(
        database_url,
        AsyncPostgresLedgerOptions::from_profile(&config.postgres_profile)?,
    )
    .await?;

    for index in 0..50 {
        run_postgres_cycle(&ledger, policy.clone(), index).await?;
    }

    let mut authorize = Vec::with_capacity(config.iterations);
    let mut finalize = Vec::with_capacity(config.iterations);
    let mut combined = Vec::with_capacity(config.iterations);
    for index in 0..config.iterations {
        let request = authorize_request(1_000_000 + index);
        let cycle_started = Instant::now();
        let authorize_started = Instant::now();
        let decision = ledger
            .try_authorize_at(Some(policy.clone()), request, Utc::now())
            .await?;
        authorize.push(authorize_started.elapsed());

        let reservation_id = decision
            .reservation
            .as_ref()
            .ok_or("authorize did not create a reservation")?
            .id
            .clone();
        let finalize_started = Instant::now();
        ledger
            .finalize(reservation_id, finalize_payload(1_000_000 + index))
            .await?;
        finalize.push(finalize_started.elapsed());
        combined.push(cycle_started.elapsed());
    }

    println!(
        "direct-bench backend=postgres profile={} iterations={}",
        config.postgres_profile, config.iterations
    );
    print_summary("authorize", &authorize);
    print_summary("finalize", &finalize);
    print_summary("combined", &combined);
    Ok(())
}

fn run_sqlite_cycle(
    ledger: &mut BudgetLedger,
    policy: &noether::policy::PolicyFile,
    index: usize,
) -> Result<(), Box<dyn Error>> {
    let decision = ledger.try_authorize_at(Some(policy), &authorize_request(index), Utc::now())?;
    if let Some(reservation) = decision.reservation {
        ledger.finalize(&reservation.id, &finalize_payload(index))?;
    }
    Ok(())
}

async fn run_postgres_cycle(
    ledger: &AsyncPostgresLedger,
    policy: Arc<noether::policy::PolicyFile>,
    index: usize,
) -> Result<(), Box<dyn Error>> {
    let decision = ledger
        .try_authorize_at(Some(policy), authorize_request(index), Utc::now())
        .await?;
    if let Some(reservation) = decision.reservation {
        ledger
            .finalize(reservation.id, finalize_payload(index))
            .await?;
    }
    Ok(())
}

fn authorize_request(index: usize) -> AuthorizeRequest {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "trace_id".to_owned(),
        Value::String(format!("direct-bench-trace-{index}")),
    );
    metadata.insert(
        "request_id".to_owned(),
        Value::String(format!("direct-bench-request-{index}")),
    );
    metadata.insert(
        "agent_run_id".to_owned(),
        Value::String(format!("direct-bench-run-{index}")),
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
        model: Some(if index.is_multiple_of(7) {
            "gpt-large-bench".to_owned()
        } else {
            "gpt-small-bench".to_owned()
        }),
        estimated_tokens: Some(600 + (index % 2_000) as u64),
        estimated_cost_usd: Some(0.001 + ((index % 200) as f64 / 100_000.0)),
        metadata,
    }
}

fn finalize_payload(index: usize) -> FinalizeReservation {
    let input_tokens = 500 + (index % 1_000) as u64;
    let output_tokens = 100 + (index % 300) as u64;
    FinalizeReservation {
        reservation_id: None,
        outcome: Default::default(),
        usage: Some(UsageObservation {
            provider: Some("openai-codex".to_owned()),
            model: Some(if index.is_multiple_of(7) {
                "gpt-large-bench".to_owned()
            } else {
                "gpt-small-bench".to_owned()
            }),
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
            total_tokens: Some(input_tokens + output_tokens),
            cost_usd: Some(0.001 + ((index % 200) as f64 / 100_000.0)),
            latency_ms: Some(900 + (index % 300) as u64),
            stop_reason: None,
        }),
        actual_cost_usd: Some(0.001 + ((index % 200) as f64 / 100_000.0)),
        metadata: BTreeMap::new(),
    }
}

fn print_summary(name: &str, samples: &[Duration]) {
    let mut values = samples
        .iter()
        .map(|duration| duration.as_secs_f64() * 1_000_000.0)
        .collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    let avg = values.iter().sum::<f64>() / values.len() as f64;
    println!(
        "{name},count={},min_us={:.3},p50_us={:.3},p95_us={:.3},p99_us={:.3},max_us={:.3},avg_us={:.3}",
        values.len(),
        values[0],
        percentile(&values, 0.50),
        percentile(&values, 0.95),
        percentile(&values, 0.99),
        values[values.len() - 1],
        avg
    );
}

fn percentile(values: &[f64], percentile: f64) -> f64 {
    let index = ((values.len() - 1) as f64 * percentile).round() as usize;
    values[index]
}

fn remove_sqlite_files(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
}
