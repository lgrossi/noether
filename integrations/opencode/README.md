# Noether OpenCode integration

OpenCode currently exposes public plugin hooks for lifecycle/events and tool execution, but not a
documented provider/model pre-call hook. This integration therefore records OpenCode activity through
Noether events only. It does not claim to block provider calls or finalize provider usage.

## Capability matrix

**Capability limit:** this plugin records OpenCode activity and tool execution, but it cannot prove
or block main model provider spend because OpenCode does not expose a documented provider pre-call
transport hook. Treat provider-spend prevention claims as unsupported for this integration.

| Capability | Status | Basis |
| --- | --- | --- |
| Provider pre-call authorization | Not supported by this integration | OpenCode plugin docs list event, tool, shell, TUI, session, message, permission, file, command, and LSP hooks; they do not document a provider/model pre-call hook. |
| Provider/model access before model call | Not supported by this integration | No documented plugin hook exposes a mutable provider request before transport. |
| Usage finalization | Not supported by this integration | No documented plugin hook guarantees provider usage/cost payloads after transport. |
| Tool execution events | Supported | `tool.execute.before` and `tool.execute.after`. |
| Session/message lifecycle events | Supported | Generic `event` hook receives documented OpenCode event types such as `session.*` and `message.*`. |

## Install locally

Copy or symlink `noether-opencode.mjs` into one of OpenCode's plugin directories:

- `.opencode/plugins/`
- `~/.config/opencode/plugins/`

Configure with environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `NOET_OPENCODE_URL` | `http://127.0.0.1:4051` | Noether sidecar URL. |
| `NOET_OPENCODE_TIMEOUT_MS` | `1000` | Event delivery timeout. |
| `NOET_OPENCODE_PROJECT` | OpenCode project name or directory basename | Project metadata. |
| `NOET_OPENCODE_SUBJECT` | unset | Subject metadata. |
| `NOET_OPENCODE_INCLUDE_BODY` | unset | Set to `1` only to include body-like strings; default is shape-only summaries. |

The plugin posts to `POST /v1/events` and never calls model providers.
