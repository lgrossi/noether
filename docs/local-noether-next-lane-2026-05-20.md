# Local Noether next-lane handoff

Date: 2026-05-20

## Purpose

Continue the local Noether setup work from a validated baseline and close the remaining runtime and
UX gaps:

1. hot reload for local policy/config changes
2. a standard `.noether/` home layout for local state
3. a simpler local startup command with far fewer explicit paths
4. better deny messages, especially model-denial wording
5. convert the live manual-test findings into concrete UX/reporting improvements

This handoff is for a new lane to continue implementation, not to rediscover context.

## Current validated baseline

The current local setup was validated with real Pi requests routed through a local Noether sidecar.

### Live local config

`~/.pi/agent/noether.json`

```json
{
  "noetherUrl": "http://127.0.0.1:4051",
  "projectFromCwd": true,
  "budgetId": "personal-local",
  "failMode": "fail_open",
  "policyMode": "user_approved"
}
```

### Live local policy

`~/.pi/agent/noether.policy.yaml`

```yaml
version: 0
routing:
  mode: explicit_then_fallback
  fallback_order: [project, user, team, group, org, global]
budgets:
  - id: personal-local
    models:
      allow:
        - openai-codex:*
        - openai:*
        - anthropic:claude-sonnet-*
        - anthropic:claude-haiku-*
    limits:
      spend:
        - id: budget-cap
          by: global
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1000
          warn_at_fraction: 0.8
          action: block
        - id: daily-cap
          by: global
          window: 1d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 100
          action: block
```

### Local launcher

`~/.pi/agent/bin/noether-local`

Runs `noet serve` on `127.0.0.1:4051` with explicit DB/fixture/simulation/policy paths.

### Extension behavior already landed

The Pi extension now:

- derives `project` from cwd when enabled
- derives `subject` from OS user by default when not explicitly configured
- emits `metadata.harness = "pi"`
- emits `metadata.request_surface` (`responses` / `chat` / `messages`)
- emits `metadata.model_api`
- emits correlation metadata: `trace_id`, `session_id`, `agent_run_id`, `request_id`,
  `provider_call_id`
- emits request-shape metadata and agent-context metadata

## What was live-tested

### Allow path

Real Pi run:

```bash
pi -p --model openai-codex/gpt-5.4-mini --no-session "Reply with exactly: local-noether-second-ok"
```

Observed result:

- request succeeded
- output returned normally
- Noether recorded:
  - `subject = user:lgrossi`
  - `project = noether`
  - `budget_id = personal-local`
  - `entities = ["project:noether","user:lgrossi"]`
  - `metadata.harness = "pi"`
  - `metadata.request_surface = "responses"`
  - `metadata.model_api = "openai-codex-responses"`
- reservation finalized against `personal-local`

Validated trace:

- `pi.agent_context`
- `pi.provider_call.started`
- `pi.authorize`
- `pi.message_end`
- `usage.finalized`
- `pi.turn_end`
- `pi.stream_summary`
- `pi.agent_end`

### Deny path scenarios tested

All deny tests used real Pi requests against a temporarily patched local policy, then the policy was
restored.

#### 1. Request-cost deny

Observed message:

```text
[noether-pi] Noether denied this provider request and policyMode=user_approved could not collect approval, so it was blocked: selected requested budget (personal-local); estimated request cost $0.000011 exceeds enforced limit max $0.000001 (personal-local.request_cost) [decision 31ccfa65-da22-41eb-b830-e5198902c540]
Request was aborted
```

#### 2. Context-token deny

Observed message:

```text
[noether-pi] Noether denied this provider request and policyMode=user_approved could not collect approval, so it was blocked: selected requested budget (personal-local); estimated context tokens 12 exceed enforced limit max 10 (personal-local.context_tokens) [decision e205e183-bd34-4506-bd6f-677835f5e21e]
Request was aborted
```

#### 3. Model allowlist deny

Observed message:

```text
[noether-pi] Noether denied this provider request and policyMode=user_approved could not collect approval, so it was blocked: requested provider/model is not allowed by requested budget (personal-local); no fallback budget can satisfy the request (no_fallback_budget); requested provider/model is not allowed by budget (personal-local) [decision 5feff17c-d760-4f0b-907e-a9da1b5404ae]
Request was aborted
```

#### 4. Daily spend-window deny

Observed message:

```text
[noether-pi] Noether denied this provider request and policyMode=user_approved could not collect approval, so it was blocked: selected requested budget (personal-local); projected spend $1.977946 exceeds enforced 1d limit max $0.000001 (personal-local.spend_window.daily-cap) [decision 660a45a9-1cdc-45a2-9603-ed722af75e54]
Request was aborted
```

## Insights from the live tests

### What is good

- The live request path works end to end.
- Budget selection, metadata capture, and finalization are functioning.
- Limit-specific deny reasons are preserved through to the extension.
- Context-token and spend-window denials are reasonably understandable.

### What is weak

#### 1. Model denial wording is the weakest

Current model-denial message is correct but noisy:

- repeated “requested budget” / “budget”
- repeated allowlist explanation
- no direct “attempted model” callout in the surfaced message
- too much raw routing detail for the primary user message

#### 2. `user_approved` unavailable wording is clunky

Current prefix:

> `policyMode=user_approved could not collect approval`

This is accurate in non-interactive `pi -p` mode, but not polished.

#### 3. Local runtime ergonomics are too manual

The validated local setup needed:

