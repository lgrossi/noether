# Product vision

## One-line thesis

Noether keeps the invariants of LLM usage: budget, policy, evaluation, and observability across harnesses, proxies, and providers.

## Problem

LLM usage is spreading across coding harnesses, internal apps, gateway proxies, direct SDK calls, and managed model aggregators. Existing tools tend to cluster around one of two poles:

- **Routers/proxies**: good at central enforcement, but often become bloated protocol translators.
- **Harnesses/cockpits**: good at workflow visibility, but local to a tool or operator.

The unsolved gap is a small, auditable control layer that can answer:

- Who or what made this model request?
- Which project, task, or session should it count against?
- Was it allowed by policy before it ran?
- Which budget was reserved, spent, or exhausted?
- What did it cost, how long did it take, and what happened afterward?
- Which traces, tool calls, eval labels, and outcomes explain the usage?

## Product idea

Noether is a vendor-neutral LLM control sidecar. It can run locally for an individual developer or centrally for a team. It is not primarily an LLM router. It is the policy, budget, trace, and evaluation companion that a harness, app, SDK, or gateway can call into.

```text
Harness / app / proxy / SDK
        |             \
        |              \ async events
        v               v
Hot-path decision     Trace/usage ingest
        \              /
         \            /
          Policy + budget ledger + reports
```

## Target users

### Individual operator

Someone running multiple AI coding sessions through tools such as Pi, Claude Code, Codex, OpenCode, or local models.

They need:

- per-project usage visibility;
- soft or hard local budgets;
- traceability across sessions;
- awareness of expensive or runaway work;
- a path to use subscription-backed harnesses for dogfooding without productizing subscription tunneling.

### Platform or AI enablement team

A team responsible for company-wide LLM usage, budget discipline, security posture, and auditability.

They need:

- centralized policy decisions;
- hard budget reservations and reconciliation;
- team/project/user attribution;
- low-friction integration with existing proxies and apps;
- audit and observability without adopting yet another full LLM platform.

## Value proposition

Noether should be useful even when it does not own the model call:

- **With proxy/SDK integration**: it can hard-block requests before spend happens.
- **With harness integration**: it can enforce local policy and capture workflow traces.
- **With async-only ingestion**: it can still produce observability and insight, but not hard enforcement.

The product must make this enforcement level explicit.

## Non-goals

- Rebuild LiteLLM in Rust.
- Own full provider protocol correctness as the core product.
- Productize consumer-subscription tunneling or browser automation.
- Build a large dashboard before the ledger, decision API, and event schema are proven.
- Pretend model traffic alone fully explains agent/tool/MCP behavior.
- Encode one company's quota policy as the product model.

## Ecosystem fit

- **Majin**: cockpit/operator UI for local AI-native development.
- **Euler**: possible future harness/execution engine.
- **Noether**: invariant layer for budgets, policy, traces, evals, and audit.

Noether should be usable without Majin, but Majin should be able to consume Noether data for cockpit views such as budget pressure, policy denials, expensive lanes, and trace summaries.
