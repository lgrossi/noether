# Simulation surface smoke — 2026-05-18

## Goal

Verify the new served simulation surfaces behave coherently when the simulation directory is empty
and when it contains a real generated simulation artifact set.

## Commands

```bash
tmpdir="$(mktemp -d)"

cargo run --quiet --bin noet -- serve \
  --bind 127.0.0.1:4051 \
  --db-path "$tmpdir/noether.sqlite" \
  --fixture-dir "$tmpdir/fixtures" \
  --simulation-dir "$tmpdir/simulations"

curl -i http://127.0.0.1:4051/v1/simulations
curl -i http://127.0.0.1:4051/simulations

cargo run --quiet --bin noet -- simulate \
  --out-dir "$tmpdir/simulations/runaway-pressure" \
  examples/simulations/runaway-pressure.noet.yaml

curl -i http://127.0.0.1:4051/v1/simulations
curl -i http://127.0.0.1:4051/v1/simulations/runaway-pressure
curl -i http://127.0.0.1:4051/v1/simulations/runaway-pressure/dashboard
curl -i \
  'http://127.0.0.1:4051/v1/simulations/runaway-pressure/strategies/pooled%20without%20guard/usage'
curl -i \
  'http://127.0.0.1:4051/v1/simulations/runaway-pressure/strategies/guarded%20team%20budget/dashboard'
curl -i http://127.0.0.1:4051/simulations
```

## Empty-state evidence

- `GET /v1/simulations` returned `200 OK` with `[]`.
- `GET /simulations` returned `200 OK` and rendered:
  - `Noether simulation surfaces`
  - `No simulation artifacts are available yet`

## Populated-state evidence

Generated simulation:

- `examples/simulations/runaway-pressure.noet.yaml`
- output dir: `$tmpdir/simulations/runaway-pressure`

Observed API values:

- `GET /v1/simulations`
  - `id`: `runaway-pressure`
  - `name`: `runaway pressure`
  - `total_requests`: `115`
  - `strategy_count`: `2`
  - `dashboard_url`: `/v1/simulations/runaway-pressure/dashboard`
- `GET /v1/simulations/runaway-pressure`
  - `name`: `runaway pressure`
  - `total_requests`: `115`
  - strategies:
    - `pooled without limit`
    - `limited team budget`
- `GET /v1/simulations/runaway-pressure/strategies/pooled%20without%20guard/usage`
  - `total_cost_usd`: `11.988104`
  - `rows`: `5`

Observed HTML markers:

- `GET /v1/simulations/runaway-pressure/dashboard`
  - `Comparison summary`
  - `Guardrails changed the budget story`
  - `pooled without limit exhausted shared budget on day 3.`
- `GET /v1/simulations/runaway-pressure/strategies/guarded%20team%20budget/dashboard`
  - `Noether run dashboard`
  - `Risky runs`
  - `Budget routing`
- `GET /simulations`
  - `Noether simulation surfaces`
  - `runaway pressure`
  - `pooled without limit`
  - `limited team budget`

## Result

The served simulation API and browser surface behave coherently for both empty and populated
artifact states, and the per-strategy surfaces remain reachable through URL-encoded strategy ids.
