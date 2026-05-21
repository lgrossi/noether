# Hybrid budget pacing windows plan

Source design: `docs/design/hybrid-budget-pacing-windows-2026-05-20.md`

## Recommended implementation slices

### Slice 1 - explicit hybrid pacing for `first_seen` tumbling

Goal: land the user-visible feature set needed for `$1000 / 30d tumbling + $100 / 1d tumbling +
$40 / 5h rolling`.

#### Task 1 - extend the policy contract and validator

- Add to `src/contract.rs`:
  - `BudgetWindowMode`
  - `WindowAnchorPolicy`
  - `SpendWindowMode`
  - optional `BudgetRule.window_mode`
  - optional `BudgetRule.window_anchor`
  - optional `SpendWindowLimit.id`
  - optional `SpendWindowLimit.mode`
  - optional `SpendWindowLimit.anchor`
- Update `src/policy.rs` validation:
  - accept legacy omitted fields
  - require `anchor.kind` when `mode=tumbling`
  - reject duplicate spend-window IDs inside one budget
  - warn on legacy implicit windows during `policy check`
- Verification:
  - serde round-trip tests for legacy and explicit policies
  - validator tests for invalid mode/anchor combinations

#### Task 2 - implement stable tumbling window advancement

- Refactor `src/ledger.rs` window advancement so explicit tumbling windows advance by whole window
  multiples instead of `started_at = now`.
- Apply that logic to:
  - main budget windows
  - any future helper that tracks tumbling limit windows
- Preserve the legacy path for omitted `window_mode`.
- Verification:
  - unit test that idle gaps do not shift explicit tumbling boundaries
  - unit test that legacy budgets retain current reset behavior

#### Task 3 - add tumbling spend-limit accounting

- Introduce persisted limit state, likely a new SQLite table such as:
  - `limit_window_states(rule_id, limit_id, scope_key, started_at, used_usd)`
- Evaluate spend limits by mode:
  - `rolling`: keep current `recent_spend_usd(...)` behavior
  - `tumbling`: consult limit state for the current bucket
- Increment tumbling limit usage when a reservation is created under the selected budget and matched
  scope.
- Use explicit limit IDs in explanations and persistence keys.
- Verification:
  - tumbling limit deny/warn tests
  - coexistence test for `1d tumbling` and `1d rolling` on the same budget
  - SQLite reopen test for persisted limit buckets

#### Task 4 - expose structured window reasoning in reports

- Extend decision/report structs in `src/ledger.rs` and downstream JSON surfaces:
  - routing window fields for the selected budget
  - limit-hit window fields for pacing decisions
- Update summary generation in `src/ledger.rs`, CLI rendering in `src/cli.rs`, and any dashboard
  views that headline limit hits.
- Verification:
  - JSON report tests
  - CLI summary tests
  - one scenario assertion that checks explicit window metadata in the decision export

#### Task 5 - docs and runnable scenarios

- Update:
  - `docs/policy-v0.md`
  - `docs/control-contract-v0.md`
  - `docs/export-reporting-api.md`
  - relevant examples under `examples/`
- Add a scenario that demonstrates:
  - main 30d tumbling budget
  - 1d tumbling pacing deny
  - 5h rolling burst deny
- Verification:
  - `cargo test`
  - `noet scenario run <new-scenario>`

### Slice 2 - calendar anchors and unit-aware windows

Goal: support true billing-style windows without overloading raw seconds.

- Add `calendar_utc` and `calendar_tz` anchors.
- Introduce unit-aware window values for month/week/day alignment.
- Decide whether `30d tumbling` remains separate from `calendar month`.
- Migrate `allocation.window: monthly` toward the same shared semantics.

### Slice 3 - schema cleanup and UX polish

Goal: reduce policy ambiguity and make review/report surfaces self-explanatory.

- Normalize budget and limit windows onto one shared `WindowSpec`.
- Upgrade policy-check warnings into a clearer migration guide.
- Improve dashboard cards to show active window bounds and remaining pacing headroom.

## Safe sequencing

1. Contract + validator
2. Main budget tumbling advancement
3. Tumbling spend-limit state/evaluation
4. Reporting/UI fields
5. Docs/scenario coverage

This order keeps parsing and persistence changes ahead of behavior changes, and it yields testable
vertical checkpoints after each step.

## Recommended first slice vs follow-ups

First slice should include Tasks 1-5 above. That is the smallest coherent delivery that actually
supports hybrid pacing.

Later follow-ups should cover:

- calendar/timezone anchors
- normalization of window schema
- alignment of protected-adoption allocation windows with the same model
