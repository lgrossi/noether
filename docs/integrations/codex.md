# Codex integration

The Codex integration lives in [`integrations/codex`](../../integrations/codex).

Codex is modeled as a harness, not as a provider. The wrapper records:

- `metadata.harness = "codex"`;
- `provider = NOET_CODEX_PROVIDER` only when the provider is known/configured;
- `model` from Codex CLI args or `NOET_CODEX_MODEL`.

Local evidence:

- installed CLI: `@openai/codex@0.125.0`;
- documented local command: `codex exec --json`;
- plugin CLI help exposes marketplace management but not a stable provider pre-call hook.

The integration therefore authorizes before launching `codex exec --json`, records Codex JSONL
events, and finalizes with `outcome: "success"` only when a Codex event exposes usage/cost data. It
does not intercept or call model providers.
