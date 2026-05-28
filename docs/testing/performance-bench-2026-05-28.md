# Performance bench

Date: 2026-05-28

## Command

```bash
cargo run --release --bin noet-bench -- --rows 10000 --iterations 5
```

The bench seeds a local SQLite ledger with synthetic authorize/finalize/event rows, then measures
in-process Axum requests for the app endpoints and hot-path sidecar API.

## Results

| Endpoint | Before p50 | After p50 | Notes |
| --- | ---: | ---: | --- |
| `GET /v1/app/policy` | ~65 ms | ~9 ms | Rule stats now use SQL aggregation instead of materializing all decisions. |
| `GET /v1/app/runs?limit=80` | ~127 ms | ~28 ms | Default unfiltered page now uses SQL pagination and page-scoped usage lookup. |
| `GET /v1/app/replay` without draft | ~144 ms | ~18 ms | Empty replay state now uses aggregate totals instead of rebuilding history. |
| `POST /v1/authorize` | ~77 ms at 1k rows | ~0.15 ms | SQLite uses WAL + normal synchronous mode for local sidecar write latency. |
| `POST /v1/reservations/{id}/finalize` | ~6 ms at 1k rows | ~0.10 ms | Same SQLite durability tuning. |
| `POST /v1/events` | ~2.6 ms at 1k rows | ~0.04 ms | Same SQLite durability tuning. |

## Remaining work

- Replay with a saved draft still re-evaluates historical authorizations. That is expected to be
  heavier than the empty replay state and should get a separate benchmark once the UI supports
  large replay diffs.
- Filtered `/runs` still falls back to materializing history so rule/search filters stay exact.
  Move filtered search to SQL when larger real datasets make that path slow.
