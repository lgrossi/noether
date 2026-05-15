# Pi subscription-backed findings

## Executive answer

For the validated Pi 0.74.0 setup on 2026-05-14:

1. **Can subscription-backed Pi traffic be routed through Noether?** Yes, for the tested `openai-codex/gpt-5.5` subscription provider, by overriding the built-in provider `baseUrl` so Pi sends Codex Responses traffic to `noet`.
2. **Can it be pre-authorized?** Yes. The primary path is now the Noether Pi extension: it calls Noether in `before_provider_request` and calls `ctx.abort()` on deny before Pi sends the provider request. Route-through remains a fallback when Noether intentionally sits on the HTTP path.
3. **Can it be observed after the fact?** Yes. Pi session JSONL records model/provider, user message, assistant message, usage, cost, stop reason, and response id. Pi extensions can also capture provider payload summaries and response metadata during the call.

## Validated normal subscription-backed behavior

The normal provider/model from Pi settings was:

```text
openai-codex/gpt-5.5
```

Stream 1 ran a tiny prompt with normal subscription credentials through Pi's normal provider path plus a local probe extension:

```bash
pi --model openai-codex/gpt-5.5 \
  --thinking off \
  --no-tools \
  --session-dir .noet/fixtures/pi-stream1-20260514/subscription-sessions \
  --extension .noet/fixtures/pi-stream1-20260514/pi-probe.ts \
  --no-skills \
  --no-prompt-templates \
  --no-context-files \
  -p \
  --system-prompt 'You are concise.' \
  'Reply with exactly: pi-subscription-ok'
```

Observed output:

```text
pi-subscription-ok
```

The extension observed a provider payload summary before the HTTP request. It contained:

- keys: `include`, `input`, `instructions`, `model`, `parallel_tool_calls`, `prompt_cache_key`, `reasoning`, `store`, `stream`, `text`, `tool_choice`;
- `model`: `gpt-5.5`;
- `stream`: `true`;
- `inputCount`: `1`;
- `reasoning`: `{ "effort": "high", "summary": "auto" }`.

Although the command passed `--thinking off`, the session file recorded a later Pi thinking-level change to `high`. That appears to come from the chosen model/settings path. This should be reviewed before relying on command-line thinking flags for spend control.

## Session artifacts

The local session file was written under:

```text
.noet/fixtures/pi-stream1-20260514/subscription-sessions/
```

The JSONL session showed:

- session header with cwd;
- model change to `openai-codex/gpt-5.5`;
- thinking-level changes;
- user message;
- assistant message with provider `openai-codex`, model `gpt-5.5`, token usage, cost, stop reason, and response id.

This is sufficient for after-the-fact usage ingestion, subject to privacy rules for prompt/response bodies.

## Route-through subscription probe

A separate isolated Pi agent directory copied existing credentials and added:

```json
{
  "providers": {
    "openai-codex": {
      "baseUrl": "http://127.0.0.1:4040"
    }
  }
}
```

That override preserved built-in `openai-codex` models. A tiny prompt then hit Noether and produced a local fixture for:

```text
POST /codex/responses
```

Important observed headers/body facts:

- authorization was present and redacted by Noether;
- Pi sent a subscription/account header;
- Pi sent `originator: pi`;
- body model was `gpt-5.5`;
- body shape matched Codex Responses rather than OpenAI Chat Completions.

The current Noether mock returned Chat Completions SSE, so the route-through probe confirmed capture but not a working subscription-backed model response.

## Findings by capability

### Route

`openai-codex` subscription traffic can be rerouted by Pi provider config because Pi applies `baseUrl` overrides to built-in providers while preserving built-in models and existing OAuth auth.

This should be done through a documented template or extension for real usage. Stream 1 did not edit the real global `~/.pi/agent/models.json`.

### Pre-authorize

Noether can pre-authorize if Pi sends traffic to Noether first. The natural enforcement point is inside `noet` before mock/upstream forwarding.

Noether can pre-authorize ordinary built-in subscription traffic that goes directly from Pi to the provider when the Noether Pi extension is enabled. The extension enforces policy in `before_provider_request` and calls `ctx.abort()` on deny before Pi sends the provider request. This keeps Pi's transport/auth path intact, so the regression proof should stay tied to Pi hook behavior.

### Observe

Noether can observe subscription-backed usage after the fact by ingesting:

- Pi session JSONL files;
- Pi extension event logs;
- optional local Noether fixtures when traffic is routed through Noether.

Session ingestion gives provider/model/usage/cost/message metadata after completion. Extension hooks can capture provider payload summaries before the request and response headers/status after the HTTP response is received.

## Gaps

- Noether does not yet emit an OpenAI Codex Responses-compatible streaming response.
- Noether does not yet have a real upstream-forwarding template for `openai-codex` subscription traffic.
- Noether does not yet have a bodyless or sanitized session ingester for Pi JSONL sessions.
- Extension hook enforcement needs a dedicated test before treating it as a hard policy gate.
