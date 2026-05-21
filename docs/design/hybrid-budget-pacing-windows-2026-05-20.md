# Hybrid budget pacing windows

## Verified current state

- Main budget limits are driven by `BudgetRule.window_seconds` and `BudgetLedger::window()` in
  `src/ledger.rs`. When a bucket expires, the code resets `started_at = now` and `used_usd = 0`.
  That is a lazy activity-anchored bucket, not an explicitly modeled calendar or fixed-phase
  window.
- `limits.spend[]` accepts only a duration string such as `5h` or `7d`
  (`src/policy.rs::parse_limit_window`) and is evaluated as trailing recent spend via
  `recent_spend_usd(..., now - window, now)` in `src/ledger.rs`.
- Main budget state is persisted in `budget_windows(rule_id, started_at, used_usd)`. Spend-window
  limits have no persisted bucket state today.
- Decision/report surfaces only expose generic `limit_hits[].rule_id/reason/severity` and
  `routing.budget_window_remaining_usd`; they do not tell the user which window mode was evaluated or the
  window bounds that caused a hit.

## Desired end state

One budget can combine:

- a main budget cap such as `$1000 / 30d tumbling`
- a pacing cap such as `$100 / 1d tumbling`
- an anti-burst cap such as `$40 / 5h rolling`

Those semantics must be explicit in policy and visible in decision/report output.

## Decision 1: make window semantics explicit and additive under `limits.spend[]`

```yaml
budgets:
  - id: personal-primary
    limits:
      spend:
        - id: budget-cap
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1000
          warn_at_fraction: 0.8
          action: block
        - id: daily-cap
          window: 1d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 100
          action: block
        - id: burst-5h
          window: 5h
          mode: rolling
          max_usd: 40
          action: block
```

Why this shape:

- it keeps existing `window_seconds` and `window` fields valid;
- it avoids a breaking rewrite while making mode/anchor explicit;
- `limits.spend[].id` is needed because `budget.spend_window.1d` cannot distinguish
  tumbling `1d` from rolling `1d`.

Longer term, vnext should normalize both budget windows and limit windows onto one shared
`WindowSpec` object. The first slice should not block on that rename.

## Decision 2: define explicit tumbling anchors, but start with `first_seen`

Window semantics should be:

- `rolling`: trailing lookback from `now - size` to `now`
- `tumbling`: fixed-size buckets whose boundaries advance by whole window increments

Anchor semantics should be explicit for tumbling windows:

- `first_seen`: the first accepted request creates the anchor; later buckets advance by whole
  multiples of the window size, not by `now`
- later follow-up: `calendar_utc`
- later follow-up: `calendar_tz` with IANA timezone

Recommendation for first slice:

- support `first_seen` only;
- reject or warn on calendar anchors for now;
- update wording everywhere from ambiguous `monthly` to precise `30d tumbling` unless a future
  calendar anchor is actually configured.

## Decision 3: preserve legacy policies, but make legacy semantics visible

Compatibility story:

- a budget with only `window_seconds` keeps current behavior in the compatibility path;
- a spend limit with only `window` keeps current rolling behavior;
- `noet policy check` should emit warnings for implicit legacy windows:
  - main budgets: implicit lazy activity-anchored bucket
  - spend limits: implicit rolling limit

This avoids silent behavior changes for existing policies while pushing new policies toward
explicit semantics.

## Decision 4: use one window-accounting model for budgets and tumbling limits

The evaluator should stop treating tumbling limits as ad hoc history queries. The reusable
accounting model should be:

- shared window key = budget id + optional limit id + optional matched-entity scope
- shared state = `started_at`, `used_usd`
- shared advancement rule = if elapsed >= size, advance by whole multiples of the size

That keeps main budget windows and tumbling limit windows coherent, while rolling limits continue
to query recent reservation spend from history.

## Decision 5: decisions and reports must explain window hits structurally

When Noether warns or denies on a pacing window, users need more than a prose string. Add optional
structured fields to decision/report surfaces:

- for routing:
  - `budget_window_mode`
  - `budget_window_started_at`
  - `budget_window_ends_at`
- for limit hits:
  - `window_id`
  - `window_mode`
  - `window_started_at`
  - `window_ends_at`
  - `projected_spend_usd`
  - `max_usd`
  - `scope_entity`

The existing human-readable reason string stays, but it should be generated from these fields so
CLI, extension UX, and dashboards tell the same story.

## Rejected alternatives

- Treat all pacing as rolling: strong anti-burst behavior, but it does not match the intended
  allowance model.
- Change all existing budgets to strict tumbling immediately: too risky because current policies
  rely on implicit legacy behavior.
- Keep spend-window IDs derived only from duration strings: incompatible with mixed tumbling and
  rolling limits of the same size.

## First-slice scope

Recommended first slice:

1. explicit mode/anchor fields in policy
2. explicit limit IDs
3. `first_seen` tumbling support for main budgets and spend limits
4. structured report fields for window decisions

Defer:

- calendar/timezone anchors
- schema normalization to a shared `WindowSpec` object
- migration of `allocation.window: monthly` onto the same window model
