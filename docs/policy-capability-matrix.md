# Policy capability matrix

Noether policy capabilities are integration-dependent. A policy can be valid while a specific
integration can only report part of it after the fact.

## Capability levels

| Level | Meaning |
| --- | --- |
| Hard enforceable | Noether can decide before spend/action and the integration can block. |
| Soft enforceable | Noether can warn or ask before spend/action, but local operator behavior or fail mode can still continue. |
| Report-only | Noether can detect or explain after events arrive, but cannot prevent the action. |
| Unsupported | The integration does not expose enough signal for useful control or reporting. |

## Control matrix

| Control | Sidecar/SDK with pre-call | Pi extension | LiteLLM | Codex wrapper | Claude Code hook | OpenCode plugin |
| --- | --- | --- | --- | --- | --- | --- |
| Required attribution | Hard enforceable when caller uses `requireAuthorization`/equivalent | Hard enforceable on provider hook path | Hard enforceable in pre-call hook | Hard enforceable before wrapper launch | Tool-governed only | Report-only/event metadata |
| Model allowlist | Hard enforceable | Hard enforceable on provider hook path | Hard enforceable in pre-call hook | Wrapper-gated before launch | Not main-model enforceable today | Report-only if model visible |
| Request cost limit | Hard enforceable if estimated cost is supplied | Hard enforceable when estimate is available | Hard enforceable when estimate is available | Wrapper-gated when estimate/model is supplied | Tool-governed only | Unsupported/report-only |
| Context/token estimate limit | Hard enforceable if estimated tokens are supplied | Hard enforceable when Pi exposes estimate | Hard enforceable when estimate metadata is supplied | Wrapper-gated when estimate is supplied | Tool-governed only | Unsupported/report-only |
| Spend windows | Hard enforceable for authorized requests | Hard enforceable on provider hook path | Hard enforceable in pre-call hook | Wrapper-gated before launch | Tool-governed only | Unsupported/report-only |
| Tool-call limits | Integration-dependent | Report-only today from lifecycle events | Unsupported unless gateway emits tool events before execution | Report-only if events expose tools | Hard enforceable for documented tool hooks | Tool-governed where documented hooks can block |
| Agent-step limits | Report-only unless caller checks between steps | Report-only today from lifecycle events | Unsupported unless gateway emits step events before continuation | Report-only if JSONL exposes steps | Report-only/best-effort | Report-only if events expose steps |
| Retry limits | Report-only unless caller checks before retry | Report-only today from provider-call/turn events | Unsupported unless gateway exposes retry lifecycle | Report-only if JSONL exposes retries/provider calls | Report-only/best-effort | Report-only if events expose retries |
| Protected adoption pool | Hard enforceable for authorized spend | Hard enforceable on provider hook path | Hard enforceable in pre-call hook | Wrapper-gated before launch | Tool-governed only | Report-only/unsupported |

## Documentation rule

Use "deny", "block", or "enforce" only when an integration can stop the relevant provider spend or
tool/action before it happens.

Use "report-only", "detect", "highlight", or "audit" when Noether learns from lifecycle or usage
events after the fact.

## Product consequence

For company pilots, Noether should lead with the integration paths where the important company
controls are hard enforceable. For weaker integrations, the product value is still real, but the
claim is observability and audit rather than enforcement.

