# Pi wrapper research for subscription-backed providers

Date: 2026-05-15
Pi package inspected: `@earendil-works/pi-coding-agent` 0.74.0

## Question

Can Noether wrap Pi subscription-backed providers without brittle manual upstream mapping, while Pi keeps provider routing, auth, request shaping, and streaming?

Short answer: yes, for a Pi-specific path, the best evidenced direction is a Pi extension loaded by a Noether wrapper. The extension can run an async Noether authorization call in `before_provider_request` and can hard-stop the turn with `ctx.abort()` before the local proof server receives a provider request. Pi still owns provider auth, routing, payload serialization, HTTP/WebSocket/SSE transport, and stream parsing. Usage and traces can be ingested from Pi session/events after the response.

This is different from Noether's normal proxy scenario. For normal app/proxy integrations, the caller can ask Noether before sending upstream and Noether does not need to intercept HTTP. The Pi-specific value is that Pi subscription mode already owns auth/routing, so Noether should hook Pi first and keep transparent `baseUrl` route-through as a fallback.

## Evidence inspected

Pi docs:

- `/home/lgrossi/.nvm/versions/node/v22.22.2/lib/node_modules/@earendil-works/pi-coding-agent/README.md`
- `docs/extensions.md`
- `docs/sdk.md`
- `docs/models.md`
- `docs/custom-provider.md`
- `docs/session-format.md`
- `docs/settings.md`

Pi examples:

- `examples/extensions/provider-payload.ts`
- `examples/extensions/custom-provider-anthropic/index.ts`
- `examples/extensions/custom-provider-gitlab-duo/index.ts`
- `examples/extensions/custom-footer.ts`
- `examples/sdk/06-extensions.ts`
- `examples/sdk/09-api-keys-and-oauth.ts`
- `examples/sdk/11-sessions.ts`
- `examples/sdk/13-session-runtime.ts`

Installed source:

- `dist/core/sdk.js`
- `dist/core/extensions/runner.js`
- `dist/core/extensions/types.d.ts`
- `dist/core/model-registry.js`
- `dist/cli.js`
- `node_modules/@earendil-works/pi-ai/dist/models.generated.js`
- `node_modules/@earendil-works/pi-ai/dist/models.d.ts`
- `node_modules/@earendil-works/pi-ai/dist/providers/openai-codex-responses.js`

Prior Noether repo docs:

- `docs/integrations/pi.md`
- `docs/integrations/pi-options.md`
- `docs/integrations/pi-decisions.md`
- `docs/integrations/pi-subscription-findings.md`
- `examples/pi/models.noether.json`

## Option A: Pi extension hook path

### What hooks exist?

The extension lifecycle includes:

- `before_agent_start`: after user input, before the agent loop; can inject a session message and modify the system prompt.
- `context`: before each LLM call; can modify the message list.
- `before_provider_request`: after Pi has built the provider-specific payload and immediately before sending.
- `after_provider_response`: after an HTTP response is received and before stream body consumption.
- `message_update`, `message_end`, `turn_end`, and `agent_end`: observe assistant streaming/final messages and usage after Pi parses the provider stream.

Docs describe `before_provider_request` as payload inspection/replacement, not as a dedicated policy API. Source confirms `BeforeProviderRequestEvent` contains only `{ type, payload }`, and `BeforeProviderRequestEventResult = unknown`.

### Can it hard-deny before upstream spend?

Proven locally with no real provider spend.

Ignored proof files were created only under `.noet/proofs/pi-hook-deny/`. The proof used a custom local provider pointing to `http://127.0.0.1:4559/v1` and a local HTTP server that logs any provider request.

Results:

- Throwing from `before_provider_request` does **not** deny. `dist/core/extensions/runner.js` catches handler errors, emits an extension error, and continues with the current payload. The local provider server still received `POST /v1/chat/completions`.
- Calling `ctx.abort()` inside `before_provider_request` did deny. Pi exited with `Request was aborted.`, and the local provider server received zero requests.
- Calling a local async authorization endpoint from `before_provider_request`, receiving `{ "allow": false }`, then calling `ctx.abort()` also denied before provider send. The local authorize server received `POST /authorize`; the local provider server received zero requests.

