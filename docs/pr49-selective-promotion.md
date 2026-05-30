# PR #49 selective promotion notes

Date: 2026-05-29

Source PR: <https://github.com/lgrossi/noether/pull/49>

Context: PR #48 already merged PostgreSQL ledger support into `main`. PR #49 is therefore not a merge candidate as-is; it is a source of candidate tests, seams, and benchmark ideas.

## Promotion matrix

| Component | PR #49 idea | Value | Risk after PR #48 | Decision |
| --- | --- | --- | --- | --- |
| Server parity tests | Run the same endpoint behavior against SQLite and Postgres | High confidence that both backends preserve auth/finalize/event/report basics | Low if adapted to current `LedgerBackend` instead of importing PR #49 `Backend` | Promoted in this branch |
| Test Postgres isolation | Per-test schema with `search_path` | Avoids test data collision in shared live PG database | Low; same pattern already exists in current server tests | Promoted in this branch |
| Backend abstraction (`src/backend.rs`) | Large pluggable storage abstraction plus split hot state | Could reduce server branching long term | High; duplicates or bypasses #48 advisory lock/reload/async-finalize semantics | Do not promote as-is; promote only a narrow explicit backend marker |
| Hot-path rewrite | Local hot-state first, persisted writes via backend dispatch | Potential latency win | High; #48 deliberately reloads shared DB state and uses advisory transaction locks for multi-instance correctness | Defer to a correctness-first design |
| `tokio::try_join!` pipelining | Parallelize independent PG writes | Possible latency reduction | Medium; must preserve transaction/advisory-lock boundaries and failure atomicity | Benchmark/design separately |
| `UNNEST`/batch inserts | Reduce PG round trips for batch writes | Possible seed/report performance win | Medium; useful mainly for bulk seed or non-hot paths | Benchmark separately |
| `events_count` instead of retained events vec | Avoid in-memory event growth | Already addressed differently in current main via persistence-focused paths | Low value now | No action |
| Report/read dispatch | Backend-specific read path | Already present in current main through `AppState::read_ledger` and `spawn_blocking` reads | Low value now | No action |
| `noet-bench` p99/live additions | Better endpoint benchmark reporting | Mostly already present in current main from #48 follow-up work | Low value now | No action |
| `examples/direct-bench.rs` | Direct DB-layer benchmark separate from Axum service bench | Useful for isolating storage cost | Medium; PR #49 version depends on its unmerged backend/hot-state API | Rebuilt against current APIs in this branch |
| Simulation PG isolation | Schema-per-strategy simulation isolation | Could be useful for future PG simulation support | Medium; current simulation path remains SQLite-oriented | Defer |

## What was promoted

This branch adds `tests/parity_server.rs` plus `tests/common/mod.rs`.

The tests cover:

- `/v1/authorize` creates reservations.
- `/v1/reservations/{id}/finalize` is idempotent.
- finalize rejects invalid accounting.
- `/v1/events` accepts trace events.
- `/health` reports readiness.
- enforce-mode policy deny is surfaced by `/v1/authorize`.
- spend caps deny over-cap requests.
- dry-run deny still returns a decision payload.
- authorize/finalize writes are visible through usage reports.
- reservations are counted toward subsequent spend-limit decisions.

Postgres parity runs only when `NOET_TEST_POSTGRES_URL` is set. Otherwise the same tests run SQLite and print a skip line for the Postgres half.

This branch also replaces `BudgetLedger` backend dispatch based on optional
connection presence with a typed selected-store enum. SQLite and Postgres
remain supported; the ledger now owns exactly one active store variant instead
of carrying parallel optional connections.

This branch also adds `examples/direct-bench.rs`, rebuilt against the current
SQLite `BudgetLedger` and Postgres `AsyncPostgresLedger` APIs rather than PR
#49's unmerged HotState/backend API.

This branch also absorbs two incidental PR #49 bug fixes without taking the
HotState rewrite:

- SQLite/in-memory server write paths now run synchronous ledger work on
  Tokio's blocking pool instead of executor worker threads.
- Durable ledgers no longer retain every recorded trace event in process
  memory; `BudgetLedger` keeps an event count while reports continue to read
  persisted events.

## Validation performed

- `cargo test --test parity_server`: 10 passed, SQLite path.
- `NOET_TEST_POSTGRES_URL=postgres://spillio:spillio@127.0.0.1:5432/spillio cargo test --test parity_server`: 10 passed, SQLite plus live Postgres schema-isolated parity paths.
- `cargo test`: 168 passed, 4 ignored.
- `cargo run --release --example direct-bench -- --backend sqlite --iterations 10`: passed.
- `cargo run --release --example direct-bench -- --backend postgres --db-url postgres://spillio:spillio@127.0.0.1:5432/spillio?... --iterations 10 --postgres-profile strict`: passed against an isolated schema.

## Current recommendation

Promote #49 in small follow-up PRs only:

1. parity tests (this branch),
2. direct DB-layer benchmark rebuilt against current main APIs (this branch),
3. larger backend abstraction only after a design that preserves #48 advisory-lock/shared-state semantics,
4. hot-path optimizations only with SQLite and Postgres benchmark evidence plus multi-instance correctness tests.
