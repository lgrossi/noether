# Dashboard redesign parity review — 2026-05-18

## Scope

This review covers the redesigned live dashboard surface served by `noet serve` and the current
CLI/export dashboard surfaces that still exist in `src/cli.rs`.

## Live dashboard surface now covers

- executive overview
- budgets / bucket pressure and pacing
- adoption / entity health and coaching signals
- trace explorer
- strategy lab
- shared controls for:
  - window
  - lens
  - entity
  - trace

## New live-dashboard API contracts

- `GET /v1/dashboard/filters`
- `GET /v1/dashboard/overview`
- `GET /v1/dashboard/budgets`
- `GET /v1/dashboard/adoption`
- `GET /v1/dashboard/traces`
- `GET /v1/dashboard/strategy-lab`

Legacy reporting routes remain available.

## Verification

- `cargo test --quiet`
  - 102 passed
- server tests cover:
  - split dashboard API contracts
  - live dashboard shell/assets
  - strategy lab endpoint
  - existing reporting routes
  - existing simulation routes

## Parity outcome

The live product now covers the major product areas that previously required a mix of:

- live dashboard shell
- reporting endpoints
- CLI-generated run dashboards
- CLI-generated simulation dashboards

However, the CLI dashboards still remain useful as static/offline artifacts and should **not** be
removed yet without explicit visual/product signoff.

## Retirement decision

Decision: **do not retire CLI dashboards in this slice**.

Reason:

- the live product shape now exists
- the split dashboard contracts now exist
- the strategy lab now exists
- but CLI removal should stay behind final human acceptance of:
  - live visual quality
  - offline/export replacement expectations
  - simulation comparison ergonomics

## Next acceptable deletion boundary

CLI dashboard removal becomes eligible only after explicit signoff that the live dashboard +
strategy lab fully replace the intended review workflows.
