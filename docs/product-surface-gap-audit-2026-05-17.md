# Product surface gap audit — 2026-05-17

## Scope

This audit tracks product-language drift between shipped Noether surfaces and the way docs describe
them.

Audited surfaces:

- `README.md`
- `docs/roadmap.md`
- `docs/github-issues.md`
- `docs/team-deployment.md`
- `docs/export-reporting-api.md`
- `docs/product-vision.md`
- `docs/README.md`
- `src/server.rs`
- `src/cli.rs`
- `src/ledger.rs`
- `src/reporting.rs`
- `src/live_dashboard.rs`

## Method

This audit used direct code and doc reads only. No design conclusions are treated as facts in this
document.

## Current implementation baseline

Verified code facts:

- `src/server.rs` now serves:
  - `POST /v1/authorize`
  - `POST /v1/reservations/{id}/finalize`
  - `POST /v1/events`
  - `GET /v1/reports/usage`
  - `GET /v1/reports/decisions`
  - `GET /v1/reports/traces/{trace_id}`
  - `GET /v1/reports/observations`
  - `GET /v1/reports/dashboard-data`
  - `GET /v1/reports/dashboard`
  - `GET /v1/reports/updates`
  - `GET /dashboard`
  - `GET /dashboard/app.js`
  - `GET /dashboard/app.css`
  - capture/proxy routes such as `/v1/chat/completions`
  - `/health`
- `src/server.rs` still does not implement:
  - `/v1/simulations/*`
- `src/cli.rs` still generates static export dashboard HTML as a file artifact:
  - `report dashboard`
  - `scenario run`
  - `simulate`
- `src/reporting.rs` now exposes a shared reporting domain over the existing ledger reads.

## Status of the original audited gap

The original audited product-surface gap is closed in code:

1. a real reporting HTTP API now exists,
2. a real served live dashboard now exists,
3. the live dashboard is backed by reporting data rather than CLI HTML reuse,
4. the static export dashboard remains a separate product surface.

## Remaining findings after implementation

### Finding 1 — several docs still describe the reporting API and live dashboard as future work

Severity: high

Evidence before correction:

- `docs/team-deployment.md` still said reporting remained CLI/SQLite-only and `/v1/reports/*`
  should not be routed to `noet serve`.
- `docs/export-reporting-api.md` still described the reporting API as proposed future work.
- `docs/README.md` still described the export/reporting contract doc as proposed HTTP shapes.
- `docs/roadmap.md` and `docs/github-issues.md` Slice 8 still said the reporting API and live
  dashboard were future work.

Impact:

- The docs understated shipped capability after the implementation landed.
- Shared deployment guidance would have told readers not to use product surfaces that now exist.

### Finding 2 — simulation HTTP surfaces remain genuinely future-facing and must stay that way

Severity: medium

Evidence:

- `docs/export-reporting-api.md` includes:
  - `GET /v1/simulations/{simulation_id}`
  - `GET /v1/simulations/{simulation_id}/dashboard`
- `src/server.rs` still has no simulation HTTP routes.
- The simulation flow in `src/cli.rs` still writes artifacts under `.noet/simulations/...`; it does
  not persist simulation runs into a server-owned query model.

Impact:

- Simulation HTTP browsing is still not a shipped capability.
- These surfaces must remain explicitly future-facing until a real persistence/query model exists.

### Finding 3 — `docs/product-vision.md` still used unqualified `dashboard` language after Noether grew two real dashboard surfaces

Severity: medium

Evidence before correction:

- the main product diagram ended in `Policy + budget ledger + dashboard`
- the Observe pillar said `local ledger and dashboard`
- the scenario section said reports flowed through `dashboard`
- the validation examples said `reports and dashboards explain the scenario in human terms`

Impact:

- Now that Noether ships both a static export dashboard and a live dashboard, bare `dashboard`
  language is ambiguous again unless the surface is obvious from context.

## Audit conclusion

The earlier product-surface closure work changed the code reality. The remaining work in this audit
pass is documentation alignment:

- describe the reporting API and live dashboard as shipped,
- keep simulation HTTP endpoints future-facing,
- qualify `dashboard` language where multiple surfaces now exist.
