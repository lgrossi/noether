# Integration capability matrix

Date: 2026-05-27

Product invariant: Noether is a decision sidecar. Integrations own provider transport.

| Integration | Install shape | Pre-provider authorize | Deny blocks provider/tool | Usage finalize | Events | Limitation |
| --- | --- | --- | --- | --- | --- | --- |
| TypeScript SDK | Library | Supported by caller | Supported via `requireAuthorization` / `withDecision` | Supported by caller | Supported | SDK does not call providers or infer usage. |
| Python SDK | Library | Supported by caller | Supported via `require_authorization` / `with_decision` | Supported by caller | Supported | SDK does not call providers or infer usage. |
| Rust SDK | Library crate | Supported by caller | Supported via `require_authorization` / `with_decision` | Supported by caller | Supported | SDK does not call providers or infer usage. |
| Pi extension | Pi extension package | Supported through `before_provider_request` | Supported through `ctx.abort()` | Supported when Pi exposes assistant usage at `message_end` | Supported | Keeps hot-path HTTP local instead of importing SDK to stay packaging-light. |
| LiteLLM | Proxy callback | Supported through `async_pre_call_hook` | Supported by returning LiteLLM rejection string | Supported on success; failure finalizes zero cost without fake usage | Supported for failures | Depends on LiteLLM callback payload shape for usage/cost. |
| OpenCode | Plugin | Not supported | Not supported | Not supported | Supported for documented event/tool hooks | Public plugin surface does not document provider pre-call or usage hooks. |
| Claude Code | Hook command | Tool/action authorization supported; main model pre-call not supported | Supported for `PreToolUse` and `PermissionRequest` | Best effort for Agent tool usage only | Supported | Public hook surface does not document main model provider pre-call/usage hooks. |
| Codex | `codex exec --json` wrapper | Supported before launching `codex exec` | Supported before process spawn | Best effort when JSONL events expose usage/cost | Supported from JSONL stream | Local CLI exposes `exec --json`; no stable provider pre-call plugin hook verified. |

## Finalize/accounting rules

`POST /v1/reservations/{id}/finalize` accepts:

- `outcome: "success" | "failure" | "cancelled"`; default is `success` for backwards compatibility.
- `actual_cost_usd`: optional finite non-negative number.
- `usage.cost_usd`: optional finite non-negative number.
- token counts must be non-negative integers; when input, output, and total are all present,
  `total_tokens >= input_tokens + output_tokens`.

Failure and cancellation finalization must not invent usage. Include `usage` only when the harness or
gateway actually exposed token/cost data.

Finalization remains idempotent: after a reservation is finalized, repeated calls return the already
finalized reservation.
