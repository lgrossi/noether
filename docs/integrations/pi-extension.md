# Pi extension integration

Date: 2026-05-15

## Architecture

For Pi subscription-backed usage, Noether does not translate provider protocols or forward provider streams. Pi continues to own provider authentication, provider/model routing, provider-specific request shaping, transport/streaming, response parsing, and session persistence.

The primary integration is a normal Pi extension installed and enabled through Pi's extension/package mechanism:

```text
Pi normal extension
        |
        | before_provider_request
        v
 POST /v1/authorize on local Noether
        |
 allow/warn continues, deny calls ctx.abort()
        |
 message_end/turn_end/agent_end/tool hooks
        v
 enqueue async POST /v1/events
 enqueue async POST /v1/reservations/{id}/finalize
```

The extension package lives at [`extensions/pi-noether`](../../extensions/pi-noether). It is commit-safe: no credentials, no real Pi config, and no captured prompts are stored in the repository.

The stable sidecar API is documented by Noether's OpenAPI endpoint at `/openapi.json` and the
human docs at `/docs`. The extension payloads are tested against that API shape. The extension keeps
its small hot-path HTTP calls local instead of importing the TypeScript SDK package because Pi loads
extensions directly and the authorization hook must stay packaging-light and latency-bound. Other
TypeScript integrations should prefer the SDK under [`sdk/typescript`](../../sdk/typescript).

## Hook flow and emitted payloads

Noether currently uses seven normal-path Pi hooks:

```text
before_agent_start
  └─ cache bodyless agent context: selected tools, skills, context files

before_provider_request
  ├─ build bodyless authorization metadata + cached agent context
  ├─ POST /v1/authorize with a strict timeout
  ├─ enqueue POST /v1/events kind=pi.agent_context
  └─ deny => ctx.abort(); allow/warn => continue

tool_call
  └─ enqueue POST /v1/events kind=pi.tool_call

tool_result
  └─ enqueue POST /v1/events kind=tool.observed

message_end
  ├─ enqueue POST /v1/events kind=pi.message_end
  └─ enqueue POST /v1/reservations/{id}/finalize

turn_end
  └─ enqueue POST /v1/events kind=pi.turn_end

agent_end
  └─ enqueue POST /v1/events kind=pi.agent_end
```

Only `before_provider_request` waits for Noether, because it is the authorization point before Pi
sends the provider request. All lifecycle reporting, reservation finalization, and debug logging is
queued and delivered best-effort so Noether persistence cannot degrade Pi interaction latency.
Queued deliveries use bounded retries and bounded timeouts; failures are surfaced as
`pi.authorize_error`, `pi.reservation_finalize_error`, or `pi.delivery_error` when the extension can
still reach Noether.

### `before_agent_start`

Pi exposes the prompt startup context before the agent loop. Noether does not send the prompt text,
but it records the workflow shape: active tools, loaded skills, context files, and cwd. This cached
context is attached to the next authorization request and emitted as `pi.agent_context`.

Example Pi-side input:

```json
{
  "prompt": "not sent to Noether by default",
  "systemPromptOptions": {
    "selectedTools": ["read", "bash", "edit"],
    "skills": [{ "name": "diagnose" }, { "name": "tdd" }],
    "contextFiles": [{ "path": "AGENTS.md" }],
    "cwd": "/repo"
  }
}
```

Noether event:

```json
{
  "trace_id": "generated-trace-id",
  "kind": "pi.agent_context",
  "payload": {
    "source": "noether-pi",
    "decision_id": "decision-id",
    "reservation_id": "reservation-id",
    "selected_tools": ["read", "bash", "edit"],
    "skills": ["diagnose", "tdd"],
    "context_files": ["AGENTS.md"],
    "cwd": "/repo",
    "prompt": { "type": "string", "length": 30 }
  }
}
```

### `before_provider_request`

Pi provides the provider-shaped payload and context immediately before transport. The payload may
contain prompt-like fields, so Noether summarizes it by default instead of sending bodies.

Example Pi-side inputs:

