# AI budget allocation standards

Date: 2026-05-15

## Current state

Noether v0 policy has flat budget rules: a request matches `subject`, `project`, `provider`, or
`model`; matching budget windows are charged by estimated/finalized cost. This is useful for local
proofing, but not enough for company AI governance because it cannot express shared budget pools,
eligible entities, model allowlists, budget selection, pacing, adoption goals, or priority reserves.
It also focuses too much on money windows, while AI misuse is often better controlled through
operational signals such as model choice, context size, tool calls, retries, and agent loops.

## Desired end state

Noether should treat a request as a claim against one selected budget pool. The request carries
company-provided metadata, Noether chooses the budget that should pay, validates budget eligibility
and model access, reserves estimated cost, finalizes actual cost, and explains the routing decision.

Noether should not become an identity/trust system. It trusts the metadata it receives. Companies
that need stronger guarantees should inject metadata from their own trusted middleware, Pi config,
SSO mapping, API gateway, or project tooling before the request reaches Noether. Noether may still
offer thin convenience helpers that derive metadata, such as mapping a cwd/git remote to
`project:*`; those helpers create request metadata, not a new trust/provenance model.

## Prior-art anchors

- OpenAI API projects combine scoped usage, spend budgets, rate limits, model permissions, and RBAC
  around project boundaries.
- Anthropic Workspaces support workspace-level spend/rate limits, and Anthropic applies limits per
  model as well as at organization/workspace levels.
- FinOps allocation practice uses account structures, tags, labels, derived metadata, showback, and
  chargeback to make shared costs visible and accountable.
- Kubernetes `ResourceQuota` shows the useful primitive boundary: quota scopes and priority fit
  better than a fully general business policy language.

Sources:

- <https://help.openai.com/en/articles/9186755-managing-your-work-in-the-api-platform-with-projects.eps>
- <https://platform.openai.com/docs/guides/rbac>
- <https://support.anthropic.com/en/articles/9796807-creating-and-managing-workspaces>
- <https://docs.anthropic.com/en/api/rate-limits>
- <https://www.finops.org/framework/capabilities/manage-shared-cloud-cost/>
- <https://www.finops.org/wg/identifying-shared-costs/>
- <https://kubernetes.io/docs/concepts/policy/resource-quotas/>

## Decision 1: request metadata is trusted input, not proven truth

The request should expose an optional `budget_id` and a normalized entity list such as
`user:alice`, `team:core`, `org:example`, `project:demo`, or `purpose:learning`.
Noether does not validate whether those claims are true. It only evaluates them against configured
budgets.

Rationale: this keeps Noether useful as an ask/reply sidecar instead of forcing customers into a
Noether-owned identity/middleware architecture. It also matches how FinOps systems commonly rely on
upstream tags, account structures, labels, and derived metadata.

## Decision 2: explicit budget is preferred, but invalid explicit budgets fall back

Routing should default to `explicit_then_fallback`:

1. If `budget_id` is present, try that budget first.
2. Use it only if it exists, the request is eligible, the model is allowed, and the requested spend
   fits remaining budget/slice constraints.
3. If it fails, record the rejection reason and infer another valid budget.
4. If no valid budget remains, deny.

Rationale: explicit user/caller intent is useful, but hard-failing on a typo, exhausted grant, or
model mismatch is needlessly brittle when another valid company budget can pay. The decision report
must show both the rejected explicit budget and selected fallback.

## Decision 3: inference favors specificity, then priority, then best-fit budget pressure

When Noether infers a budget, it should evaluate all valid budgets and sort by:

1. most-specific matched entity kind;
2. configured priority;
3. best-fit budget pressure;
4. stable budget id.

Default specificity order:

```text
project > user > team > group > org > global
```

Rationale: project budgets should usually pay before personal/team/org pools because projects are
the most useful unit for company work attribution. The specificity order remains configurable for
companies that want user grants or team pools to win. If two budgets are equivalent for the request,
Noether should choose the most obvious healthy pot rather than deny: a budget expiring soon with
healthy remaining capacity can be a better fit than a huge long-lived pool, while an over-paced or
nearly exhausted budget should lose. Priority remains available for intentional company overrides.

## Decision 4: model allowlists are first-class budget constraints

Each budget may define allowed provider/model patterns. If omitted, all models are allowed. If
present, the selected budget may only pay for matching models.

Example:

```yaml
models:
  allow:
    - openai:gpt-4.1
    - openai:gpt-4.1-mini
    - anthropic:claude-sonnet-*
```

Rationale: model access and budget scope are already coupled in AI platforms, and AI spend can vary
dramatically by model. This is not merely “premium model steering”; it is basic budget integrity.

## Decision 5: start with standards, not a broad policy language

The initial allocation standards should be named and constrained:

- `pooled_cap`: eligible requests share one pool.
- `reserved_plus_shared`: an entity kind receives reserved slices plus shared overflow.
- `protected_adoption_pool`: per-user/team protected opportunity with bounded carryover and
  underuse surfaced.
- `priority_reserve`: protected project/purpose/emergency reserve, with remainder policy later.

AI-specific controls should be explicit but small:

- spend windows such as 5h and 7d caps to avoid burning a monthly budget too early;
- runaway limits for unusually expensive requests, agent loops, retries, and tool storms;
- adoption/underuse signals so unused budget is visible as opportunity loss, not only savings.

