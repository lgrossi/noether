# Noether Codex integration

This integration treats Codex as a harness, not as a provider.

The locally installed Codex CLI (`@openai/codex@0.125.0`) exposes `codex exec --json` for an
observable non-interactive event stream. Its public CLI help does not expose a stable provider
pre-call/plugin hook for the main model request. The Noether integration is therefore a wrapper for
non-interactive Codex runs:

```text
noether-codex -> Noether /v1/authorize
noether-codex -> codex exec --json ...
codex owns provider transport
noether-codex -> Noether /v1/events
noether-codex -> Noether /v1/reservations/{id}/finalize only when usage is present
```

## Capability matrix

**Capability limit:** this wrapper authorizes a Codex run before launch and reports observable
JSONL events, but Codex still owns provider transport. It cannot prove that an already-started
Codex process blocks provider spend inside a provider pre-call hook.

| Capability | Status | Notes |
| --- | --- | --- |
| Pre-run authorization | Supported | Wrapper calls `/v1/authorize` before spawning Codex. |
| Provider/model separation | Supported | `metadata.harness = "codex"`; provider is `NOET_CODEX_PROVIDER` when known, model comes from `--model`/`-m` or `NOET_CODEX_MODEL`. Codex is never sent as provider. |
| Provider transport hook inside Codex | Not supported | No stable provider pre-call hook is exposed by local CLI help. |
| JSONL event reporting | Supported | Wrapper requires/forces `codex exec --json` and posts events. |
| Usage finalization | Best effort | Finalizes only if Codex JSONL events contain usage/cost fields. |

## Usage

```bash
node integrations/codex/noether-codex.mjs --model gpt-5.5 "fix the tests"
```

The wrapper runs:

```bash
codex exec --json --model gpt-5.5 "fix the tests"
```

## Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `NOET_CODEX_URL` | `http://127.0.0.1:4051` | Noether sidecar URL. |
| `NOET_CODEX_FAIL_MODE` | `fail_closed` | `fail_open` allows Codex to run when Noether is unavailable; `fail_closed` blocks. |
| `NOET_CODEX_TIMEOUT_MS` | `1000` | Noether request timeout. |
| `NOET_CODEX_PROJECT` | cwd basename | Project metadata. |
| `NOET_CODEX_SUBJECT` | unset | Subject metadata. |
| `NOET_CODEX_PROVIDER` | unset | Provider metadata when known. |
| `NOET_CODEX_MODEL` | unset | Model fallback when not passed with `--model`/`-m`. |
| `NOET_CODEX_BIN` | `codex` | Codex executable. |

The wrapper never calls model providers itself.
