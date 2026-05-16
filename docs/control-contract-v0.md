# Control contract v0

Noether's control contract is provider-neutral. Provider request/response correctness stays at adapter and capture edges.

## Decision API

### `POST /v1/authorize`

Request:

```json
{
  "subject": "user:alice",
  "project": "noether",
  "provider": "openai",
  "model": "gpt-example",
  "estimated_tokens": 12000,
  "estimated_cost_usd": 0.024,
  "metadata": { "session": "local-dev" }
}
```

Response:

```json
{
  "decision_id": "decision-id",
  "outcome": "warn",
  "reservation": {
    "id": "reservation-id",
    "amount_usd": 0.024,
    "currency": "USD",
    "status": "active",
    "created_at": "2026-05-15T00:00:00Z",
    "expires_at": "2026-05-15T01:00:00Z"
  },
  "explanations": [
    {
      "rule_id": "dev-daily",
      "reason": "estimated cost reaches warning threshold",
      "severity": "warn"
    }
  ],
  "created_at": "2026-05-15T00:00:00Z"
}
```

`outcome` is `allow`, `warn`, or `deny`. `deny` responses do not include a reservation.

### `POST /v1/reservations/{id}/finalize`

Request:

```json
{
  "actual_cost_usd": 0.019,
  "usage": {
    "provider": "openai",
    "model": "gpt-example",
    "input_tokens": 8000,
    "output_tokens": 1200,
    "total_tokens": 9200,
    "cost_usd": 0.019,
    "latency_ms": 2100,
    "stop_reason": "stop"
  }
}
```

Finalization is idempotent in local memory: repeating the same finalize call returns the already finalized reservation.

### `POST /v1/events`

Request:

```json
{
  "trace_id": "trace-id",
  "kind": "request.completed",
  "occurred_at": "2026-05-15T00:00:00Z",
  "payload": {
    "reservation_id": "reservation-id",
    "status": "ok"
  }
}
```

The endpoint currently accepts and stores events in memory.

## Supporting event types

The v0 Rust contract also includes `UsageObservation`, `TraceEvent`, `ToolEvent`, and `EvalAnnotation` so adapters can report usage, tool execution, and evaluation outcomes without encoding provider-specific protocol details into Noether core.