Proof artifacts left for audit:

- `.noet/proofs/pi-hook-deny/agent/models.json`
- `.noet/proofs/pi-hook-deny/throw-deny.ts`
- `.noet/proofs/pi-hook-deny/abort-deny.ts`
- `.noet/proofs/pi-hook-deny/async-authorize-deny.ts`
- `.noet/proofs/pi-hook-deny/logs/`

### Can it call Noether before the provider request?

Yes. `ExtensionHandler` can return a Promise, `emitBeforeProviderRequest()` awaits each handler in extension order, and the async proof successfully used `fetch("http://127.0.0.1:4560/authorize", { signal: ctx.signal })` before aborting the provider call.

### What metadata is visible?

At the exact `before_provider_request` hook:

- `event.payload`: provider-specific serialized request payload. For OpenAI-compatible local proof this included system/user messages, `model`, `stream`, cache fields, `stream_options`, `store`, and token limit fields. Prior subscription probe for `openai-codex/gpt-5.5` saw Codex Responses payload keys including `include`, `input`, `instructions`, `model`, `reasoning`, `stream`, `text`, and `tool_choice`.
- `ctx.model`: current Pi model object, including provider/model/api/baseUrl according to docs/source.
- `ctx.modelRegistry`: documented access to models and API keys.
- `ctx.sessionManager`: current session entries/branch/file metadata.
- `ctx.cwd`, `ctx.signal`, idle/abort/shutdown helpers.

Not directly present on the event:

- final request headers/auth;
- HTTP status;
- raw response body or stream chunks.

`after_provider_response` exposes HTTP `status` and normalized `headers`. Usage/cost is available after Pi parses the stream via assistant messages (`message_end`, `turn_end`, `agent_end`) and persisted session JSONL, not from `after_provider_response` itself.

### Does it preserve streaming?

Yes for allowed requests. The extension does not replace transport; Pi still calls its provider implementation and consumes provider SSE/WebSocket streams. `after_provider_response` fires before stream consumption, while `message_update` observes parsed streaming assistant events.

### Extension-path conclusion

This is the strongest fit for the product direction. Noether can be a control layer for Pi subscription mode without becoming a provider translation layer. The hard-deny mechanism is `ctx.abort()`, not throw/return. That is a practical but slightly indirect contract, so the integration should treat it as Pi-version-sensitive and keep a regression proof.

## Option B: Pi SDK / runner path

The SDK can create sessions with Pi's own `AuthStorage`, `ModelRegistry`, `SettingsManager`, `SessionManager`, and `DefaultResourceLoader`. Defaults use Pi's normal `~/.pi/agent/auth.json` and `models.json`; custom paths are supported. SDK examples show in-memory sessions, custom auth/model stores, extension loading, and runtime session replacement.

Useful APIs:

- `createAgentSession()` / `createAgentSessionRuntime()`;
- `AuthStorage.create()` and `ModelRegistry.create(authStorage)`;
- `SessionManager.create/open/continueRecent/inMemory/list/listAll`;
- `DefaultResourceLoader({ additionalExtensionPaths, extensionFactories })`;
- `session.subscribe()` for `message_update`, `message_end`, `turn_end`, `agent_end`, queue, retry, and compaction events;
- `session.prompt()` with `preflightResult`, but that preflight is prompt acceptance, not provider-send authorization.

Can a Noether-managed runner use Pi while Pi owns providers/auth/routing? Yes. If Noether embeds Pi SDK and uses Pi's auth/model registry, Pi still resolves auth and provider routing. For exact per-provider-send enforcement, the SDK path should load the same extension hook or inline extension factory; the SDK docs do not expose a separate public "deny provider send" API beyond the extension/runtime path.

SDK-path conclusion: strong if Noether is willing to become the process that runs Pi. It gives cleaner event ingestion than scraping session files, but it changes the user launch surface more than an extension loaded into ordinary Pi.

