use std::collections::BTreeMap;
use std::error::Error;
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use chrono::{Duration as ChronoDuration, Utc};
use noether::contract::{AuthorizeRequest, DecisionMode, FinalizeReservation, TraceEvent};
use noether::ledger::BudgetLedger;
use noether::policy::parse_policy_bytes;
use noether::server::{AppState, build_router};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tower::ServiceExt;

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

const BENCH_REPLAY_PROPOSAL: &str = r#"
version: 0
budgets:
  - id: bench-project
    models:
      allow: [openai-codex:gpt-small-bench]
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = BenchConfig::from_env()?;
    if let Some(base_url) = config.base_url.as_deref() {
        return run_live_bench(base_url, config.iterations).await;
    }
    let policy = parse_policy_bytes(BENCH_POLICY.as_bytes())?;
    let db_path = std::env::temp_dir().join(format!(
        "noether-bench-{}-{}.sqlite",
        std::process::id(),
        config.rows
    ));
    remove_sqlite_files(&db_path);
    let fixture_dir = std::env::temp_dir().join(format!(
        "noether-bench-fixtures-{}-{}",
        std::process::id(),
        config.rows
    ));
    let proposal_path = fixture_dir.join("policy.proposed.yaml");
    let _ = std::fs::remove_dir_all(&fixture_dir);
    std::fs::create_dir_all(&fixture_dir)?;

    let mut ledger = BudgetLedger::open_sqlite(&db_path)?;
    match config.seed_mode {
        SeedMode::Product => seed_ledger(&mut ledger, &policy, config.rows)?,
        SeedMode::BulkCompany => {
            drop(ledger);
            seed_company_ledger_bulk(&db_path, config.rows, config.events_per_decision)?;
            ledger = BudgetLedger::open_sqlite(&db_path)?;
        }
    }

    let mut state = AppState::new(
        fixture_dir.clone(),
        None,
        Some(policy),
        DecisionMode::Enforce,
    );
    state.policy_proposal_path = proposal_path.clone();
    *state.ledger.lock().await = ledger;
    let app = build_router(state.clone());

    println!(
        "noet-bench rows={} iterations={} db={}",
        config.rows,
        config.iterations,
        db_path.display()
    );
    println!("name,count,min_ms,p50_ms,p95_ms,max_ms,avg_ms");

    bench_get(
        "GET /v1/app/policy",
        app.clone(),
        "/v1/app/policy",
        config.iterations,
    )
    .await?;
    bench_get(
        "GET /v1/app/runs",
        app.clone(),
        "/v1/app/runs?limit=80",
        config.iterations,
    )
    .await?;
    bench_get(
        "GET /v1/app/replay",
        app.clone(),
        "/v1/app/replay",
        config.iterations,
    )
    .await?;
    if !config.skip_draft_replay {
        std::fs::write(&proposal_path, BENCH_REPLAY_PROPOSAL)?;
        bench_get(
            "GET /v1/app/replay (draft simulation)",
            app.clone(),
            "/v1/app/replay",
            config.iterations,
        )
        .await?;
    }
    bench_authorize(app.clone(), config.iterations).await?;
    bench_finalize(app.clone(), config.iterations).await?;
    bench_event(app, config.iterations).await?;

    remove_sqlite_files(&db_path);
    let _ = std::fs::remove_dir_all(&fixture_dir);
    Ok(())
}

#[derive(Debug)]
struct BenchConfig {
    rows: usize,
    iterations: usize,
    base_url: Option<String>,
    seed_mode: SeedMode,
    events_per_decision: usize,
    skip_draft_replay: bool,
}

#[derive(Clone, Copy, Debug)]
enum SeedMode {
    Product,
    BulkCompany,
}