```json
{
  "event": {
    "payload": {
      "model": "gpt-5.5",
      "input": [{ "role": "user", "content": "not sent to Noether by default" }],
      "instructions": "not sent to Noether by default",
      "stream": true
    }
  },
  "ctx": {
    "cwd": "/repo",
    "model": {
      "provider": "openai-codex",
      "id": "gpt-5.5",
      "api": "openai-codex-responses"
    },
    "context_usage": {
      "tokens": 1234,
      "contextWindow": 128000,
      "percent": 0.96
    }
  }
}
```

Noether authorization request:

```json
{
  "budget_id": "project-noether",
  "entities": ["project:noether", "user:demo"],
  "subject": "user:demo",
  "project": "noether",
  "provider": "openai-codex",
  "model": "gpt-5.5",
  "estimated_tokens": 1234,
  "metadata": {
    "harness": "pi",
    "extension": "noether-pi",
    "extension_version": "dev",
    "trace_id": "generated-trace-id",
    "request_id": "generated-request-id",
    "cwd": "/repo",
    "model_api": "openai-codex-responses",
    "request_surface": "responses",
    "payload_kind": "object",
    "payload_keys": ["input", "instructions", "model", "stream"],
    "payload_summary": {
      "input": { "type": "array", "length": 1 },
      "instructions": { "type": "string", "length": 31 },
      "model": { "type": "string", "length": 7 },
      "stream": true
    },
    "agent_context": {
      "selected_tools": ["read", "bash", "edit"],
      "skills": ["diagnose", "tdd"],
      "context_files": ["AGENTS.md"]
    },
    "context_window": 128000,
    "context_usage_percent": 0.96
  }
}
```

If Noether returns `deny`, the extension calls `ctx.abort()`. Throwing an error from the hook is not
used as a deny path because Pi catches extension errors and continues.

### Response-level signal

The normal extension path does not use `after_provider_response`. Real Pi hook observations showed
that it may not fire for the active provider path, while finalized usage and assistant content are
available later from `message_end` and turn lifecycle hooks. Response-level persistence therefore
starts from `message_end` and is delivered asynchronously.

### `tool_call` and `tool_result`

`tool_call` fires before Pi executes a tool. Noether records the tool name, tool call id, and a
shape-only input summary. It does not send command text, file contents, or tool arguments verbatim.

```json
{
  "trace_id": "generated-trace-id",
  "kind": "pi.tool_call",
  "payload": {
    "source": "noether-pi",
    "decision_id": "decision-id",
    "reservation_id": "reservation-id",
    "tool_name": "bash",
    "tool_call_id": "toolu_123",
    "input_summary": {
      "command": { "type": "string", "length": 42 },
      "timeout": 1000
    }
  }
}
```

`tool_result` fires after execution. Noether emits the generic `tool.observed` event family so the
result appears in `noet report observations --kind tool`.

```json
{
  "trace_id": "generated-trace-id",
  "kind": "tool.observed",
  "payload": {
    "source": "noether-pi",
    "decision_id": "decision-id",
    "reservation_id": "reservation-id",
    "name": "bash",
    "duration_ms": 240,
    "success": true,
    "metadata": {
      "tool_call_id": "toolu_123",
      "input_summary": {
        "command": { "type": "string", "length": 42 }
      },
      "content_summary": { "type": "array", "length": 1 },
      "details_summary": {
        "exitCode": 0
      }
    }
  }
}
```

### `message_end`

Pi exposes the parsed assistant message after streaming completes. Noether extracts usage from the
assistant message, emits a timeline event, and finalizes the reservation once.

Example usage event:

```json
{
  "trace_id": "generated-trace-id",
  "kind": "pi.message_end",
  "payload": {
    "source": "noether-pi",
    "decision_id": "decision-id",
    "reservation_id": "reservation-id",
    "usage": {
      "provider": "openai-codex",
      "model": "gpt-5.5",
      "input_tokens": 900,
      "output_tokens": 180,
      "total_tokens": 1080,
      "cost_usd": 0.0019,
      "stop_reason": "stop"
    }
  }
}
```

Finalize payload:

```json
{
  "reservation_id": "reservation-id",
  "actual_cost_usd": 0.0019,
  "usage": {
    "provider": "openai-codex",
    "model": "gpt-5.5",
    "input_tokens": 900,
    "output_tokens": 180,
    "total_tokens": 1080,
    "cost_usd": 0.0019,
    "stop_reason": "stop"
  },
  "metadata": {
    "trace_id": "generated-trace-id",
    "request_id": "generated-request-id",
    "source": "noether-pi"
  }
}
```

