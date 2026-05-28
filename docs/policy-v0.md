# Policy v0

Default policy path: `policy.noet.yaml`

Validate a policy:

```text
noet policy check policy.noet.yaml
```

Run capture with policy decisions recorded but not enforced:

```text
noet serve --policy policy.noet.yaml --decision-mode dry-run
```

Run capture with deny decisions blocking before mock/upstream:

```text
noet serve --policy policy.noet.yaml --decision-mode enforce
```

## Format

```yaml
version: 0
routing:
  mode: explicit_then_fallback
  fallback_order: [project, user, team, group, org, global]
budgets:
  - id: dev-daily
    priority: 0
    match:
      project: noether
    models:
      allow:
        - openai:gpt-4.1
        - anthropic:claude-sonnet-*
    limits:
      spend:
        - id: budget-cap
          by: project
          window: 1d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1.00
          warn_at_fraction: 0.8
          action: block
        - id: spike-5m
          by: project
          window: 5m
          mode: rolling
          max_usd: 40
          action: block
      request_cost:
        max_usd: 0.50
        action: warn
      context_tokens:
        max_tokens: 120000
        action: block
    allocation:
      standard: protected_adoption_pool
      by: user
      protected_amount_usd: 25
      window: monthly
      carryover:
        percent: 10
        cap_usd: 50
policies:
  - id: require-project
    action: block
    reason: project is required for budget attribution
    when:
      missing: project
```

## Budget behavior

The v0 evaluator is an in-memory fixed-window budget:

- routing defaults to `explicit_then_fallback`;
- when `budget_id` is present, Noether tries that budget first and records why it was rejected
  before falling back;
- inferred fallback budgets sort by `routing.fallback_order`, higher `priority`, lower projected budget
  pressure, and stable budget id;
- `match` decides when a budget applies;
- omit `match` for a catch-all budget;
- flat `match` fields compose with AND semantics by default;
- `match.any` expresses OR across nested clauses;
- `match.not` negates a nested clause;
- `match.project`, `match.user`, `match.team`, `match.group`, `match.org`,
  `match.workflow`, and `match.surface` all use typed request entities;
- `models.allow` constrains a matching budget to provider/model patterns such as
  `openai:gpt-4.1` or wildcard suffixes such as `anthropic:claude-sonnet-*`;
- `limits.request_cost` defines a per-budget request-cost limit that can warn or
  deny when one request's estimated cost exceeds its threshold;
- `limits.context_tokens` defines a per-budget context limit that can warn or deny when
  authorize-time context/input token estimates exceed its threshold;
- `limits.spend[]` is the only money/window constraint model:
  - `by` controls which scope key the window is counted against, such as
    `global`, `project`, `user`, `team`, `group`, `org`, `workflow`, or `surface`;
  - every spend window defines its own `window`, `mode`, `anchor`, `max_usd`,
    `warn_at_fraction`, and `action`;
  - all spend windows compose with AND semantics;
  - if reporting needs one derived broad budget view, Noether uses the biggest window;
- `limits.spend[]` supports explicit `id`, `mode`, and `anchor` for pacing and burst limits on
  the same budget:
  - `mode: tumbling` uses persisted bucket state and requires `anchor.kind: first_seen`;
  - `mode: rolling` keeps trailing recent-spend behavior for 10s-1h spike guards and must omit `anchor`;
- spend-window ids must be unique within one budget so report output can distinguish, for example,
  a `1d tumbling` pacing limit from a `1d rolling` burst limit;
- if a request does not include `estimated_tokens`, `limits.context_tokens` does not fire and the
  request continues under the rest of policy evaluation;
- `allocation.standard: protected_adoption_pool` parses policy-only adoption-governance inputs:
  `by` (`user` or `team`), `protected_amount_usd`, `window` (`monthly`), and
  `carryover.{percent,cap_usd}`;
- omitted `models.allow` means all provider/model pairs are allowed;
- `project` and `subject` request fields are still treated as entity sources when explicit
  `project:*` or `user:*` entities are absent;
- estimated cost uses `estimated_cost_usd` when present;
- otherwise estimated cost falls back to `estimated_tokens * 0.000001`;
- `allow` creates a reservation;
- `warn` creates a reservation and includes an explanation;
- `deny` does not create a reservation.

Reservations are finalized through `POST /v1/reservations/{id}/finalize`.

## Hybrid pacing example

See `examples/scenarios/hybrid-budget-pacing-windows.noet.yaml` for a runnable end-to-end example
that demonstrates:

- a `30d` tumbling spend cap;
- a `1d` tumbling pacing limit deny;
- a `5m` rolling spike-guard limit deny.