impl BenchConfig {
    fn from_env() -> Result<Self, Box<dyn Error>> {
        let mut rows = 1_000;
        let mut iterations = 30;
        let mut base_url = None;
        let mut seed_mode = SeedMode::Product;
        let mut events_per_decision = 1;
        let mut skip_draft_replay = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--rows" => {
                    rows = args
                        .next()
                        .ok_or("--rows requires a value")?
                        .parse::<usize>()?;
                }
                "--iterations" => {
                    iterations = args
                        .next()
                        .ok_or("--iterations requires a value")?
                        .parse::<usize>()?;
                }
                "--base-url" => {
                    base_url = Some(args.next().ok_or("--base-url requires a value")?);
                }
                "--seed-mode" => {
                    seed_mode = match args.next().ok_or("--seed-mode requires a value")?.as_str() {
                        "product" => SeedMode::Product,
                        "bulk-company" => SeedMode::BulkCompany,
                        value => return Err(format!("unknown seed mode: {value}").into()),
                    };
                }
                "--events-per-decision" => {
                    events_per_decision = args
                        .next()
                        .ok_or("--events-per-decision requires a value")?
                        .parse::<usize>()?;
                }
                "--skip-draft-replay" => {
                    skip_draft_replay = true;
                }
                "--help" | "-h" => {
                    println!(
                        "Usage: cargo run --release --bin noet-bench -- [--rows N] [--iterations N] [--seed-mode product|bulk-company] [--events-per-decision N] [--skip-draft-replay] [--base-url URL]"
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other}").into()),
            }
        }
        Ok(Self {
            rows,
            iterations,
            base_url,
            seed_mode,
            events_per_decision,
            skip_draft_replay,
        })
    }
}

fn seed_ledger(
    ledger: &mut BudgetLedger,
    policy: &noether::policy::PolicyFile,
    rows: usize,
) -> Result<(), Box<dyn Error>> {
    let base_time = Utc::now() - ChronoDuration::seconds(rows as i64);
    for index in 0..rows {
        let trace_id = format!("bench-trace-{index}");
        let request_id = format!("bench-request-{index}");
        let agent_run_id = format!("bench-run-{index}");
        let request = authorize_request(
            index,
            Some(&trace_id),
            Some(&request_id),
            Some(&agent_run_id),
        );
        let created_at = base_time + ChronoDuration::seconds(index as i64);
        let decision = ledger.try_authorize_at(Some(policy), &request, created_at)?;
        if let Some(reservation) = decision.reservation {
            let finalize = finalize_payload(index, &trace_id, &request_id, &agent_run_id);
            ledger.finalize(&reservation.id, &finalize)?;
        }
        ledger.record_event(TraceEvent {
            id: None,
            trace_id: Some(trace_id),
            occurred_at: Some(created_at),
            kind: "tool.observed".to_owned(),
            payload: json!({
                "name": "shell",
                "duration_ms": 10 + (index % 90),
                "success": index % 17 != 0,
                "metadata": { "agent_run_id": agent_run_id, "request_id": request_id }
            }),
        })?;
    }
    Ok(())
}

