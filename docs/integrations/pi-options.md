# Pi integration options

## Recommendation

Use two tracks:

1. **Custom/local provider route for deterministic capture and policy development.** Keep `examples/pi/models.noether.json` as the safe starter path.
2. **Pi session and extension ingestion for subscription-backed usage until Noether supports provider-specific pass-through for OpenAI Codex Responses.**

Add subscription route-through only after Noether can either forward `/codex/responses` correctly or produce a protocol-compatible Codex Responses stream.

## Option matrix

| Option | What it gives Noether | Enforcement strength | Implementation cost | Notes |
| --- | --- | --- | --- | --- |
| Custom provider to `noet` | Full local request/response capture for OpenAI-compatible traffic | Strong when `noet` is hot path | Low | Validated with `noether/noether-mock`. Does not use subscription credentials. |
| Built-in provider `baseUrl` override | Subscription-backed traffic reaches `noet` first | Strong if Noether forwards or blocks | Medium | Validated for capture with `openai-codex`; needs Codex Responses compatibility for end-to-end use. |
| Session-log ingestion | Provider/model/usage/cost/messages after completion | None | Low | Best immediate subscription-backed observability path. Requires privacy controls. |
| Pi extension hooks | Payload summaries, response metadata, system prompt/context visibility | Medium | Medium | `before_provider_request` can inspect/rewrite payloads. Treat as policy advisory until blocking behavior is tested. |
| CLI wrapper | Can select model, inject env/settings/session-dir, run preflight checks | Medium | Low/Medium | Useful for teams that launch Pi through `noet pi ...`; easy to bypass unless adopted as policy. |
| Prompt prelude injection | Adds attribution/policy context to prompts | Weak | Low | Useful for metadata and nudges. Not a budget gate and may affect model behavior. |
| SDK embedding | Direct access to Pi sessions, auth, model registry, and events | Strong in a controlled app | High | Good for a Noether-managed Pi runner; less suitable for transparent control of normal user Pi. |
| Local transport only for custom providers | Full Noether ownership of supported protocols | Strong | Medium | Cleanest production control mode for API-key/local providers. Does not automatically cover subscription providers. |

## Option details

### 1. Custom provider

Use `examples/pi/models.noether.json` or an additive global `models.json` entry:

```json
{
  "providers": {
    "noether": {
      "baseUrl": "http://127.0.0.1:4040/v1",
      "api": "openai-completions",
      "apiKey": "noether-local",
      "models": [
        { "id": "noether-mock" }
      ]
    }
  }
}
```

Pros:

- least invasive;
- no subscription credentials involved;
- works with current `noet` mock response.

Cons:

- validates Noether transport/control, not subscription billing behavior.

### 2. Built-in provider override

Pi supports overriding a built-in provider by setting `baseUrl` while preserving built-in models. For `openai-codex`, the route-through shape is:

```json
{
  "providers": {
    "openai-codex": {
      "baseUrl": "http://127.0.0.1:4040"
    }
  }
}
```

Pros:

- uses the user's subscription-backed provider/model path;
- lets Noether see the request before provider spend;
- can become the strongest enforcement mode.

Cons:

- current Noether mock is not Codex Responses-compatible;
- proxy forwarding must preserve provider-specific auth headers and streaming protocol;
- risky to apply globally without a rollback path.

### 3. Session-log ingestion

Pi session files are JSONL and include model/provider changes, messages, usage, cost, stop reason, and response ids. Noether can ingest them asynchronously.

Recommended first ingestion posture:

- default bodyless ingestion;
- hash or omit prompt/response bodies unless explicitly enabled;
- record source session path, session id, entry ids, provider, model, usage, cost, timestamps, and stop reason.

Pros:

- immediately useful for subscription-backed observability;
- no transport changes;
- no provider protocol emulation.

Cons:

- cannot block spend;
- costs are whatever Pi recorded;
- body privacy needs explicit policy.

### 4. Pi extension hook

Pi extensions expose:

- `before_provider_request` for provider-specific payload inspection/rewrite;
- `after_provider_response` for status/header observation;
- `before_agent_start` and `context` for system prompt/context changes;
- session events for persistence and metadata.

Pros:

- good source of structured pre-request observations;
- can attach Noether attribution without owning provider transport;
- can support warnings or soft gates.

Cons:

- not yet validated as a hard blocking mechanism for provider spend;
- extension code runs in Pi's process with user permissions;
- captured payloads may include sensitive prompt data.

### 5. CLI wrapper

A wrapper can:

- start `noet`;
- set `PI_CODING_AGENT_DIR` or `--session-dir`;
- inject a provider override;
- run preflight budget checks before launching Pi;
- collect session logs after Pi exits.

Pros:

- easy to test;
- avoids editing global Pi config;
- good for repeatable local validation.

Cons:

- bypassable if the user runs `pi` directly;
- cannot enforce per-request budgets unless combined with proxy or extension hooks.

### 6. Prompt prelude injection

This mirrors "rtk"-style prompt prelude patterns: inject a policy/attribution block into Pi prompts or system prompt.

Pros:

- low implementation cost;
- useful for attribution and intent capture.

Cons:

- not security or spend control;
- changes model-visible prompt surface;
- cannot guarantee compliance.

### 7. SDK-managed Pi runner

The Pi SDK can create sessions with explicit auth storage, model registry, settings, extensions, and event subscriptions.

Pros:

- strongest application-level control if Noether owns the runner;
- avoids scraping session files;
- can pair policy checks with the prompt lifecycle.

Cons:

- larger product surface;
- changes how users launch Pi;
- still needs provider-specific stream compatibility for route-through modes.

## Near-term next steps

1. Add a sanitized Pi session ingester for metadata-only usage.
2. Add a Codex Responses fixture test from the captured `/codex/responses` shape.
3. Implement either:
   - upstream pass-through for `/codex/responses`, or
   - a minimal Codex Responses-compatible mock stream.
4. Re-test subscription route-through with the isolated `PI_CODING_AGENT_DIR` before proposing any additive global config.

