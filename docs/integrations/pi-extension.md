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
 after_provider_response/message_end/turn_end/agent_end
        v
 POST /v1/events
 POST /v1/reservations/{id}/finalize
```

The extension package lives at [`extensions/pi-noether`](../../extensions/pi-noether). It is commit-safe: no credentials, no real Pi config, and no captured prompts are stored in the repository.

## Hook flow and emitted payloads

Noether currently uses eight Pi hooks:

```text
before_agent_start
  └─ cache bodyless agent context: selected tools, skills, context files

before_provider_request
  ├─ build bodyless authorization metadata + cached agent context
  ├─ POST /v1/authorize
  ├─ POST /v1/events kind=pi.agent_context
  └─ deny => ctx.abort(); allow/warn => continue

after_provider_response
  └─ POST /v1/events kind=pi.provider_response

tool_call
  └─ POST /v1/events kind=pi.tool_call

tool_result
  └─ POST /v1/events kind=tool.observed

message_end
  ├─ POST /v1/events kind=pi.message_end
  └─ POST /v1/reservations/{id}/finalize

turn_end
  └─ POST /v1/events kind=pi.turn_end

agent_end
  └─ POST /v1/events kind=pi.agent_end
```

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

### `after_provider_response`

Pi exposes response status and headers before stream/body parsing. Noether emits status, sanitized
headers, and elapsed time; usage is not available at this hook.

```json
{
  "trace_id": "generated-trace-id",
  "kind": "pi.provider_response",
  "payload": {
    "source": "noether-pi",
    "decision_id": "decision-id",
    "reservation_id": "reservation-id",
    "status": 200,
    "headers": {
      "content-type": "text/event-stream",
      "authorization": "[redacted]"
    },
    "latency_ms": 1450
  }
}
```

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
cargo run --bin noet -- serve --policy examples/policy.noet.yaml --decision-mode enforce
```

Configure the extension with environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `NOET_URL` | `http://127.0.0.1:4040` | Local Noether API URL. |
| `NOET_PI_PROJECT` | unset | Project metadata sent to `/v1/authorize`. |
| `NOET_PI_SUBJECT` | unset | Subject/user metadata sent to `/v1/authorize`. |
| `NOET_PI_FAIL_MODE` | `fail_open` | Use `fail_closed` to abort provider sends when Noether is unavailable. |
| `NOET_PI_INCLUDE_BODY` | unset | Set to `1` or `true` only to include sanitized body-shaped metadata. |
| `NOET_PI_EXTENSION_VERSION` | `dev` | Version metadata for events/authorization. |

## Privacy posture

The extension is bodyless by default. Its authorization request includes configured `subject` and `project`, provider/model from Pi context when available, estimated context tokens from `ctx.getContextUsage()` when available, and sanitized metadata such as cwd, model API, payload type, top-level payload keys, and shape summaries.

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
- `deny`: call `ctx.abort()` before Pi sends the provider request.

Pi extension errors are not a denial mechanism; Pi catches thrown handler errors and continues. The hard-deny mechanism for this integration is `ctx.abort()`, as validated in [`docs/integrations/pi-wrapper-research.md`](./pi-wrapper-research.md).

Noether unavailability is configurable:

- default: `fail_open` so local development does not break when the sidecar is down;
- strict: `fail_closed` so provider sends are aborted if authorization cannot be obtained.

## Local safe proof

The safe proof script uses only local mock Noether and provider servers plus a mock Pi extension lifecycle. It writes no credentials and cannot reach a real provider:

```bash
npm --prefix extensions/pi-noether run proof:deny
```

The script starts a mock Noether endpoint that returns `deny`, registers the Noether extension against a small mock Pi API, emits `before_provider_request`, and only sends to the mock provider if the extension did not abort. It asserts Noether saw one authorization request, the provider saw zero requests, and prompt text was not sent to Noether.

Generated proof files stay under ignored `.noet/`.

## Known limits

- The extension uses Pi 0.74.0 hook behavior. `ctx.abort()` should remain under regression proof because `before_provider_request` is not documented as a dedicated policy API.
- `after_provider_response` exposes status and headers, not parsed usage. Usage is reported later from assistant messages in `message_end`/`turn_end`.
- User-controlled extension enablement means plain Pi runs without the extension are intentionally outside Noether's local personal setup.
- Usage/reservation matching is sequential and prototype-level. Parallel provider sends would need stronger correlation if Pi adds them.