fn seed_company_ledger_bulk(
    db_path: &std::path::Path,
    rows: usize,
    events_per_decision: usize,
) -> Result<(), Box<dyn Error>> {
    let mut conn = Connection::open(db_path)?;
    conn.execute_batch(
        "
        PRAGMA synchronous = OFF;
        PRAGMA temp_store = MEMORY;
        PRAGMA locking_mode = EXCLUSIVE;
        ",
    )?;
    let tx = conn.transaction()?;
    let base_time = Utc::now() - ChronoDuration::seconds(rows as i64);
    {
        let mut insert_decision = tx.prepare(
            "
            INSERT INTO decisions (
                decision_id, trace_id, request_id, subject, project, provider, model,
                estimated_tokens, estimated_cost_usd, outcome, action, explanations_json,
                metadata_json, entities_json, selected_budget_id, matched_entity,
                selection_reason, model_check, routing_json, limit_hits_json, app_run_key, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
            )
            ",
        )?;
        let mut insert_reservation = tx.prepare(
            "
            INSERT INTO reservations (
                id, decision_id, amount_usd, estimated_amount_usd, actual_amount_usd, currency,
                status, created_at, expires_at, finalized_at, budget_rule_ids_json,
                limit_window_spends_json, allocation_spends_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, 'USD', 'finalized', ?6, ?7, ?8, ?9, ?10, '[]')
            ",
        )?;
        let mut insert_scope = tx.prepare(
            "
            INSERT INTO reservation_limit_scopes (
                reservation_id, rule_id, limit_id, scope_key, amount_usd, created_at
            ) VALUES (?1, 'bench-project', 'bench-budget-cap', ?2, ?3, ?4)
            ",
        )?;
        let mut insert_usage = tx.prepare(
            "
            INSERT INTO usage_observations (
                id, reservation_id, trace_id, provider, model, input_tokens, output_tokens,
                total_tokens, cost_usd, latency_ms, stop_reason, source, metadata_json, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'stop', 'bench', ?11, ?12)
            ",
        )?;
        let mut insert_event = tx.prepare(
            "
            INSERT INTO events (id, trace_id, kind, occurred_at, source, payload_json)
            VALUES (?1, ?2, ?3, ?4, 'bench', ?5)
            ",
        )?;

        for index in 0..rows {
            let run_index = index / 4;
            let trace_id = format!("bench-company-trace-{run_index}");
            let request_id = format!("bench-company-request-{index}");
            let agent_run_id = format!("bench-company-run-{run_index}");
            let decision_id = format!("bench-company-decision-{index}");
            let reservation_id = format!("bench-company-reservation-{index}");
            let subject = format!("user:bench-{}", index % 500);
            let model = if index % 7 == 0 {
                "gpt-large-bench"
            } else {
                "gpt-small-bench"
            };
            let created_at = base_time + ChronoDuration::seconds(index as i64);
            let created_at = created_at.to_rfc3339();
            let cost = 0.001 + ((index % 200) as f64 / 100_000.0);
            let input_tokens = 500 + (index % 1000) as i64;
            let output_tokens = 100 + (index % 300) as i64;
            let metadata = json!({
                "trace_id": trace_id,
                "request_id": request_id,
                "agent_run_id": agent_run_id
            });
            let entities = json!(["project:noether", subject]);
            let routing = json!({
                "selected_budget_id": "bench-project",
                "matched_entity": "project:noether",
                "selection_reason": "selected fallback budget for project:noether",
                "model_check": "allowed:bench-project"
            });
            let explanations = json!([{
                "rule_id": "bench-project",
                "reason": "selected fallback budget for project:noether",
                "severity": "info"
            }]);
            let app_run_key = format!("agent-run:{agent_run_id}");
            insert_decision.execute(params![
                decision_id,
                trace_id,
                request_id,
                subject,
                "noether",
                "openai-codex",
                model,
                input_tokens + output_tokens,
                cost,
                "allow",
                "allow",
                explanations.to_string(),
                metadata.to_string(),
                entities.to_string(),
                "bench-project",
                "project:noether",
                "selected fallback budget for project:noether",
                "allowed:bench-project",
                routing.to_string(),
                "[]",
                app_run_key,
                created_at
            ])?;
            let expires_at =
                (base_time + ChronoDuration::seconds(index as i64 + 3600)).to_rfc3339();
            let scope_json = json!([{
                "rule_id": "bench-project",
                "limit_id": "bench-budget-cap",
                "scope_key": "project:noether"
            }]);
            insert_reservation.execute(params![
                reservation_id,
                format!("bench-company-decision-{index}"),
                cost,
                cost,
                cost,
                created_at,
                expires_at,
                created_at,
                "[\"bench-project\"]",
                scope_json.to_string()
            ])?;
            insert_scope.execute(params![
                format!("bench-company-reservation-{index}"),
                "project:noether",
                cost,
                created_at
            ])?;
            let usage_metadata = json!({
                "trace_id": format!("bench-company-trace-{run_index}"),
                "request_id": format!("bench-company-request-{index}"),
                "agent_run_id": format!("bench-company-run-{run_index}")
            });
            insert_usage.execute(params![
                format!("bench-company-usage-{index}"),
                format!("bench-company-reservation-{index}"),
                format!("bench-company-trace-{run_index}"),
                "openai-codex",
                model,
                input_tokens,
                output_tokens,
                input_tokens + output_tokens,
                cost,
                900 + (index % 300) as i64,
                usage_metadata.to_string(),
                created_at
            ])?;
            for event_index in 0..events_per_decision {
                insert_event.execute(params![
                    format!("bench-company-event-{index}-{event_index}"),
                    format!("bench-company-trace-{run_index}"),
                    if event_index == 0 {
                        "tool.observed"
                    } else {
                        "usage.observed"
                    },
                    created_at,
                    json!({
                        "name": "bench",
                        "success": index % 17 != 0,
                        "metadata": usage_metadata
                    })
                    .to_string()
                ])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

fn authorize_request(
    index: usize,
    trace_id: Option<&str>,
    request_id: Option<&str>,
    agent_run_id: Option<&str>,
) -> AuthorizeRequest {
    let mut metadata = BTreeMap::new();
    if let Some(trace_id) = trace_id {
        metadata.insert("trace_id".to_owned(), Value::String(trace_id.to_owned()));
    }
    if let Some(request_id) = request_id {
        metadata.insert(
            "request_id".to_owned(),
            Value::String(request_id.to_owned()),
        );
    }
    if let Some(agent_run_id) = agent_run_id {
        metadata.insert(
            "agent_run_id".to_owned(),
            Value::String(agent_run_id.to_owned()),
        );
    }
    AuthorizeRequest {
        budget_id: None,
        entities: vec![
            "project:noether".to_owned(),
            format!("user:bench-{}", index % 12),
        ],
        subject: Some(format!("user:bench-{}", index % 12)),
        project: (index >= 1_000_000 || index % 19 != 0).then_some("noether".to_owned()),
        provider: Some("openai-codex".to_owned()),
        model: Some(if index % 7 == 0 {
            "gpt-large-bench".to_owned()
        } else {
            "gpt-small-bench".to_owned()
        }),
        estimated_tokens: Some(600 + (index % 2_000) as u64),
        estimated_cost_usd: Some(0.001 + ((index % 200) as f64 / 100_000.0)),
        metadata,
    }
}

fn finalize_payload(
    index: usize,
    trace_id: &str,
    request_id: &str,
    agent_run_id: &str,
) -> FinalizeReservation {
    let input_tokens = 500 + (index % 1000);
    let output_tokens = 100 + (index % 300);
    serde_json::from_value(json!({
        "outcome": "success",
        "actual_cost_usd": 0.001 + ((index % 200) as f64 / 100000.0),
        "usage": {
            "provider": "openai-codex",
            "model": if index % 7 == 0 { "gpt-large-bench" } else { "gpt-small-bench" },
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
            "cost_usd": 0.001 + ((index % 200) as f64 / 100000.0),
            "latency_ms": 900 + (index % 300)
        },
        "metadata": {
            "trace_id": trace_id,
            "request_id": request_id,
            "agent_run_id": agent_run_id
        }
    }))
    .expect("static finalize payload is valid")
}

async fn bench_get(
    name: &str,
    app: axum::Router,
    uri: &str,
    iterations: usize,
) -> Result<(), Box<dyn Error>> {
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        samples.push(request_duration(app.clone(), Method::GET, uri, None).await?);
    }
    print_summary(name, &samples);
    Ok(())
}

async fn bench_authorize(app: axum::Router, iterations: usize) -> Result<(), Box<dyn Error>> {
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let body = serde_json::to_vec(&authorize_request(
            1_000_000 + index,
            Some(&format!("bench-hot-trace-{index}")),
            Some(&format!("bench-hot-request-{index}")),
            Some(&format!("bench-hot-run-{index}")),
        ))?;
        samples
            .push(request_duration(app.clone(), Method::POST, "/v1/authorize", Some(body)).await?);
    }
    print_summary("POST /v1/authorize", &samples);
    Ok(())
}

async fn bench_finalize(app: axum::Router, iterations: usize) -> Result<(), Box<dyn Error>> {
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let request = authorize_request(
            2_000_000 + index,
            Some(&format!("bench-finalize-trace-{index}")),
            Some(&format!("bench-finalize-request-{index}")),
            Some(&format!("bench-finalize-run-{index}")),
        );
        let body = serde_json::to_vec(&request)?;
        let response = request_json(app.clone(), Method::POST, "/v1/authorize", Some(body)).await?;
        let reservation_id = response
            .get("reservation")
            .and_then(|reservation| reservation.get("id"))
            .and_then(Value::as_str)
            .ok_or("authorize response did not include reservation id")?;
        let finalize = finalize_payload(
            2_000_000 + index,
            &format!("bench-finalize-trace-{index}"),
            &format!("bench-finalize-request-{index}"),
            &format!("bench-finalize-run-{index}"),
        );
        let body = serde_json::to_vec(&finalize)?;
        samples.push(
            request_duration(
                app.clone(),
                Method::POST,
                &format!("/v1/reservations/{reservation_id}/finalize"),
                Some(body),
            )
            .await?,
        );
    }
    print_summary("POST /v1/reservations/{id}/finalize", &samples);
    Ok(())
}

async fn bench_event(app: axum::Router, iterations: usize) -> Result<(), Box<dyn Error>> {
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let body = serde_json::to_vec(&TraceEvent {
            id: None,
            trace_id: Some(format!("bench-event-trace-{index}")),
            occurred_at: None,
            kind: "tool.observed".to_owned(),
            payload: json!({ "name": "shell", "success": true }),
        })?;
        samples.push(request_duration(app.clone(), Method::POST, "/v1/events", Some(body)).await?);
    }
    print_summary("POST /v1/events", &samples);
    Ok(())
}

