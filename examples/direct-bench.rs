//! Direct DB-layer bench. Measures one authorize+finalize cycle without the
//! axum HTTP harness, isolating DB cost from the ~20ms overhead that dominates
//! noet-bench numbers.
//!
//! Default: SQLite via the integrated `BudgetLedger::try_authorize_at` path
//! (sub-millisecond, no spawn_blocking, no snapshot/dispatch).
//!
//! With `--via-backend`: runs through `Backend::persist_authorize_writes`
//! (the same path the HTTP handler uses minus axum). Works for both
//! `--db-url sqlite://...` and `--db-url postgres://...`. Numbers include
//! the snapshot/dispatch overhead that the integrated SQLite path skips.
//!
//! Usage:
//!   cargo run --release --example direct-bench
//!   cargo run --release --example direct-bench -- --via-backend
//!   cargo run --release --example direct-bench -- --via-backend --db-url postgres://noether:test@localhost:5433/noether
//!   cargo run --release --example direct-bench -- --iterations 500

use std::error::Error;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use chrono::Utc;
use noether::backend::{Backend, path_to_sqlite_url, sqlite_url_to_path, url_scheme};
use noether::contract::{AuthorizeRequest, FinalizeReservation, UsageObservation};
use noether::ledger::{
    BudgetLedger, ConnMutex, HotState, RoutingPersistenceFields, finalize_hot,
    try_authorize_at_hot,
};
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
    })).unwrap()
}

fn fin() -> FinalizeReservation {
    FinalizeReservation {
        reservation_id: None,
        outcome: Default::default(),
        usage: Some(UsageObservation {
            provider: Some("openai-codex".into()),
            model: Some("gpt-small-bench".into()),
            input_tokens: Some(80),
            output_tokens: Some(20),
            total_tokens: Some(100),
            cost_usd: Some(0.00009),
            latency_ms: Some(50),
            stop_reason: None,
        }),
        actual_cost_usd: Some(0.00009),
        metadata: Default::default(),
    }
}

fn report(label: &str, mut samples: Vec<u128>) {
    samples.sort();
    let pct = |p: f64| samples[((samples.len() as f64 - 1.0) * p).round() as usize];
    let avg = samples.iter().sum::<u128>() / samples.len() as u128;
    println!(
        "{label:35} n={:>4}  min={:>5}us p50={:>5}us p95={:>5}us p99={:>5}us max={:>5}us avg={:>5}us",
        samples.len(), samples[0], pct(0.5), pct(0.95), pct(0.99), samples[samples.len()-1], avg,
    );
}

