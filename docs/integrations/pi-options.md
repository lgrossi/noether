# Pi integration options

## Recommendation

Use two tracks:

1. **Normal Pi extension for subscription-backed usage.** This is the primary path because Pi keeps auth/routing/request shaping/streaming, while Noether authorizes before provider send and ingests usage afterward. The user controls enabling/disabling the extension through Pi.
2. **Custom/local provider route for deterministic capture and policy development.** Keep `examples/pi/models.noether.json` as the safe starter path.

Transparent subscription route-through and launcher wrappers are fallback/debug paths. Add more route-through only after Noether explicitly needs to sit on the HTTP path and can forward provider-specific streams correctly.

## Option matrix

| Option | What it gives Noether | Enforcement strength | Implementation cost | Notes |
| --- | --- | --- | --- | --- |
| Normal Pi extension package | Pre-send authorization and post-response/session usage without provider forwarding | Strong when enabled; deny uses `ctx.abort()` | Medium | Primary path for subscription-backed Pi usage. See [`pi-extension.md`](./pi-extension.md). |
| Custom provider to `noet` | Full local request/response capture for OpenAI-compatible traffic | Strong when `noet` is hot path | Low | Validated with `noether/noether-mock`. Does not use subscription credentials. |
| Built-in provider `baseUrl` override | Subscription-backed traffic reaches `noet` first | Strong if Noether forwards or blocks | Medium | Fallback for capture/route-through; needs provider stream compatibility for end-to-end use. |
| Session-log ingestion | Provider/model/usage/cost/messages after completion | None | Low | Useful observability complement. Requires privacy controls. |
| CLI wrapper/debug launcher | Can select model, inject env/settings/session-dir, run preflight checks | Weak alone | Low/Medium | Optional debug helper only; not the recommended personal setup. |
| Prompt prelude injection | Adds attribution/policy context to prompts | Weak | Low | Useful for metadata and nudges. Not a budget gate and may affect model behavior. |
| SDK embedding | Direct access to Pi sessions, auth, model registry, and events | Strong in a controlled app | High | Good for a Noether-managed Pi runner; less suitable for transparent control of normal user Pi. |
| Local transport only for custom providers | Full Noether ownership of supported protocols | Strong | Medium | Cleanest production control mode for API-key/local providers. Does not automatically cover subscription providers. |

## Option details

### 1. Normal Pi extension package

Install or enable [`extensions/pi-noether`](../../extensions/pi-noether) through Pi's extension mechanism:

```json
{
  "packages": [
    "/absolute/path/to/noether/extensions/pi-noether"
  ]
}
```

Pros:

- Pi keeps subscription auth, routing, provider payload construction, and streaming;
- Noether authorizes in `before_provider_request`;
- `deny` calls `ctx.abort()` before provider send;
- usage/status/session observations are reported after response parsing;
- default authorization metadata is bodyless;
- users retain normal Pi control over whether the extension is enabled.

Cons:

- only protects Pi runs where the extension is enabled;
- `ctx.abort()` behavior is Pi-version-sensitive and needs regression proof;
- requires a running local Noether server.

### 2. Custom provider

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

### 3. Built-in provider override

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
- can become the strongest enforcement mode when Noether intentionally owns the HTTP path.

Cons:

- current Noether mock is not Codex Responses-compatible;
- proxy forwarding must preserve provider-specific auth headers and streaming protocol;
- not the primary personal setup because it turns Noether into a provider forwarding component.

### 4. Session-log ingestion

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

### 5. CLI wrapper/debug launcher

A launcher can still be useful for repeatable proof runs: set temporary environment variables, point Pi at an extension path for one invocation, or isolate sessions under `.noet/`. It should not be the primary adoption path.

Pros:

- easy to test;
- avoids editing global Pi config during proof runs;
- good for repeatable local validation.

Cons:

- bypassable if the user runs normal `pi`;
- cannot enforce per-request budgets unless the extension is enabled;
- adds product surface that is unnecessary for the personal setup.

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

1. Keep the extension deny proof running against Pi upgrades.
2. Add a sanitized Pi session ingester for metadata-only usage.
3. Add a Codex Responses fixture test from the captured `/codex/responses` shape.
4. Keep transparent route-through as an explicit fallback/debug path, not the primary Pi integration.