## Option C: CLI wrapper path

A `noet pi ...` wrapper can avoid touching global config by passing:

- `PI_CODING_AGENT_DIR` to an isolated generated agent directory;
- `--session-dir` or `PI_CODING_AGENT_SESSION_DIR` for Noether-owned session capture;
- `--extension <noether-extension.ts>` to install the authorization/trace hook for that run;
- `--no-extensions` plus explicit extension if the wrapper wants a controlled extension set;
- model, thinking, context, and resource flags as policy requires.

It can also run a launch preflight against Noether before starting Pi, inject metadata through the extension/session messages, and ingest session JSONL after exit.

Enforcement strength:

- Wrapper-only launch preflight: weak to medium. It is bypassed if the user runs `pi` directly and cannot enforce per-turn budget changes after launch.
- Wrapper plus hard-deny extension: strong for runs launched through the wrapper, because per-provider-send authorization happens inside Pi before transport.
- Wrapper plus session ingestion only: observation only.

CLI-wrapper conclusion: best packaging/control mechanism for the extension path. It should not be treated as the enforcement mechanism unless it always injects the extension.

## Option D: Network proxy / env-var path

Pi CLI sets an Undici `EnvHttpProxyAgent` globally in `dist/cli.js`, so `HTTP_PROXY`/`HTTPS_PROXY` style environment proxying is intentionally supported for Undici/fetch-backed traffic. Some provider SDKs also use fetch; Bedrock has separate proxy-agent handling in Pi AI source.

Without TLS MITM:

- For HTTPS/WSS upstreams, a proxy sees CONNECT target host/port, connection timing, and byte counts. It cannot see model, prompt, request path, authorization headers, response headers inside TLS, usage, or response bodies.
- It can block by host/port or force all traffic through a network chokepoint.
- It cannot do Noether's desired content/model/session-aware pre-authorization.

With TLS MITM, observation could be deeper, but that would introduce certificate installation, credential exposure risk, and provider-specific protocol handling. That conflicts with the "transparent/forward control layer first, not provider translation layer" direction.

Network-proxy conclusion: useful only as coarse network containment. Not a good primary integration for Pi subscription policy/usage.

## Option E: Transparent `baseUrl` override path

This remains a viable fallback when Noether must be on the HTTP hot path.

Evidence:

- Docs say overriding a built-in provider with only `baseUrl` preserves built-in models and existing OAuth/API-key auth.
- `dist/core/model-registry.js` applies provider-level `baseUrl` to existing built-in models.
- Pi's generated model registry exposes provider/model/api/baseUrl. Example: `openai-codex` models use `api: "openai-codex-responses"` and `baseUrl: "https://chatgpt.com/backend-api"`.
- `openai-codex-responses.js` appends `/codex/responses` to the model base URL unless the configured URL already ends in `/codex` or `/codex/responses`.

Can Noether avoid hardcoded upstream maps? Partially.

Pi exposes enough registry data for an integration to enumerate original provider/model `api` and `baseUrl` from Pi's installed package (`getProviders()`, `getModels()`, or `ModelRegistry`) before applying a Noether `baseUrl` override. That can generate mappings instead of hand-maintained provider tables.

However, Noether would still become responsible for provider-specific forwarding details:

- deriving final request URL from Pi's provider API implementation;
- preserving auth/account headers;
- handling SSE/WebSocket differences;
- returning provider-compatible error/stream shapes.

That is exactly the translation/compatibility surface the product direction wants to avoid as the primary path.

BaseUrl conclusion: keep as fallback for deterministic capture and route-through experiments, and generate mappings from Pi's model registry when possible. Do not make it the primary subscription-backed path unless extension/SDK hook enforcement breaks or Noether explicitly accepts provider-specific pass-through work.

## Enforcement strength matrix

