# Noether Claude Code integration

Claude Code exposes hooks for session/user-prompt/tool lifecycle. It does not document a provider
transport pre-call hook for the main model request. This integration therefore governs Claude Code
tool actions and records lifecycle events; it does not claim to authorize the main provider call.

## Capability matrix

**Capability limit:** this hook can block Claude Code tool/permission flows, but it cannot prove or
block main model provider spend because Claude Code does not expose a documented provider pre-call
transport hook. Treat provider-spend prevention claims as unsupported for this integration.

| Capability | Status | Notes |
| --- | --- | --- |
| Main model provider pre-call authorization | Not supported | Claude Code public hooks do not expose a provider request before transport. |
| Provider/model access for main model call | Not supported | Hook payloads expose session/tool context, not the main provider request. |
| Main model usage finalization | Not supported | No documented hook guarantees main model usage/cost after transport. |
| Tool pre-call authorization | Supported | `PreToolUse` can return `permissionDecision: "deny"`. |
| Permission request authorization | Supported | `PermissionRequest` can return `decision.behavior: "deny"`. |
| Tool success/failure events | Supported | `PostToolUse` and `PostToolUseFailure`. |
| Agent subtask usage | Best effort | `Agent` `PostToolUse` may expose `usage`, `totalTokens`, and duration. |

## Hook configuration

Example `.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "node /absolute/path/to/noether/integrations/claude-code/noether-claude-code.mjs"
          }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "node /absolute/path/to/noether/integrations/claude-code/noether-claude-code.mjs"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "node /absolute/path/to/noether/integrations/claude-code/noether-claude-code.mjs"
          }
        ]
      }
    ],
    "PostToolUseFailure": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "node /absolute/path/to/noether/integrations/claude-code/noether-claude-code.mjs"
          }
        ]
      }
    ]
  }
}
```

## Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `NOET_CC_URL` | `http://127.0.0.1:4051` | Noether sidecar URL. |
| `NOET_CC_FAIL_MODE` | `fail_open` | `fail_closed` denies when Noether is unavailable. |
| `NOET_CC_TIMEOUT_MS` | `1000` | Hot-path authorize/event timeout. |
| `NOET_CC_PROJECT` | cwd basename | Project metadata. |
| `NOET_CC_SUBJECT` | unset | Subject metadata. |
| `NOET_CC_STATE_DIR` | `.noether/claude-code` under cwd | Reservation correlation state. |
| `NOET_CC_INCLUDE_BODY` | unset | Set to `1` only to include body-like fields; default is shape-only summaries. |

The hook posts to Noether's sidecar API only. It never calls model providers.