### `turn_end` and `agent_end`

`turn_end` records turn-level lifecycle metadata and any usage that can be extracted from the final
turn message:

```json
{
  "trace_id": "generated-trace-id",
  "kind": "pi.turn_end",
  "payload": {
    "source": "noether-pi",
    "decision_id": "decision-id",
    "reservation_id": "reservation-id",
    "turn_index": 0,
    "usage": {
      "total_tokens": 1080,
      "cost_usd": 0.0019
    }
  }
}
```

`agent_end` records coarse lifecycle completion:

```json
{
  "trace_id": "generated-trace-id",
  "kind": "pi.agent_end",
  "payload": {
    "source": "noether-pi",
    "decision_id": "decision-id",
    "reservation_id": "reservation-id",
    "message_count": 4
  }
}
```

The extension does not currently use `context`, `message_start`, `message_update`,
`tool_execution_start`, `tool_execution_update`, `tool_execution_end`, `turn_start`, or
`agent_start`.

## Installation and enabling

Pi auto-discovers extensions from:

- `~/.pi/agent/extensions/*.ts`
- `~/.pi/agent/extensions/*/index.ts`
- `.pi/extensions/*.ts`
- `.pi/extensions/*/index.ts`

For local development, either copy/symlink the package into one of those locations or add it to Pi settings as a local package:

```json
{
  "packages": [
    "/absolute/path/to/noether/extensions/pi-noether"
  ]
}
```

Alternatively, point Pi's `extensions` setting directly at the TypeScript entrypoint:

```json
{
  "extensions": [
    "/absolute/path/to/noether/extensions/pi-noether/src/index.ts"
  ]
}
```

Pi settings paths are user-controlled. Noether does not edit `~/.pi/agent/settings.json`, `models.json`, or `auth.json`.

For an ad hoc run, Pi also supports:

```bash
pi --extension "$PWD/extensions/pi-noether"
```

This is useful for local proofing, but the recommended personal setup is the normal Pi extension mechanism so the user can enable or disable Noether in Pi.

## Runtime configuration

Run local Noether:

```bash
noet up
```

For source checkouts, use `cargo run --bin noet -- up`. The extension's local auto-start path uses
`noet up --root ... --bind ...` and writes owner/lease state under `.noet/pi-sidecar`. Existing
`.noether/pi-sidecar` owner files are still read and cleared for compatibility.

Configure the extension with environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `NOET_URL` | `http://127.0.0.1:4051` | Local Noether API URL. |
| `NOET_PI_PROJECT` | unset | Project metadata sent to `/v1/authorize`. |
| `NOET_PI_SUBJECT` | unset | Subject/user metadata sent to `/v1/authorize`. |
| `NOET_PI_BUDGET_ID` | unset | Explicit budget id sent to `/v1/authorize`. |
| `NOET_PI_ENTITIES` | unset | Comma-separated trusted entities such as `project:noether,user:local`. |
| `NOET_PI_FAIL_MODE` | `fail_open` | Use `fail_closed` to abort provider sends when Noether is unavailable. |
| `NOET_PI_POLICY_MODE` | `enforce` | Deny handling for Noether policy decisions: `enforce` aborts, `user_approved` prompts the user to continue or block, `warn` continues with a user-visible warning, `monitor` continues with report-only handling. |
| `NOET_PI_INCLUDE_BODY` | unset | Set to `1` or `true` only to include sanitized body-shaped metadata. |
| `NOET_PI_EXTENSION_VERSION` | `dev` | Version metadata for events/authorization. |
| `NOET_PI_AUTHORIZE_TIMEOUT_MS` | `1000` | Maximum time the hot-path authorization hook waits for Noether. |
| `NOET_PI_AUTO_START_LOCAL` | auto | When `NOET_URL=http://127.0.0.1:4051`, ensure the local sidecar is running from `session_start`, and stop it on `session_shutdown` only after the last active Pi session releases it. |
| `NOET_PI_LOCAL_BIN` | auto | Binary used for `noet up`; defaults to `target/debug/noet` under `NOET_PI_LOCAL_ROOT`/cwd when present, otherwise `noet` on `PATH`. |
| `NOET_PI_LOCAL_ROOT` | user home | Root path passed to `noet up --root ...` when local auto-start is used. Defaults to the same managed local home that `noet config init` uses. |
| `NOET_PI_LOCAL_START_TIMEOUT_MS` | `3000` | How long the extension waits for the auto-started local sidecar to become healthy. |
| `NOET_PI_QUEUE_MAX_ITEMS` | `100` | Bound applied to both concurrent async deliveries and queued backlog for events, finalization, and debug logs. |
| `NOET_PI_DEBUG_HOOKS` | unset | Set to `raw` to enable local raw hook dump mode. |
| `NOET_PI_DEBUG_HOOK_LOG_DIR` | unset | Directory for raw debug hook JSONL files when debug mode is enabled. |