Rationale: this borrows known allocation ideas while acknowledging AI-specific burn speed,
productivity value, model variance, and adoption goals. It also accounts for real company behavior:
some teams are not trying to squeeze AI spend down, but are under-spending and need safe ways to
increase adoption and consume more of the allocated budget productively. It avoids inventing
arbitrary DSL primitives before the product has real usage evidence.

## Benchmark scenario: active-user rolling pool

A real company pattern to learn from is: one AI budget pot, divided among active users, enforced
through short windows such as 5 minutes, with unused allowance accumulating or redistributing. This
is a useful benchmark, not a standard to copy blindly.

What Noether should learn:

- AI needs short-horizon burn control; monthly caps alone are too coarse.
- Static per-user/team slices can strand budget when adoption is uneven.
- Underuse is an adoption signal, not automatically a success.
- Controls on model choice, context, tool calls, retries, and agent loops are more actionable than
  asking people to optimize for money-spend ratios.

What Noether should avoid copying:

- active users as the primary denominator, because it can punish bursty valuable work and create bad
  incentives around when to spend;
- ultra-short redistribution as the core allocation policy, because it can be hard to explain and
  can incentivize underspending or budget gaming;
- waiting on inactive users for a full month before redistributing capacity.

The design implication is to support multi-window spend limits and bounded protected opportunity,
not to make active-user rolling redistribution a v1 allocation standard.

## Protected adoption carryover

`protected_adoption_pool` gives low/new adopters a fair shot without letting unused budget pile up
forever. Carryover is a separate bucket and is consumed before the current window grant.

End-of-window rule:

```text
next_carryover =
  min(remaining_carryover + remaining_current_grant * carryover_percent, carryover_cap)
```

Next window:

```text
current_grant_balance = protected_amount
available = carryover_balance + current_grant_balance
spend_order = carryover first, then current grant
```

Example:

```yaml
allocation:
  standard: protected_adoption_pool
  by: user
  protected_amount_usd: 25
  window: monthly
  carryover:
    percent: 10
    cap_usd: 50
```

If a user starts with `$10` carryover and `$25` current grant, then spends `$12`, the `$10`
carryover is consumed first and `$2` comes from the current grant. At month end, `$23` of current
grant remains, so `$2.30` carries forward. This protects future opportunity but prevents a year of
AI inactivity from creating a large personal balance.

## Shape sketch

```yaml
routing:
  mode: explicit_then_fallback
  specificity: [project, user, team, group, org, global]

budgets:
  - id: project-noether
    amount_usd: 1000
    window: monthly
    priority: 50
    eligible:
      entities: [project:noether]
    models:
      allow: [openai:gpt-4.1, anthropic:claude-sonnet-*]
    allocation:
      standard: pooled_cap

  - id: eng-shared
    amount_usd: 5000
    window: monthly
    priority: 10
    eligible:
      entities: [org:example]
    allocation:
      standard: reserved_plus_shared
      by: team
      reserved_percent: 80
      shared_percent: 20

  - id: ai-adoption
    amount_usd: 2000
    window: monthly
    priority: 20
    eligible:
      entities: [org:example]
    models:
      allow: [openai:gpt-4.1-mini, anthropic:claude-haiku-*]
    allocation:
      standard: protected_adoption_pool
      by: user
      protected_amount_usd: 25
      carryover:
        percent: 10
        cap_usd: 50
    limits:
      spend:
        - window: 5h
          max_usd: 10
        - window: 7d
          max_usd: 75
      tool_calls: 30
      agent_steps: 50
      context_tokens: 120000
```

## Patterns to follow

- Keep decisions explainable: selected budget, rejected requested budget, matched entity, model
  check, remaining budget, allocation slice, and fallback reason.
- Prefer fixed routing semantics over user-configurable algorithms.
- Treat `purpose:*` as an entity string, not a dedicated top-level policy dimension. Companies can
  model purposes differently, and Noether should not hard-code organizational semantics too early.
- Prefer operational AI controls over money-only throttles where possible: model allowlists,
  context ceilings, tool/step/retry limits, and spend windows should all produce visible feedback.
- Keep v0 policy compatibility by treating current flat budget rules as legacy/simple pooled caps.
- Make dashboard/report UX show budget health, pacing, low adopters, protected carryover, top
  consumers, model denials, and fallback selections.

## Patterns to avoid

- No claim provenance/trust DSL in Noether v1.
- No arbitrary nested policy language.
- No double-charging one request to multiple budgets.
- No hidden ambiguity denial when a deterministic best-fit tie-breaker can choose safely.
- No model-tier taxonomy before plain model allowlists.
- No active-user rolling-share standard in v1; use it as a benchmark scenario for burn control,
  adoption visibility, and redistribution tradeoffs.

## Compatibility and migration

Current `limit_usd` budget rules can migrate to `amount_usd` + `allocation.standard=pooled_cap`.
Current `match.project/provider/model/subject` becomes `eligible.entities` and/or `models.allow`.
The ledger should store selected budget id, selection reason, rejected requested budget reason, and
charged slice so reports can explain behavior across both old and new policies.

## Open questions

- How thin should the first project derivation helper be: cwd prefix mapping, git remote mapping, or
  both?
- What exact best-fit budget pressure score is simple enough for v1 while handling expiring budget,
  remaining capacity, and pacing health?
- Which operational limits should be enforced in the hot authorization path first: request cost,
  context tokens, model allowlist, tool calls, agent steps, or retries?