async fn run_live_bench(base_url: &str, iterations: usize) -> Result<(), Box<dyn Error>> {
    let base_url = base_url.trim_end_matches('/');
    let client = reqwest::Client::new();
    println!("noet-bench-live base_url={base_url} iterations={iterations}");
    println!("name,count,min_ms,p50_ms,p95_ms,max_ms,avg_ms");
    bench_live_authorize(&client, base_url, iterations).await?;
    bench_live_finalize(&client, base_url, iterations).await?;
    Ok(())
}

async fn bench_live_authorize(
    client: &reqwest::Client,
    base_url: &str,
    iterations: usize,
) -> Result<(), Box<dyn Error>> {
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let request = authorize_request(
            3_000_000 + index,
            Some(&format!("bench-live-authorize-trace-{index}")),
            Some(&format!("bench-live-authorize-request-{index}")),
            Some(&format!("bench-live-authorize-run-{index}")),
        );
        samples.push(
            live_request_duration(
                client
                    .post(format!("{base_url}/v1/authorize"))
                    .json(&request),
            )
            .await?,
        );
    }
    print_summary("LIVE POST /v1/authorize", &samples);
    Ok(())
}

async fn bench_live_finalize(
    client: &reqwest::Client,
    base_url: &str,
    iterations: usize,
) -> Result<(), Box<dyn Error>> {
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let trace_id = format!("bench-live-finalize-trace-{index}");
        let request_id = format!("bench-live-finalize-request-{index}");
        let agent_run_id = format!("bench-live-finalize-run-{index}");
        let request = authorize_request(
            4_000_000 + index,
            Some(&trace_id),
            Some(&request_id),
            Some(&agent_run_id),
        );
        let response = live_request_json(
            client
                .post(format!("{base_url}/v1/authorize"))
                .json(&request),
        )
        .await?;
        let reservation_id = response
            .get("reservation")
            .and_then(|reservation| reservation.get("id"))
            .and_then(Value::as_str)
            .ok_or("authorize response did not include reservation id")?;
        let finalize = finalize_payload(4_000_000 + index, &trace_id, &request_id, &agent_run_id);
        samples.push(
            live_request_duration(
                client
                    .post(format!(
                        "{base_url}/v1/reservations/{reservation_id}/finalize"
                    ))
                    .json(&finalize),
            )
            .await?,
        );
    }
    print_summary("LIVE POST /v1/reservations/{id}/finalize", &samples);
    Ok(())
}

