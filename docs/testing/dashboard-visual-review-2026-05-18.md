# Dashboard visual review — 2026-05-18

## Scope

Review the served live dashboard after the visual redesign pass against a rich seeded dataset and
real generated simulation artifacts.

Covered surfaces:

- `/dashboard` overview
- `/dashboard` budgets
- `/dashboard` adoption
- `/dashboard` trace explorer
- `/dashboard` strategy lab

## Environment

- repo: `noether`
- server:

  ```bash
  cargo run --quiet --bin noet -- serve \
    --bind 127.0.0.1:4051 \
    --db-path /tmp/noether-dashboard-review/live.sqlite \
    --simulation-dir /tmp/noether-dashboard-review/simulations \
    --policy examples/dashboard-review-policy.noet.yaml
  ```

- seeded review dataset:

  ```bash
  bash examples/dashboard-review-seed.sh \
    http://127.0.0.1:4051 \
    /tmp/noether-dashboard-review/live.sqlite \
    /tmp/noether-dashboard-review/simulations
  ```

## Seed characteristics

The seeded review dataset intentionally mixes:

- multiple teams/projects/users
- multiple budgets and fallback behavior
- multiple providers/models
- cache-heavy and cache-poor traces
- tool-heavy runtime traces
- one explicit deny trace
- lifecycle limit reporting via `pi.tool_call`, `pi.turn_end`, and `pi.provider_call.started`
- generated simulations:
  - `synthetic-company`
  - `runaway-pressure`
  - `adoption-pressure`

## Browser-grounded capture

Primary path:

- attempted Playwriter browser review
- blocked by disconnected extension in this lane

Fallback path used:

- headless Chrome via raw CDP against the real served app

Captured screenshots:

- overview: `/tmp/noether-dashboard-review/overview.png`
- budgets: `/tmp/noether-dashboard-review/budgets.png`
- adoption: `/tmp/noether-dashboard-review/adoption.png`
- traces: `/tmp/noether-dashboard-review/traces.png`
- strategy (default selected simulation): `/tmp/noether-dashboard-review/strategy.png`
- strategy (runaway-pressure selected): `/tmp/noether-dashboard-review/strategy-runaway.png`

## Observed outcome

### Overview

- shell now uses real Noether assets from `assets/brand/`
- layout reads like an observability cockpit rather than prose-first cards
- metrics, ranked concentration, policy posture, and trace ranking are visible above the fold
- number formatting is materially stronger than the rejected pass

### Budgets

- bucket pacing is now presented as a dense pressure table
- the page shows remaining room, projected variance, peak-day share, and bucket shape directly
- concentration and a daily heatmap are present, so the page reads as burn/pacing analysis

### Adoption

- adoption is now visibly analytical rather than descriptive
- cache/tool/limit patterns are shown in both a scatter plot and dense matrix
- the page is reviewable with the seeded data, though the strongest protected-pool story only
  appears when the reviewer switches to a user-oriented lens

### Trace Explorer

- the trace surface now behaves like a trace review page instead of stacked text cards
- shortlist, timeline lanes, correlated event table, and interoperability stats all render
- the seeded labs/runtime traces make report-only lifecycle limitrails and tool density visible

### Strategy Lab

- the strategy surface is materially redesigned, not only re-skinned
- it now shows objective winners, a cost/adoption scatter, model concentration, normalized
  comparison bands, and a detailed tradeoff matrix
- `runaway-pressure` is the strongest review simulation for differentiation; its screenshot shows
  the intended decision-support shape clearly

## Review verdict

Outcome: **reviewable**

The live dashboard is now in a materially different product state than the rejected pass:

- chart-first
- denser
- branded with actual Noether assets
- trace-native
- strategy-lab redesigned as scenario analysis / decision support
- backed by a rich seeded dataset instead of a toy ledger

## Remaining polish notes

These are no longer blockers to reviewability, but they are visible follow-ups:

- the budgets hero aside can still be made more informative
- adoption defaults are most compelling when lensing by user rather than project
- strategy scatter labels can still be tuned for tighter two-point layouts
