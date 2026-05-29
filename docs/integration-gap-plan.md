# Integration gap plan

Noether's company claim must distinguish governed integrations from observed integrations.

## Current categories

| Category | Meaning |
| --- | --- |
| Governed | Noether can authorize before provider spend and the integration can block on deny. |
| Wrapper-gated | Noether can authorize before launching a wrapper-controlled process, but not inside the tool's internal provider lifecycle. |
| Tool-governed | Noether can authorize tool/action use, but not the main model provider call. |
| Observed | Noether can ingest events after the fact, but cannot prevent provider spend. |

## Current integration posture

| Integration | Current posture | Evidence source | Main gap |
| --- | --- | --- | --- |
| Pi extension | Governed for the validated provider hook path | `docs/integrations/pi-extension.md` | Repeat real installed-tool production smoke and keep hook compatibility current. |
| LiteLLM callback | Governed where LiteLLM pre-call hook and rejection behavior apply | `docs/integrations/litellm.md` | Repeat real proxy smoke, including failure finalization and usage visibility. |
| Codex wrapper | Wrapper-gated | `docs/integrations/codex.md` | Find a stable provider pre-call/plugin hook, or keep product claim limited to wrapper-gated non-interactive runs. |
| Claude Code hook | Tool-governed | `docs/integrations/claude-code.md` | Find a stable main-model pre-call and usage/cost hook, or keep claim limited to tool/action governance plus best-effort Agent usage. |
| OpenCode plugin | Observed | `docs/integrations/opencode.md` | Find documented provider pre-call and usage hooks, or keep claim limited to event/tool observation. |

## Work needed for weaker integrations

### Codex

Goal: move from wrapper-gated to governed, or explicitly keep it wrapper-gated.

Evidence needed:

- current Codex extension/plugin/CLI docs showing a stable pre-provider hook, if one exists;
- proof that deny prevents provider traffic before any model request;
- proof that usage/cost fields are reliable enough for finalization;
- fallback statement if only `codex exec --json` remains available.

### Claude Code

Goal: decide whether Claude Code can govern the main model path or only tools/actions.

Evidence needed:

- current hook docs for main-model pre-call, usage, and cost visibility;
- proof that deny prevents main-model provider traffic, not only tool use;
- proof that Agent/subagent usage can be attributed without inventing usage;
- explicit docs for the remaining tool-governed scope if no main-model hook exists.

### OpenCode

Goal: move from observed to governed only if public hooks support it.

Evidence needed:

- documented pre-provider hook or request middleware;
- documented post-provider usage/cost hook;
- proof that deny blocks provider traffic;
- proof that event-only mode does not overclaim enforcement.

## Product rule

If a hook cannot prevent spend before provider traffic, Noether should call the integration observed
or report-only, not governed.