- a separate policy file in `~/.pi/agent`
- a launcher script
- explicit DB/fixture/simulation/policy paths
- a manual restart whenever policy changed

That is too much ceremony for the default personal/dev workflow.

#### 4. No hot reload

The deny tests required restarting the sidecar after every policy change. That is friction-heavy and
discourages iteration on local policies.

## User-requested follow-up requirements

These came directly from feedback after the live validation:

1. Noether must support hot reload.
2. Noether must have a base `.noether/` folder where configs and DB live.
3. Local startup should become a much simpler command without a long list of paths and flags.
4. Model-denial messaging should probably mention the attempted model explicitly.
5. Convert the live deny-test findings into UX/reporting improvements.

## Required implementation work

### A. Add hot reload for local policy/config

Target:

- local sidecar notices changes to its active local policy/config and reloads without restart

Minimum useful scope:

- policy file hot reload
- log or surface reload success/failure clearly
- fail closed on malformed reload only for new requests; do not corrupt in-memory state

Likely touched areas:

- `src/server.rs`
- `src/cli.rs`
- possibly a small local runtime config loader/helper

Acceptance criteria:

- editing the local policy file updates behavior for the next request without restarting `noet`
- invalid policy edit reports an error and preserves the last good active policy

### B. Standardize a `.noether/` home layout

Target:

- one canonical local runtime directory, likely per repo root for project-local state and/or under
  home for global defaults

The user explicitly asked for a base `.noether` folder. Proposed shape:

```text
.noether/
  policy.yaml
  noether.sqlite
  fixtures/
  simulations/
  logs/            # optional
```

Decisions to make:

- whether local personal usage should prefer repo-local `.noether/` or home-global `~/.noether/`
- whether Pi extension config should still live in `~/.pi/agent/noether.json` while Noether runtime
  files live under `.noether/`

Recommendation:

- keep Pi extension config in `~/.pi/agent/noether.json`
- move local Noether runtime state to repo-local `.noether/` by default

Acceptance criteria:

- starting local Noether without custom flags creates/uses the standard `.noether/` layout
- launcher script becomes optional, not required

### C. Add a simpler local command

Current friction:

```bash
cargo run --quiet --bin noet -- serve \
  --bind 127.0.0.1:4051 \
  --db-path ... \
  --fixture-dir ... \
  --simulation-dir ... \
  --policy ... \
  --decision-mode enforce
```

Need:

- one simple local/dev command

Possible shapes:

```bash
noet local up
noet local serve
noet serve --local
```

Desired defaults:

- bind `127.0.0.1:4051`
- use `.noether/noether.sqlite`
- use `.noether/fixtures`
- use `.noether/simulations`
- use `.noether/policy.yaml`
- decision mode suitable for local extension-backed enforcement

Acceptance criteria:

- local personal setup can be started with one memorable command
- no separate wrapper script is required for the normal path

### D. Improve deny-message quality

This should use the live-tested weak points directly.

Required improvements:

1. mention attempted model explicitly in model-denial messages
2. deduplicate repeated routing/model-check explanations
3. present one short primary reason, not an uncurated semicolon pile
4. keep raw decision details available in logs/events/reports, but shorten the user-facing line
5. improve `user_approved` unavailable wording

Concrete target examples:

#### Model denial desired shape

Instead of:

> requested provider/model is not allowed by requested budget ... no fallback budget ...

Prefer something like:

> Noether blocked `openai-codex/gpt-5.4-mini`: model not allowed on budget `personal-local`, and no fallback budget can pay for it.

#### Approval-unavailable desired shape

Instead of:

> policyMode=user_approved could not collect approval

Prefer something like:

> Noether would normally ask for approval here, but this Pi run could not show an approval prompt, so the request was blocked.

Likely touched areas:

- `extensions/pi-noether/src/index.ts`
- extension tests

Acceptance criteria:

- request-cost, context-token, model-denial, and spend-window deny messages are all shorter and
  clearer than the currently captured versions
- model-denial message includes attempted model
- non-interactive `user_approved` path reads naturally

### E. Turn manual-test findings into product/reporting improvements

The lane should not stop at the extension message string.

Review and improve:

- trace/report summaries for denials
- whether denied `pi.message_end` / `pi.turn_end` records are concise and helpful
- whether dashboard/risky-runs surfaces would explain model-denial and window-denial clearly

Specific questions:

1. Should denied request summaries include attempted model more prominently?
2. Should `binding_limit` be surfaced more directly in extension messaging?
3. Should model denial map to a special, more human label rather than a raw explanation string?

## Suggested execution order

1. deny-message cleanup in the Pi extension
2. local command simplification
3. `.noether/` layout defaults
4. hot reload
5. final live validation pass

Reason:

- messaging is the most visible weakness and can be iterated quickly
- command/layout changes clarify the runtime surface before adding reload behavior
- hot reload depends on a clearer notion of “the active local files”

## Validation plan for the next lane

### Automated

- policy validation tests
- extension tests
- any new hot-reload tests

### Manual

Repeat real Pi runs for:

1. normal allow
2. request-cost deny
3. context-token deny
4. model allowlist deny
5. spend-window deny

Capture exact user-facing messages again and compare before/after quality.

## Important restored baseline

At the end of this lane handoff preparation:

- the temporary deny mutations were reverted
- the active baseline policy is back to:
  - `1000 EUR / 30d`
  - `100 EUR / 1d`

Current restored policy validates:

```text
policy ok: version=0, budgets=1, policies=0
```

The temporary sidecars used during validation were stopped.
