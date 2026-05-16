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
  specificity: [project, user, team, group, org, global]
budgets:
  - id: dev-daily
    limit_usd: 1.00
    priority: 0
    warn_at_fraction: 0.8
    window_seconds: 86400
    eligible:
      entities: [project:noether]
    models:
      allow:
        - openai:gpt-4.1
        - anthropic:claude-sonnet-*
    guards:
      max_estimated_request_cost_usd:
        max_usd: 0.50
        effect: warn
      max_context_tokens:
        max_tokens: 120000
        effect: deny
    allocation:
      standard: protected_adoption_pool
      by: user
      protected_amount_usd: 25
      window: monthly
      carryover:
        percent: 10
        cap_usd: 50
    match:
      project: noether
policies:
  - id: require-project
    effect: deny
    reason: project is required for budget attribution
    when:
      missing: project
```

## Budget behavior

The v0 evaluator is an in-memory fixed-window budget:

- routing defaults to `explicit_then_fallback`;
- when `budget_id` is present, Noether tries that budget first and records why it was rejected
  before falling back;
- inferred fallback budgets sort by entity specificity, higher `priority`, lower projected budget
  pressure, and stable budget id;
- `eligible.entities` can match trusted request entities such as `project:noether`,
  `user:alice`, `team:core`, `org:example`, or `global`;
- `models.allow` constrains a matching budget to provider/model patterns such as
  `openai:gpt-4.1` or wildcard suffixes such as `anthropic:claude-sonnet-*`;
- `guards.max_estimated_request_cost_usd` can warn or deny when one request's estimated cost
  exceeds a configured per-budget threshold;
- `guards.max_context_tokens` can warn or deny when authorize-time context/input token estimates
  exceed a configured per-budget threshold;
- if a request does not include `estimated_tokens`, `max_context_tokens` does not fire and the
  request continues under the rest of policy evaluation;
- `allocation.standard: protected_adoption_pool` parses policy-only adoption-governance inputs:
  `by` (`user` or `team`), `protected_amount_usd`, `window` (`monthly`), and
  `carryover.{percent,cap_usd}`;
- omitted `models.allow` means all provider/model pairs are allowed;
- when `eligible.entities` is omitted, legacy matching rules compare optional `subject`, `project`,
  `provider`, and `model`;
- `project` and `subject` fields are also treated as legacy entity sources for compatibility;
- estimated cost uses `estimated_cost_usd` when present;
- otherwise estimated cost falls back to `estimated_tokens * 0.000001`;
- `allow` creates a reservation;
- `warn` creates a reservation and includes an explanation;
- `deny` does not create a reservation.

Reservations are finalized through `POST /v1/reservations/{id}/finalize`.
