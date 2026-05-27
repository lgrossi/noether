# Integration readiness validation

Date: 2026-05-27

## Environment

- Branch: `main`
- Commit validated before follow-up edits: `7082ca36bce7454c4016cf0ff2d44d4c92007387`
- Sidecar command:

```bash
target/debug/noet serve \
  --bind 127.0.0.1:4059 \
  --policy examples/policy.noet.yaml \
  --decision-mode enforce \
  --db-path .noet/validation.sqlite \
  --fixture-dir .noet/validation-fixtures
```

## Scenarios

| Scenario | Evidence | Result |
| --- | --- | --- |
| Health endpoint | `GET /health` returned `status=ok`, `decision_mode=enforce`, `policy_loaded=true`. | Pass |
| OpenAPI endpoint | `GET /openapi.json` returned `200` and repeated `jq` parses passed after rebuilding the current binary. | Pass |
| TypeScript SDK live sidecar | Authorized `validation/sdk-ts`, finalized cost `0.001`, report showed finalized usage row. | Pass |
| Python SDK live sidecar | Authorized `validation/sdk-python`, finalized cost `0.001`, report showed finalized usage row. | Pass |
| LiteLLM live sidecar | `async_pre_call_hook` authorized and stored reservation; success hook finalized usage. | Pass |
| OpenCode live sidecar | Plugin posted generic/tool events to `/v1/events`. | Pass |
| Claude Code live sidecar | `PreToolUse` authorized Bash action; `PostToolUse` posted observation. | Pass |
| Codex live sidecar | Wrapper authorized before fake `codex exec --json`, forwarded JSONL events, finalized observed usage. | Pass |
| Policy deny | Missing project authorization returned SDK `NoetherDeniedError` with reason `project is required for budget attribution`. | Pass |

## Report evidence

`noet report --db-path .noet/validation.sqlite usage` showed finalized rows for:

- `validation/sdk-ts`
- `validation/sdk-python`
- `openai/openai/gpt-validation` from LiteLLM
- `openai/gpt-validation` from Codex
- `claude-code` tool action finalization with zero cost

## Findings

- OpenCode and Claude Code should remain documented as event/tool integrations, not full provider
  authorization integrations.
- Codex should remain a wrapper around `codex exec --json` until a stable provider pre-call plugin
  hook is verified.
- Finalize semantics needed explicit outcome and accounting validation; this validation pass added
  follow-up implementation for that gap.

Verdict: ITERATE. The integration surface is usable for local dogfood and SDK/gateway work, but
real installed-tool smoke tests for OpenCode, Claude Code, and Codex should be repeated in their
actual user environments before calling those integrations production-ready.