| Path | Pre-spend enforcement | Usage/trace observation | Pi keeps auth/routing/shaping/streaming | Bypass risk | Product fit |
| --- | --- | --- | --- | --- | --- |
| Pi extension hard-deny with `ctx.abort()` | Strong, proven before local provider request | Strong via events/session; headers/status via `after_provider_response`; usage via final messages | Yes | Medium unless wrapper/package is mandated | Best |
| SDK-managed Pi runner + extension/inline hook | Strong in managed runner | Strong via direct event subscriptions/session APIs | Yes | Low inside managed app, high if users can run normal Pi outside it | Strong but larger product surface |
| CLI wrapper only | Weak/medium launch gate; no per-request gate by itself | Medium via session ingestion after exit | Yes | High if bypassed | Packaging layer, not core control |
| CLI wrapper + extension | Strong for wrapped runs | Strong | Yes | Medium | Recommended delivery shape |
| HTTP(S)_PROXY without MITM | Coarse host-level block only | Low; host/port/timing/bytes only for TLS | Mostly yes | Medium | Poor for policy |
| `baseUrl` override to Noether | Strong if Noether forwards/blocks correctly | Strong in Noether | No; Noether must emulate/forward provider protocol | Medium | Fallback |
| Session ingestion only | None | Medium/strong after completion | Yes | Low for observation, none for control | Complementary |

## Proven vs unknown

Proven:

- Pi 0.74.0 extension hooks include `before_provider_request` and `after_provider_response`.
- `before_provider_request` handlers are awaited and may perform async local HTTP calls.
- Throwing in `before_provider_request` is not a deny mechanism; Pi logs the extension error and continues.
- `ctx.abort()` in `before_provider_request` prevents a local provider request from being sent.
- An async Noether-like deny call followed by `ctx.abort()` prevents a local provider request from being sent.
- Pi session JSONL records assistant provider/model/usage/cost/stop reason according to docs and prior Noether validation.
- Pi built-in provider `baseUrl` overrides preserve built-in models/auth, and Pi source exposes original model `api`/`baseUrl`.

Unknown / needs validation:

- Whether `ctx.abort()` in `before_provider_request` behaves identically for every built-in subscription provider and every transport (`sse`, `websocket`, `auto`). The source checks abort before Codex SSE fetch; WebSocket behavior should be tested explicitly.
- Whether Pi maintainers consider `ctx.abort()` in this hook a stable authorization contract. Docs describe abort generally, but not as the official deny result for provider requests.
- Whether all desired response usage details are available for every provider through session messages, especially if a provider errors before emitting usage.
- How to represent a policy-denied provider turn cleanly in interactive UI/session history. Current print-mode proof exits with `Request was aborted.`
- How a team would prevent bypass if users can run plain `pi` without the Noether extension.

## Recommendation

Prioritize the Pi extension path, delivered by a `noet pi ...` wrapper or Pi package, for subscription-backed Pi usage.

Use this shape:

1. Wrapper starts/locates Noether and launches Pi with a Noether extension and isolated/session-specific paths where needed.
2. Extension calls Noether from `before_provider_request` with sanitized request/session/model metadata.
3. If Noether denies, extension calls `ctx.abort()` before Pi sends the provider request.
4. If Noether allows, extension returns nothing and Pi sends the request normally.
5. Extension/session ingestion reports response status/headers, assistant usage/cost, stop reason, model/provider, and trace IDs back to Noether.

Keep transparent `baseUrl` override as a fallback and fixture/capture path. If needed, generate original upstream mappings from Pi's installed model registry instead of handcoding them, but avoid making Noether responsible for provider-specific stream/request translation unless the product explicitly chooses that tradeoff.

## Next validation steps

1. Repeat the `ctx.abort()` proof against a subscription-backed provider with a deny policy and a network sentinel, using no real provider spend if possible.
2. Test Codex `transport: "auto"` and forced WebSocket behavior to prove abort occurs before WebSocket connect.
3. Build a sanitized extension prototype that sends only policy metadata by default, with body capture behind explicit opt-in.
4. Validate session/event usage ingestion across successful response, provider error, rate limit, abort, and tool-call turns.
5. Decide packaging: `noet pi` wrapper with explicit `--extension`, or a Pi package users install and enable.
