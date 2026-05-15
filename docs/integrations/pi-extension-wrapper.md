# Pi extension and `noet pi` wrapper prototype

Date: 2026-05-15

## Architecture

For Pi subscription-backed usage, Noether does not translate provider protocols or forward provider streams. Pi continues to own provider authentication, provider/model routing, provider-specific request shaping, transport/streaming, response parsing, and session persistence.

Noether controls the run through a Pi extension loaded by the wrapper:

```text
noet pi ... -> pi --extension integrations/pi/noether-extension.js ...
                    |
                    | before_provider_request
                    v
             POST /v1/authorize
                    |
          allow/warn continues, deny calls ctx.abort()
                    |
       after_provider_response/message_end/turn_end/agent_end
                    v
             POST /v1/events
             POST /v1/reservations/{id}/finalize
```

The source is [`integrations/pi/noether-extension.js`](../../integrations/pi/noether-extension.js). It is a sanitized, commit-safe prototype with no credentials, no captured prompts, and no dependency on real Pi config.

## Privacy posture

The extension is bodyless by default. Its authorization request includes configured `subject` and `project`, provider/model from Pi context when available, estimated context tokens from `ctx.getContextUsage()` when available, and sanitized metadata such as cwd, model API, payload type, top-level payload keys, and shape summaries.

It does **not** send prompt/body content by default. Prompt-like keys such as `messages`, `input`, `instructions`, `prompt`, and `system` are summarized by type/length only.

Body inclusion exists only as an explicit prototype escape hatch:

```bash
NOET_PI_INCLUDE_BODY=1 noet pi ...
```

Do not use that mode with real prompts unless the receiving Noether endpoint and retention policy are appropriate.

## Deny behavior

The extension runs on `before_provider_request`, calls Noether asynchronously, and handles outcomes as follows:

- `allow`: return normally and Pi sends the provider request;
- `warn`: return normally and Pi sends the provider request;
- `deny`: call `ctx.abort()` before Pi sends the provider request.

Pi extension errors are not a denial mechanism; Pi catches thrown handler errors and continues. The hard-deny mechanism for this integration is `ctx.abort()`, as validated in [`docs/integrations/pi-wrapper-research.md`](./pi-wrapper-research.md).

## Fail-open and fail-closed modes

Noether unavailability is configurable:

- default: `fail_open` (`NOET_PI_FAIL_MODE=fail_open`) so local development does not break when the sidecar is down;
- strict: `fail_closed` (`NOET_PI_FAIL_MODE=fail_closed`) so provider sends are aborted if authorization cannot be obtained.

The wrapper mirrors this with:

```bash
noet pi --fail-mode fail-open -- ...
noet pi --fail-mode fail-closed -- ...
```

In `fail-closed`, the wrapper also treats a failed `/health` preflight as a launch error unless `--no-health-check` is supplied.

## Wrapper usage

Run Noether:

```bash
cargo run --bin noet -- serve --policy examples/policy.noet.yaml --decision-mode enforce
```

Launch Pi through Noether:

```bash
cargo run --bin noet -- pi \
  --noether-url http://127.0.0.1:4040 \
  --project my-project \
  --subject alice@example.test \
  --fail-mode fail-open \
  -- \
  --model openai-codex/gpt-5.5
```

The wrapper injects `--extension <repo>/integrations/pi/noether-extension.js`, `NOET_URL`, `NOET_PI_PROJECT`, `NOET_PI_SUBJECT`, `NOET_PI_FAIL_MODE`, `--session-dir .noet/pi-sessions`, and `PI_CODING_AGENT_SESSION_DIR=.noet/pi-sessions`.

It does not edit `~/.pi/agent/auth.json`, `models.json`, or `settings.json`. By default it reads the user's normal Pi config so subscription providers still work. If a test needs complete config isolation, provide:

```bash
noet pi --agent-dir .noet/pi-agent-isolated -- ...
```

To disable discovered user/project extensions and load only Noether plus explicit Pi arguments:

```bash
noet pi --no-discovered-extensions -- ...
```

## Local safe proof

The safe proof script uses only local mock Noether and provider servers plus a mock Pi extension lifecycle. It writes no credentials and cannot reach a real provider:

```bash
node integrations/pi/proof-deny-local.mjs
```

The script starts a mock Noether endpoint that returns `deny`, registers the Noether extension against a small mock Pi API, emits `before_provider_request`, and only sends to the mock provider if the extension did not abort. It asserts Noether saw one authorization request, the provider saw zero requests, and prompt text was not sent to Noether.

Generated proof files stay under ignored `.noet/`.

A real Pi invocation proof is intentionally documented rather than run by default because it is more sensitive to installed Pi version, available local models, and interactive CLI behavior:

```bash
PI_CODING_AGENT_DIR=$PWD/.noet/pi-agent-isolated \
PI_CODING_AGENT_SESSION_DIR=$PWD/.noet/pi-sessions \
NOET_URL=http://127.0.0.1:4040 \
NOET_PI_FAIL_MODE=fail_closed \
pi --session-dir .noet/pi-sessions \
  --no-extensions \
  --extension "$PWD/integrations/pi/noether-extension.js" \
  --model <local-test-provider/model> \
  --print "safe deny proof prompt"
```

## Known limits

- The extension uses Pi 0.74.0 hook behavior. `ctx.abort()` should remain under regression proof because `before_provider_request` is not documented as a dedicated policy API.
- `after_provider_response` exposes status and headers, not parsed usage. Usage is reported later from assistant messages in `message_end`/`turn_end`.
- The wrapper protects only runs launched through `noet pi`; users can bypass it by running `pi` directly.
- The wrapper does not start `noet serve` automatically yet. It locates the configured endpoint and performs a health check before launch.
- Usage/reservation matching is sequential and prototype-level. Parallel provider sends would need stronger correlation if Pi adds them.
