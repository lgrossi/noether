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
budgets:
  - id: dev-daily
    limit_usd: 1.00
    warn_at_fraction: 0.8
    window_seconds: 86400
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

- matching rules compare optional `subject`, `project`, `provider`, and `model`;
- estimated cost uses `estimated_cost_usd` when present;
- otherwise estimated cost falls back to `estimated_tokens * 0.000001`;
- `allow` creates a reservation;
- `warn` creates a reservation and includes an explanation;
- `deny` does not create a reservation.

Reservations are finalized through `POST /v1/reservations/{id}/finalize`.