fn integrated_sqlite(iterations: usize) -> Result<(), Box<dyn Error>> {
    let policy = parse_policy_bytes(BENCH_POLICY.as_bytes())?;
    let db = std::env::temp_dir().join(format!("noether-direct-int-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let mut ledger = BudgetLedger::open_sqlite(&db)?;
    for i in 0..50 { ledger.try_authorize_at(Some(&policy), &req(i), Utc::now())?; }
    let mut samples = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let r = req(1_000_000 + i);
        let now = Utc::now();
        let t = Instant::now();
        ledger.try_authorize_at(Some(&policy), &r, now)?;
        samples.push(t.elapsed().as_micros());
    }
    println!("== integrated SQLite (BudgetLedger::try_authorize_at, no finalize) ==");
    report("authorize", samples);
    Ok(())
}

async fn via_backend(db_url: String, iterations: usize) -> Result<(), Box<dyn Error>> {
    let policy = parse_policy_bytes(BENCH_POLICY.as_bytes())?;

    let (backend, hot): (Arc<Backend>, Arc<Mutex<HotState>>) = match url_scheme(&db_url) {
        Some("sqlite") => {
            let path = sqlite_url_to_path(&db_url).ok_or("bad sqlite url")?;
            if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
            let _ = std::fs::remove_file(&path);
            let ledger = BudgetLedger::open_sqlite(&path)?;
            let hot_state = ledger.hot_state();
            drop(ledger);
            let conn: Arc<ConnMutex> = Arc::new(Mutex::new(Some(rusqlite::Connection::open(&path)?)));
            (Arc::new(Backend::sqlite_from_url(db_url.clone(), conn)), Arc::new(Mutex::new(hot_state)))
        }
        Some("postgres") | Some("postgresql") => {
            let backend = Backend::postgres_from_url(db_url.clone())?;
            if let Backend::Postgres(pg) = &backend { pg.init_schema().await?; }
            (Arc::new(backend), Arc::new(Mutex::new(HotState::default())))
        }
        other => return Err(format!("unsupported scheme: {other:?}").into()),
    };

    // Warmup
    for i in 0..30 { run_once(&backend, &hot, &policy, i).await?; }

    let mut auth = Vec::with_capacity(iterations);
    let mut fin_samples = Vec::with_capacity(iterations);
    let mut combined = Vec::with_capacity(iterations);

    for i in 0..iterations {
        let request = req(1_000_000 + i);
        let now = Utc::now();
        let c0 = Instant::now();

        let a0 = Instant::now();
        let (decision, snap) = { let mut g = hot.lock().unwrap(); try_authorize_at_hot(&mut g, Some(&policy), &request, now)? };
        if let Some(snap) = snap {
            let sel = snap.selected_budget_id.clone();
            let me = snap.matched_entity().map(|s| s.to_string());
            let routing = RoutingPersistenceFields { selected_budget_id: sel, matched_entity: me, ..Default::default() };
            backend.persist_authorize_writes(snap, request.clone(), decision.clone(), routing).await?;
        }
        auth.push(a0.elapsed().as_micros());

        let rid = decision.reservation.as_ref().map(|r| r.id.clone()).ok_or("no reservation")?;
        let payload = fin();
        let f0 = Instant::now();
        let (reservation, lw) = { let mut g = hot.lock().unwrap(); finalize_hot(&mut g, &rid, &payload)? };
        backend.persist_finalize_writes(reservation, payload, lw).await?;
        fin_samples.push(f0.elapsed().as_micros());

        combined.push(c0.elapsed().as_micros());
    }

    println!("== via Backend dispatch: {} ==", db_url);
    report("authorize (hot + persist)", auth);
    report("finalize  (hot + persist)", fin_samples);
    report("combined  (auth + finalize)", combined);
    Ok(())
}

async fn run_once(backend: &Arc<Backend>, hot: &Arc<Mutex<HotState>>, policy: &noether::policy::PolicyFile, i: usize) -> Result<(), Box<dyn Error>> {
    let request = req(i);
    let now = Utc::now();
    let (decision, snap) = { let mut g = hot.lock().unwrap(); try_authorize_at_hot(&mut g, Some(policy), &request, now)? };
    if let Some(snap) = snap {
        let sel = snap.selected_budget_id.clone();
        let me = snap.matched_entity().map(|s| s.to_string());
        let routing = RoutingPersistenceFields { selected_budget_id: sel, matched_entity: me, ..Default::default() };
        backend.persist_authorize_writes(snap, request.clone(), decision.clone(), routing).await?;
    }
    if let Some(rid) = decision.reservation.as_ref().map(|r| r.id.clone()) {
        let payload = fin();
        let (reservation, lw) = { let mut g = hot.lock().unwrap(); finalize_hot(&mut g, &rid, &payload)? };
        backend.persist_finalize_writes(reservation, payload, lw).await?;
    }
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut via = false;
    let mut iterations: usize = 1000;
    let mut db_url: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--via-backend" => via = true,
            "--iterations" => iterations = args.next().ok_or("--iterations N")?.parse()?,
            "--db-url" => db_url = Some(args.next().ok_or("--db-url URL")?),
            "-h" | "--help" => {
                println!("direct-bench [--via-backend] [--db-url <url>] [--iterations N]");
                return Ok(());
            }
            other => return Err(format!("unknown arg: {other}").into()),
        }
    }
    if via {
        let url = db_url.unwrap_or_else(|| {
            let p = std::env::temp_dir().join(format!("noether-direct-be-{}.sqlite", std::process::id()));
            path_to_sqlite_url(&p)
        });
        via_backend(url, iterations).await
    } else {
        integrated_sqlite(iterations)
    }
}
