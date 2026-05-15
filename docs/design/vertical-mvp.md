# Vertical MVP design

Date: 2026-05-15

## Current state

Noether has the right hot-path shape but only a tracer bullet: `/v1/authorize`, finalization,
`/v1/events`, fixed-window policy, capture/proxy fixtures, and the normal Pi extension work. Ledger
state and events are in memory, so a Pi run is not durable or reportable.

## Desired end state

One real Pi run leaves an inspectable local story:

```text
Pi extension -> authorize/reserve -> provider call -> finalize usage
             \-> trace/tool/eval events ---------> SQLite -> CLI reports
```

## Decision 1: MVP boundary

Represent:

- persisted policy decision for every authorization attempt, including denies and fail-closed errors;
- persisted allow/warn reservation, idempotent finalization, and actual usage/cost reconciliation;
- trace timeline by `trace_id`: authorization, provider response, usage/finalize, failures, and
  integration lifecycle;
- optional tool and eval observations accepted and reportable, even if first examples are thin;
- local CLI reports for usage, decisions, traces, and observations.

Defer: budget polish beyond fixed-window, provider compatibility beyond transparent proxy fallback,
dashboard/Majin UI, OpenTelemetry, Postgres, central auth, team admin, remote retention, normal-path
prompt/response retention, automatic Pi install, and enforcement outside user-enabled extension mode.

Rationale: budget is validated enough. The next risk is whether Noether can narrate a whole agent
run across control, usage, trace, and observability surfaces.

## Decision 2: Durable storage is SQLite

Use SQLite for local mode, not JSONL. The first schema owns:

- `schema_migrations`;
- `decisions`: `decision_id`, attribution/model fields, estimates, outcome, explanations JSON,
  policy version/hash, trace/session/request correlation, and timestamps;
- `reservations`: `reservation_id`, `decision_id`, estimated/actual amount, currency, status,
  expiry, and finalized timestamp;
- `usage_observations`: reservation/trace links, provider/model/tokens/cost, latency, stop reason,
  source, and metadata JSON;
- `events` keyed by event id and indexed by `trace_id`, kind, occurred_at, source, and payload JSON;
- `budget_windows` or ledger entries sufficient to restart fixed-window accounting;
- `captures` as metadata links to fixture files, not copied prompt/response bodies.

SQLite is still local disk storage: one file, no server. It is less setup than maintaining ad hoc
JSON/JSONL indexes, file locks, atomic rewrites, and report scans for reservations and traces.
JSONL remains fine for capture fixtures and debug export, but not as the ledger of record.

## Decision 3: Keep one generic ingest endpoint with typed event families

Keep `POST /v1/events` for async ingest. Do not add `/v1/tool-events`, `/v1/eval-annotations`, or
`/v1/usage` for the MVP. Define stable event `kind` families and validate known payloads:

- `request.started`, `request.completed`, `request.failed`;
- `usage.observed` for usage not represented by reservation finalization;
- `tool.observed` with a `ToolEvent`-shaped payload;
- `eval.annotation` with an `EvalAnnotation`-shaped payload;
- integration aliases such as `pi.provider_response`, accepted but normalized for reports.

Reservation finalization remains canonical for spending. Usage events are timeline/async-only data;
when both exist, reports prefer finalized reservation usage for spend totals.

Rationale: one event stream matches the provider-neutral control-plane boundary and avoids endpoint
churn until a family needs different authorization, idempotency, or write semantics.

## Decision 4: Reporting UX is CLI-first and story-shaped

Add `noet report` with four views:

- `usage --since <window> [--project ...] [--model ...]`: totals by project, provider, model,
  subject, tokens, cost, reservation status, and source;
- `decisions --since <window> [--outcome deny|warn|allow]`: decision table with explanation summary
  and reservation id;
- `trace <trace_id>`: chronological story with decision, reservation, provider response, finalize
  usage, tool observations, eval annotations, and failures;
- `observations --kind tool|eval [--trace <id>]`: list/detail view for tool and eval observations.

Default output is human-readable; every report gets `--json` before rich formatting. Missing
attribution and async-only observations should be visible rather than hidden in totals.

## Decision 5: Pi correlation and privacy

The Pi extension remains the primary Pi integration. It must emit a stable `trace_id` before
authorization and include it in authorization metadata, all events, and finalization metadata. It
should also include Pi `session_id` when available, a per-provider-call `request_id`, turn/message ids
when available, extension version, model API, cwd, configured subject, and configured project.

Default privacy remains bodyless: no prompt, response, tool input/output, auth headers, or cookies.
Payloads carry shape summaries, usage/cost, status, latency, stop reason, tool metadata, and eval
labels/scores. Body inclusion stays an explicit local escape hatch and must be marked in stored
events.

## Compatibility and migration

Keep current public endpoints and evolve JSON additively:

- `/v1/authorize` accepts optional correlation metadata but does not require old clients to send it;
- `/v1/reservations/{id}/finalize` stays idempotent and may persist extra usage metadata;
- `/v1/events` continues accepting current `TraceEvent` JSON while normalizing known families;
- in-memory `BudgetLedger` becomes a storage-backed repository; SQLite is default local storage and
  memory remains for tests or explicit development fallback;
- first SQLite release starts at schema version 1; no migration from previous runtime memory is
  required because no durable ledger exists.

Follow the existing stance: provider-neutral contract, metadata-first privacy, normal Pi extension
for Pi, transparent proxy as fallback/debug, and additive contracts. Avoid becoming a provider router,
making budget polish the milestone, requiring prompt retention, or splitting event ingestion early.

## Non-blocking details fixed for MVP

- Do not depend on Pi exposing stable session/turn/message ids. Generate extension-local `trace_id`
  and `request_id`; include native Pi ids only when available and mark their source.
- Keep memory storage test-only unless a development fallback is needed during implementation.
