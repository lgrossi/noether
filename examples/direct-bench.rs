//! Direct DB-layer bench: calls `BudgetLedger::try_authorize_at` 1000 times,
//! reports min/p50/p95/p99/max in microseconds.
//!
//! This isolates the SQLite hotpath cost (sub-millisecond) from the noet-bench
//! HTTP harness overhead (axum oneshot + JSON + spawn_blocking + body collect)
//! which dominates the per-request numbers reported by `noet-bench`.
//!
//! PG cannot be benched at the same layer without promoting
//! `Backend::persist_authorize_writes` from `pub(crate)` to `pub`. Apples-to-
//! apples PG comparison should go through `noet-bench --db-url postgres://...`,
//! accepting that ~20ms of harness overhead is included in both numbers.
//!
//! Usage: cargo run --release --example direct-bench

use std::time::Instant;

use chrono::Utc;
use noether::contract::AuthorizeRequest;
use noether::ledger::BudgetLedger;
use noether::policy::parse_policy_bytes;

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

fn req(i: usize) -> AuthorizeRequest {
    serde_json::from_value(serde_json::json!({
        "subject": "user:bench",
        "project": "noether",
        "provider": "openai-codex",
        "model": "gpt-small-bench",
        "estimated_tokens": 100,
        "estimated_cost_usd": 0.0001,
        "trace_id": format!("trace-{i}"),
        "request_id": format!("req-{i}"),
        "metadata": { "agent_run_id": format!("run-{i}") },
    }))
    .unwrap()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let policy = parse_policy_bytes(BENCH_POLICY.as_bytes())?;
    let db = std::env::temp_dir().join("noether-direct-bench.sqlite");
    let _ = std::fs::remove_file(&db);
    let mut ledger = BudgetLedger::open_sqlite(&db)?;

    for i in 0..50 {
        ledger.try_authorize_at(Some(&policy), &req(i), Utc::now())?;
    }

    let n = 1000;
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        let r = req(1_000_000 + i);
        let now = Utc::now();
        let t = Instant::now();
        ledger.try_authorize_at(Some(&policy), &r, now)?;
        samples.push(t.elapsed().as_micros() as u64);
    }
    samples.sort();
    let pct = |p: f64| samples[((samples.len() as f64 - 1.0) * p).round() as usize];
    println!("BudgetLedger::try_authorize_at (SQLite, direct), n={n}");
    println!(
        "  min={}us  p50={}us  p95={}us  p99={}us  max={}us  avg={}us",
        samples[0],
        pct(0.5),
        pct(0.95),
        pct(0.99),
        samples[samples.len() - 1],
        samples.iter().sum::<u64>() / samples.len() as u64
    );
    Ok(())
}
