# Claude Code integration

The Claude Code integration lives in
[`integrations/claude-code`](../../integrations/claude-code).

Current Claude Code hook docs say:

- `PreToolUse` runs before a tool call and can return `permissionDecision: "deny"`;
- `PermissionRequest` can deny a permission dialog;
- `PostToolUse` and `PostToolUseFailure` observe completed/failed tool calls;
- `Agent` tool responses may expose subagent token usage.

The public hook surface does not document a provider transport pre-call hook for the main model
request, nor a guaranteed main-model usage/cost hook. So this integration supports Noether
authorization for Claude Code tool actions and best-effort Agent subtask usage finalization only.

It never calls providers and never rewrites Claude Code provider transport.
