# Roadmap

This roadmap is a sequence of product validation slices. It is intentionally not a complete
backlog. Each slice should prove a public-facing Noether claim.

## Slice 1: local visibility

Goal: make one local AI workflow understandable without central infrastructure.

Acceptance:

- Local sidecar starts with SQLite storage.
- A real integration can call `POST /v1/authorize`.
- Usage and lifecycle events can be finalized/ingested.
- Reports show usage, decisions, trace stories, and observations.
- Static export dashboard gives a non-table view of spend, tokens, decisions, tools, agent
  activity, and trace timeline.
- Normal mode does not store prompt/body content by default.

## Slice 2: first excellent integration

Goal: make one existing AI workflow feel native with Noether.

Acceptance:

- Pi extension can run in normal extension mode.
- Authorization happens before provider send.
- Lifecycle and usage delivery is asynchronous and does not degrade Pi UX.
- Fail-open/fail-closed behavior is documented.
- Real-hook findings are represented in tests.
- Reports and the static export dashboard explain a Pi run without raw hook logs.

## Slice 3: attribution and model control

Goal: make AI work belong to a budget and constrain model access.

Acceptance:

- Requests can carry `budget_id` and `entities`.
- Budgets can define eligible entities.
- Explicit valid budget wins.
- Invalid explicit budget falls back to inferred valid budgets.
- Inference chooses by specificity, priority, best-fit budget pressure, and stable id.
- Budgets can define provider/model allowlists.
- Decisions explain selected budget, rejected requested budget, matched entity, model check, and
  remaining budget.

## Slice 4: AI-native guardrails

Goal: prevent unhealthy AI usage patterns without relying only on money caps.

Acceptance:

- Guards can enforce or warn on max request cost.
- Guards can enforce or warn on context size.
- Spend windows such as 5h and 7d can catch fast burn.
- Tool-call, retry, and agent-step guards are represented once lifecycle data supports them.
- Static export dashboard and reporting highlight guard hits and risky runs.

## Slice 5: adoption governance

Goal: support safe AI adoption instead of only spend reduction.

Acceptance:

- `protected_adoption_pool` is supported.
- Carryover is a separate bucket.
- Carryover is consumed before current grant.
- Remaining current grant carries over by configured percentage up to a cap.
- Low adopters and unused protected opportunity are visible.
- Reports distinguish savings, unused opportunity, and protected carryover liability.

## Slice 6: native scenario examples

Goal: give users concrete, runnable examples of Noether behavior.

Acceptance:

- Scenario files can describe budgets, entities, requests, model choices, tool activity, usage,
  denials, fallbacks, and finalization.
- `noet scenario run <file>` replays the scenario through public contract surfaces.
- Generated reports and the static export dashboard show the expected story without live provider
  traffic.
- Initial scenarios cover:
  - individual local developer;
  - team pooled budget;
  - project budget fallback;
  - model-denial/fallback;
  - runaway agent guard;
  - protected adoption pool.
- Scenario assertions can fail CI when behavior or report output drifts.

## Slice 7: strategy simulation lab

Goal: compare Noether strategies against realistic synthetic organizations.

Acceptance:

- Simulation files can define a synthetic company with users, teams, projects, behavior profiles,
  models, budgets, and strategy variants.
- Example profiles include power users, steady users, low adopters, experimenters, and loop-risk
  agents.
- `noet simulate <file>` compares strategies such as pooled caps, protected adoption pools,
  reserved/shared budgets, and future standards.
- Output compares budget usage, unused budget, denied requests, useful work blocked, runaway spend
  prevented, adoption coverage, fairness, model mix, carryover liability, and exhaustion timing.
- Simulation output can generate a static export dashboard and report for human review.

## Slice 8: team deployment

Goal: make Noether usable beyond one laptop.

Acceptance:

- Shared server deployment path is documented.
- Storage can move beyond local SQLite when needed.
- Auth/trusted-upstream story is explicit.
- Reporting HTTP API and live dashboard are served by `noet serve` for dashboards and external
  analysis.
- Existing local-first behavior remains available.

## Later

- Postgres storage.
- OpenTelemetry export.
- Central service auth.
- Admin/reporting HTTP API.
- Majin cockpit integration.
- Evaluation labels and outcome comparison.
- Gateway hooks for existing proxies.
