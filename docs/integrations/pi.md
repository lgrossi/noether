# Pi integration

## Scope

This note records the Stream 1 validation of Pi with Noether on 2026-05-14 using Pi 0.74.0.

Goals:

- keep existing Pi credentials and configuration safe;
- validate a Pi custom provider that routes through `noet`;
- validate what happens with the user's normal subscription-backed Pi provider;
- record whether Noether can route, pre-authorize, or observe Pi traffic.

Local captures from this run are under `.noet/fixtures/pi-stream1-20260514/`. That directory is ignored by git and may contain prompts, responses, and redacted-but-sensitive operational metadata.

## Safety baseline

Global Pi files were hashed and copied before validation:

| File | Baseline SHA-256 |
| --- | --- |
| `~/.pi/agent/models.json` | `fbf75a2ec7cdc642b396bb7e01c1ccd84fd5883561b21bfd3aa98910c8731876` |
| `~/.pi/agent/settings.json` | `cdfa1e5090e7ca5980732070416630cd2311840a313aa3dd140f20d7f2064fdf` |
| `~/.pi/agent/auth.json` | `0411b32c5c91cba0fb0fb18b5d3d6820156ee300829b5f655034e6e3b5bf69c0` |

Backups were written locally to `.noet/fixtures/pi-stream1-20260514/config-backup/`.

Global Pi files had the same hashes after validation. Stream 1 did not run `/login` or `/logout`, did not manually edit `auth.json`, and did not edit the real global `models.json` or `settings.json`.

## Custom provider template

The safe template is in [`examples/pi/models.noether.json`](../../examples/pi/models.noether.json).

It defines a `noether` provider:

- `baseUrl`: `http://127.0.0.1:4040/v1`
- `api`: `openai-completions`
- model: `noether-mock`
- local placeholder `apiKey`: `noether-local`

For validation, the template was copied into an isolated Pi agent directory:

```bash
mkdir -p .noet/pi-agent-noether
cp examples/pi/models.noether.json .noet/pi-agent-noether/models.json
PI_CODING_AGENT_DIR=$PWD/.noet/pi-agent-noether pi --list-models noether --offline
```

Result:

```text
provider  model         context  max-out  thinking  images
noether   noether-mock  128K     4.1K     no        no
```

## Custom provider validation

Started Noether capture:

```bash
cargo run --bin noet -- serve --fixture-dir .noet/fixtures/pi-stream1-20260514/custom-provider
```

Ran Pi through the isolated custom provider:

```bash
PI_CODING_AGENT_DIR=$PWD/.noet/pi-agent-noether \
  pi --model noether/noether-mock \
  --thinking off \
  --no-tools \
  --no-session \
  --no-extensions \
  --no-skills \
  --no-prompt-templates \
  --no-context-files \
  --offline \
  -p 'Reply with exactly: pi-noether-ok'
```

Observed Pi output:

```text
Noether mock response
```

The fixture recorded a `POST /v1/chat/completions` request with:

- `authorization` header redacted;
- JSON body keys including `model`, `messages`, `stream`, `stream_options`, `max_tokens`, `store`, and prompt-cache fields;
- response source `mock`;
- status `200`;
- response content type `text/event-stream`.

Conclusion: Pi custom providers that use `openai-completions` can route through Noether today for local capture and mock responses.

## Subscription-backed routing probe

The user's normal subscription-backed provider was `openai-codex` with model `gpt-5.5` in `~/.pi/agent/settings.json`. Auth presence was confirmed by key name only: `openai-codex`.

To avoid editing global config, Stream 1 created an isolated Pi agent directory and copied the existing `auth.json` and `settings.json` into `.noet/pi-agent-openai-codex-proxy/`, then added a minimal local `models.json` override:

```json
{
  "providers": {
    "openai-codex": {
      "baseUrl": "http://127.0.0.1:4040"
    }
  }
}
```

With that isolated override, `pi --list-models gpt-5.5 --offline` still listed `openai-codex/gpt-5.5`, proving the override preserved built-in models and the copied subscription credential.

Running a tiny prompt with this override reached Noether. The captured request path was:

```text
/codex/responses
```

The request body included keys:

- `include`
- `input`
- `instructions`
- `model`
- `parallel_tool_calls`
- `prompt_cache_key`
- `store`
- `stream`
- `text`
- `tool_choice`

The model field was `gpt-5.5`.

The current Noether mock response is OpenAI Chat Completions SSE, not OpenAI Codex Responses SSE. Pi therefore did not produce a useful assistant response in this route-through test, but Noether did receive and capture the subscription-backed request before provider delivery.

Conclusion: subscription-backed `openai-codex` traffic can be routed through Noether by overriding the built-in provider `baseUrl`, but Noether needs an `openai-codex-responses`-compatible response path or upstream forwarding to make that route usable end to end.

## What Noether can control

| Mode | Route through Noether | Pre-authorize before spend | Observe after the fact |
| --- | --- | --- | --- |
| Pi custom provider (`noether/noether-mock`) | Yes | Yes, inside `noet` before mock/upstream forwarding | Yes, via Noether fixtures |
| Built-in subscription provider without override | No | No hard Noether gate | Yes, via Pi session files and extension hooks |
| Built-in subscription provider with `baseUrl` override | Yes | Yes, inside `noet` before upstream forwarding | Yes, via Noether fixtures and Pi session files |
| Pi extension only | No transport ownership | Partial; can inspect/rewrite payload, but this is not a proxy-level spend gate | Yes, via extension events and session files |

## Documentation sources used

Pi local docs read during validation:

- `README.md`
- `docs/models.md`
- `docs/custom-provider.md`
- `docs/session-format.md`
- `docs/settings.md`
- `docs/extensions.md`
- `docs/sdk.md`

Relevant documented Pi behavior:

- custom providers are configured through `~/.pi/agent/models.json`;
- overriding a built-in provider with only `baseUrl` preserves built-in models;
- sessions are JSONL files under `~/.pi/agent/sessions/` or a configured `sessionDir`;
- extensions can observe `before_provider_request` and `after_provider_response`;
- the SDK can use Pi auth/model registry/session APIs directly.