async fn live_request_json(request: reqwest::RequestBuilder) -> Result<Value, Box<dyn Error>> {
    let response = live_request(request).await?;
    Ok(serde_json::from_slice(&response)?)
}

async fn live_request_duration(
    request: reqwest::RequestBuilder,
) -> Result<Duration, Box<dyn Error>> {
    let started = Instant::now();
    let _ = live_request(request).await?;
    Ok(started.elapsed())
}

async fn live_request(request: reqwest::RequestBuilder) -> Result<Vec<u8>, Box<dyn Error>> {
    let response = request.send().await?;
    let status = response.status();
    let url = response.url().clone();
    let bytes = response.bytes().await?.to_vec();
    if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::ACCEPTED {
        return Err(format!(
            "{url} returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        )
        .into());
    }
    Ok(bytes)
}

async fn request_json(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Option<Vec<u8>>,
) -> Result<Value, Box<dyn Error>> {
    let bytes = request_body(app, method, uri, body).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn request_duration(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Option<Vec<u8>>,
) -> Result<Duration, Box<dyn Error>> {
    let started = Instant::now();
    let _ = request_body(app, method, uri, body).await?;
    Ok(started.elapsed())
}

async fn request_body(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Option<Vec<u8>>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let response = app
        .oneshot(builder.body(Body::from(body.unwrap_or_default()))?)
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
    if status != StatusCode::OK && status != StatusCode::ACCEPTED {
        return Err(format!(
            "{uri} returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        )
        .into());
    }
    Ok(bytes)
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
    let avg = if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    };
    println!(
        "{name},{},{min:.3},{p50:.3},{p95:.3},{max:.3},{avg:.3}",
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

fn remove_sqlite_files(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}
