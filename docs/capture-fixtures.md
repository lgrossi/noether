# Capture fixture schema v1

Schema id: `noether.capture.v1`

Capture fixtures are local JSON files written by `noet serve` under `.noet/fixtures` by default. They preserve the existing capture-spike behavior: request and response bodies are retained so real harness/provider shapes can be studied.

## Privacy and redaction

Noether redacts:

- secret-like headers such as `authorization`, `cookie`, `x-api-key`, and token/secret/credential headers;
- recursive JSON object keys such as `api_key`, `apiKey`, `token`, `access_token`, `refresh_token`, `authorization`, `password`, `secret`, and `cookie`.

Prompt and response text are not automatically removed. Body retention is explicit local capture behavior and should not be treated as a central deployment default.

## Shape

```json
{
  "schema": "noether.capture.v1",
  "trace_id": "trace-id",
  "captured_at": "2026-05-15T00:00:00Z",
  "request": {
    "method": "POST",
    "path": "/v1/chat/completions",
    "headers": { "authorization": "<redacted>" },
    "body": { "kind": "json", "value": { "model": "example" } }
  },
  "response": {
    "source": "mock",
    "status": 200,
    "headers": { "content-type": "application/json" },
    "body": { "kind": "json", "value": { "id": "chatcmpl-example" } },
    "chunks": [{ "index": 0, "bytes": 42, "text": "{\"id\":\"chatcmpl-example\"}" }],
    "error": null
  },
  "decision": {
    "mode": "dry_run",
    "decision": {
      "decision_id": "decision-id",
      "outcome": "allow",
      "reservation": {
        "id": "reservation-id",
        "amount_usd": 0.001,
        "currency": "USD",
        "status": "active",
        "created_at": "2026-05-15T00:00:00Z",
        "expires_at": "2026-05-15T01:00:00Z"
      },
      "explanations": [],
      "created_at": "2026-05-15T00:00:00Z"
    }
  }
}
```

`decision` is present only when the server was started with `--policy`.

`response.error` is omitted when no streaming error occurred. For progressive upstream streams,
`response.body` is a bounded byte-count summary, `response.chunks` stores the first chunk previews,
and `response.error` records an upstream stream failure or client disconnect when one occurs.

## Inspection commands

```text
noet fixtures list .noet/fixtures
noet fixtures show .noet/fixtures/<trace-id>.json
noet fixtures redact-check .noet/fixtures/<trace-id>.json
```