The extension reads persisted JSON config in this precedence order:

1. `~/.pi/agent/noether.json` legacy global config
2. `~/.pi/agent/noet.json` noet-aligned global config
3. `.pi/noether.json` legacy project config
4. `.pi/noet.json` noet-aligned project config

Later files override earlier files, so `.pi/noet.json` can migrate a project without deleting the
legacy `.pi/noether.json`. The JSON format is integration-specific compatibility surface; core
`noet` config remains YAML.

When the extension is pointed at the standard local URL `http://127.0.0.1:4051`, it treats that as
the personal sidecar path and ensures `noet up` is healthy during `session_start`.
The extension keeps a lease for the life of the Pi session and stops the managed sidecar on
`session_shutdown` only when no other active Pi sessions still hold a lease. Remote/shared Noether
URLs are not auto-started.

Queued event/finalization delivery currently uses internal bounded retries with short backoff and a
bounded per-attempt timeout. Those values are intentionally internal for now; the public runtime
knobs are the hot-path authorize timeout and the delivery queue/concurrency bound.

## Raw hook dump mode

For live Pi inspection, explicitly enable raw debug hooks before starting Pi:

```bash
export NOET_PI_DEBUG_HOOKS=raw
export NOET_PI_DEBUG_HOOK_LOG_DIR="$PWD/.noet/pi-hook-logs"
tail -f .noet/pi-hook-logs/before_provider_request.raw.jsonl
tail -f .noet/pi-hook-logs/message_end.raw.jsonl
```

When enabled, the extension appends one JSON object per hook call:

- `before_provider_request.raw.jsonl`: raw Pi hook `event`, raw hook `ctx` after JSON-safe serialization, generated `trace_id` / `request_id`, and the Noether authorization request built from that hook.
- `message_update.raw.jsonl`, `message_end.raw.jsonl`, `turn_end.raw.jsonl`, and `agent_end.raw.jsonl`: raw lifecycle hook payloads when those hooks fire.

This mode is intentionally separate from normal Noether event ingestion. It is for discovering what
Pi actually exposes during a real conversation. It may include prompt/provider payload data and should
only be used locally with logs you are willing to inspect and delete.

## Privacy posture

The extension is bodyless by default. Its authorization request includes configured `subject` and `project`, or an OS-derived local subject plus cwd-derived project when those helpers are enabled, provider/model from Pi context when available, estimated context tokens from `ctx.getContextUsage()` when available, and sanitized metadata such as cwd, model API, request surface (`responses` / `chat` / `messages` when inferable), payload type, top-level payload keys, and shape summaries.

It does **not** send prompt/body content by default. Prompt-like keys such as `messages`, `input`, `instructions`, `prompt`, and `system` are summarized by type/length only.

Body inclusion exists only as an explicit escape hatch:

```bash
NOET_PI_INCLUDE_BODY=1 pi
```

Do not use that mode with real prompts unless the receiving Noether endpoint and retention policy are appropriate.

## Deny behavior

The extension runs on `before_provider_request`, calls Noether asynchronously, and handles outcomes as follows:

- `allow`: return normally and Pi sends the provider request;
- `warn`: return normally and Pi sends the provider request;
- `deny` + `policyMode=enforce`: show the deny reason and call `ctx.abort()` before Pi sends the provider request;
- `deny` + `policyMode=user_approved`: show the deny reason in Pi's confirm dialog, ask whether to proceed anyway, continue only after explicit approval, and otherwise abort;
- `deny` + `policyMode=warn`: show the deny reason as a warning and still let Pi send the provider request;
- `deny` + `policyMode=monitor`: record/report the deny reason without warning or blocking.

When Pi UI confirmation is unavailable, `policyMode=user_approved` blocks the request and reports that approval could not be collected.

Pi extension errors are not a denial mechanism; Pi catches thrown handler errors and continues. The hard-deny mechanism for this integration is `ctx.abort()`, as validated in [`docs/integrations/pi-wrapper-research.md`](./pi-wrapper-research.md).

Noether unavailability is configurable:

- default: `fail_open` so local development does not break when the sidecar is down;
- strict: `fail_closed` so provider sends are aborted if authorization cannot be obtained.

`failMode` only covers transport failures talking to Noether. `policyMode` only covers real
Noether `deny` decisions.

Current regression coverage covers:

- `allow` continues without abort;
- `deny` aborts before provider send;
- `deny` in `policyMode=user_approved` continues after approval and aborts after rejection;
- authorize timeout in `fail_open` and `fail_closed`;
- immediate sidecar-unavailable failure in `fail_open` and `fail_closed`;
- bounded retries for queued lifecycle delivery;
- surfaced queued delivery failures via `pi.delivery_error`;
- raw hook logs remaining opt-in.

## Local safe proof

The safe proof script uses only local mock Noether and provider servers plus a mock Pi extension lifecycle. It writes no credentials and cannot reach a real provider:

```bash
npm --prefix extensions/pi-noether run proof:deny
```

The script starts a mock Noether endpoint that returns `deny`, registers the Noether extension against a small mock Pi API, emits `before_provider_request`, and only sends to the mock provider if the extension did not abort. It asserts Noether saw one authorization request, the provider saw zero requests, and prompt text was not sent to Noether.

Generated proof files stay under ignored `.noet/`.

## Troubleshooting

### Pi keeps running when Noether is down

That is the default `fail_open` behavior. Set:

```bash
export NOET_PI_FAIL_MODE=fail_closed
export NOET_PI_POLICY_MODE=enforce
```

if you want the provider send aborted when Noether cannot be reached.

If you are using the standard personal setup on `http://127.0.0.1:4051`, the extension now tries to
start `noet up` during `session_start` before any provider traffic, then applies
`fail_open` or `fail_closed` normally if the sidecar still cannot be reached.

### Pi stalls before provider send

Check the Noether authorize timeout:

```bash
export NOET_PI_AUTHORIZE_TIMEOUT_MS=1000
```

Lower it for stricter hot-path bounds or raise it only if the local sidecar is expected to be
slow.

### Lifecycle events or finalize calls seem to be missing

They are queued asynchronously after authorization. `NOET_PI_QUEUE_MAX_ITEMS` caps both active
deliveries and queued backlog, so a busy run can still drop best-effort lifecycle work when either
bound is saturated. Check:

- `NOET_PI_QUEUE_MAX_ITEMS` if the run is very event-heavy;
- Noether-side `pi.authorize_error`, `pi.reservation_finalize_error`, or `pi.delivery_error`
  observations;
- local debug hook logs only if you explicitly enabled `NOET_PI_DEBUG_HOOKS=raw`.

### Raw hook logs did not appear

That is expected unless you explicitly opt in:

```bash
export NOET_PI_DEBUG_HOOKS=raw
export NOET_PI_DEBUG_HOOK_LOG_DIR="$PWD/.noet/pi-hook-logs"
```

## Known limits

- The extension uses Pi 0.74.0 hook behavior. `ctx.abort()` should remain under regression proof because `before_provider_request` is not documented as a dedicated policy API.
- The normal path does not rely on `after_provider_response`; usage is reported from assistant messages in `message_end`/`turn_end`.
- User-controlled extension enablement means plain Pi runs without the extension are intentionally outside Noether's local personal setup.
- Usage/reservation matching is sequential and prototype-level. Parallel provider sends would need stronger correlation if Pi adds them.
