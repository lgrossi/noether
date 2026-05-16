# Roadmap

This roadmap is a sequence of validation slices, not a complete product backlog.

## Slice 0: capture spike

Status: started.

Goal: prove Noether can observe real harness traffic through supported local base URL or proxy hooks.

Acceptance:

- Pi can be configured to send traffic through `noet serve`.
- At least one additional harness is tested: Claude Code, OpenCode, or Codex.
- Fixtures capture request path, redacted headers, request body, response status/body, and trace id.
- Captured traffic is sufficient to draft the first control contract.

## Slice 1: control contract v0

Goal: define Noether's stable domain boundary before adding budget logic.

Acceptance:

- Rust structs and documented JSON examples for:
  - `AuthorizeRequest`;
  - `AuthorizeDecision`;
  - `Reservation`;
  - `FinalizeReservation`;
  - `TraceEvent`;
  - `UsageObservation`;
  - `ToolEvent`;
  - `EvalAnnotation`.
- Contract examples derived from captured harness fixtures.
- Explicit compatibility notes for capture-only, async-only, and enforcement modes.

## Slice 2: local ledger

Goal: persist decisions, reservations, usage, and events locally.

Acceptance:

- SQLite-backed ledger.
- CLI report for spend/usage by project and model.
- Idempotent finalize path for reservations.
- Fixtures and trace events can be linked by trace id.

## Slice 3: policy-as-code

Goal: evaluate simple local policies before model calls.

Acceptance:

- `policy.noet.yaml` supports subjects, projects, model classes, logging rules, and budgets.
- `noet policy check` validates config and explains rules.
- `POST /v1/authorize` returns allow, deny, or warn with explanations.

## Slice 4: hard budget semantics

Goal: make budget enforcement real.

Acceptance:

- Reservation-before-call.
- Reconciliation-after-call.
- Rolling window and fixed window budgets.
- Concurrent reservations cannot overspend a hard budget under normal operation.
- Expired or abandoned reservations are handled explicitly.

## Slice 5: first useful integration

Goal: integrate Noether with one real workflow beyond capture.

Candidates:

- Pi custom provider/capture path;
- Claude Code proxy/base URL path;
- OpenCode custom provider;
- LiteLLM/Bifrost hook or sidecar mode.

Acceptance:

- Integration calls Noether's decision API.
- Reports show attributed usage by project/session.
- Failure mode is documented: fail-open, fail-closed, or warn-only.

## Slice 6: scenario emulator

Goal: validate Noether's product claims with self-contained realistic examples.

Acceptance:

- Scenario files can describe entities, budgets, requests, model choices, tool activity, usage,
  denials, fallbacks, and finalization.
- `noet` can replay a scenario through the public contract instead of private ledger shortcuts.
- Generated reports/dashboard show the expected story without live provider traffic.
- Scenarios cover at least:
  - one individual/local run;
  - one team/project budget run;
  - one model-denial/fallback run;
  - one runaway or spend-window guard run;
  - one adoption/carryover-style run once that allocation standard exists.
- Scenario assertions can fail CI when Noether behavior or report output drifts.

## Later

- Postgres storage.
- OpenTelemetry export.
- Central service auth.
- Admin/reporting API.
- Majin cockpit integration.
- Evaluation labels and outcome comparison.
- Gateway hooks for existing proxies.
