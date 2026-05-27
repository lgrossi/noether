# OpenCode integration

The OpenCode integration lives in [`integrations/opencode`](../../integrations/opencode).

Current public OpenCode plugin docs support plugins loaded from `.opencode/plugins/` or
`~/.config/opencode/plugins/`, a generic `event` hook, and documented hooks such as
`tool.execute.before` and `tool.execute.after`. They do not document a provider/model pre-call hook
or a guaranteed post-provider usage hook.

Resulting scope:

- supported: session/message/tool event reporting to `POST /v1/events`;
- unsupported for now: provider-call authorization, deny/block before model transport, and provider
  usage finalization.

This is intentionally weaker than the Pi and LiteLLM integrations, but it does not pretend OpenCode
exposes lifecycle points that are not documented.
