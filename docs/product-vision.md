# Product vision

## Thesis

Noether is the local-first control plane for AI work: it observes agent usage, attributes it to the
right work, controls risky spend/model behavior, and helps teams grow AI adoption safely.

## Problem

AI usage is spreading across coding agents, harnesses, internal apps, SDKs, gateway proxies, and
managed model platforms. Existing tools tend to cluster around two poles:

- **Routers/proxies**: good at central enforcement, but often become protocol translators and model
  gateways.
- **Harnesses/cockpits**: good at workflow visibility, but usually local to one tool or operator.

The missing layer is a small, auditable control plane that can answer:

- Who or what made this model request?
- Which project, task, session, or entity should it count against?
- Was it allowed by policy before spend happened?
- Which budget was selected, reserved, finalized, denied, or exhausted?
- Which model, context size, tools, retries, and agent steps explain the usage?
- Did this run look productive, risky, runaway, or under-attributed?
- Are teams adopting AI well, or leaving useful budget/opportunity unused?

## Product idea

Noether runs locally for an individual or centrally for a team. It is not primarily an LLM router.
It is the policy, budget, trace, and adoption companion that a harness, app, SDK, or gateway can
call into.

```text
Harness / app / proxy / SDK
        |             \
        |              \ async events
        v               v
Hot-path decision     Trace/usage ingest
        \              /
         \            /
          Policy + budget ledger + reporting API + dashboards
```

## Adoption thesis

Noether should be useful on day one without enforcement.

The adoption path is:

```text
observe -> attribute -> warn -> enforce -> improve
```

Users should be able to keep their existing AI tools, add Noether, and immediately get useful
visibility. Enforcement should be progressive and explicit.

## Product pillars

### Observe

Make AI work visible:

- provider/model usage;
- token/cost accounting;
- trace timelines;
- tool and agent activity;
- eval and annotation events;
- local ledger, reporting API, and dashboards.

### Attribute

Make AI work belong somewhere:

- normalized entities such as user, project, team, org, and purpose;
- explicit or inferred budget routing;
- project derivation helpers over time;
- missing attribution warnings;
- selected-budget explanations.

### Control

Apply lightweight governance:

- allow/warn/deny decisions;
- model allowlists;
- budget selection and reservation;
- spend windows;
- request-cost and context-size limits;
- future tool-call, retry, and agent-step guards;
- fail-open and fail-closed modes.

### Improve

Help teams use AI better:

- low-adoption detection;
- protected adoption pools;
- bounded carryover;
- underuse as opportunity signal;
- context-heavy and tool-heavy run detection;
- model-denial and fallback reporting.

### Simulate

Make product claims executable.

Noether should include:

1. **Native end-to-end scenarios**
   - 5-6 curated examples that demonstrate realistic use cases from authorization through reports
     and the static export dashboard.

2. **Strategy simulations**
   - synthetic companies with many users, teams, projects, usage profiles, and allocation/control
     strategies so users can compare likely outcomes before adopting a policy.

Scenario emulation is a product feature, not only a test harness. It lets maintainers and adopters
validate claims such as:

- budget routing chooses the expected budget;
- model allowlists deny or fallback correctly;
- spend windows catch runaway burn;
- protected adoption pools carry over correctly;
- reports, the live dashboard, and static export dashboards explain the scenario in human terms.

## Target users

### Individual operator

Someone running multiple AI coding sessions through tools such as Pi, Claude Code, Codex, OpenCode,
or local models.

They need:

- per-project usage visibility;
- local soft or hard budgets;
- traceability across sessions;
- awareness of expensive or runaway work;
- privacy-preserving local storage.

### Small engineering team

A team adopting AI agents but not ready for enterprise AI infrastructure.

They need:

- shared visibility;
- model and budget guardrails;
- project/team attribution;
- understandable reports;
- observe/warn modes before enforcement.

### Platform or AI enablement team

A team responsible for AI governance, enablement, budget discipline, and auditability.

They need:

- centralized policy decisions;
- hard budget reservations and reconciliation;
- team/project/user attribution;
- low-friction integration with existing tools;
- showback before chargeback;
- adoption and underuse visibility.

## Value proposition

Noether should be useful even when it does not own the model call:

- **With proxy/SDK integration**: it can hard-block requests before spend happens.
- **With harness integration**: it can enforce local policy and capture workflow traces.
- **With async-only ingestion**: it can still produce observability and insight, but not hard
  enforcement.

The product must make this enforcement level explicit.

## Non-goals

- Rebuild LiteLLM in Rust.
- Own full provider protocol correctness as the core product.
- Productize consumer-subscription tunneling or browser automation.
- Store prompts by default.
- Build a broad enterprise policy DSL.
- Pretend model traffic alone fully explains agent/tool/MCP behavior.
- Encode one company's quota policy as the product model.

## Ecosystem fit

- **Majin**: cockpit/operator UI for local AI-native development.
- **Euler**: possible future harness/execution engine.
- **Noether**: invariant layer for budgets, policy, traces, evals, and audit.

Noether should be usable without Majin, but Majin should be able to consume Noether data for cockpit
views such as budget pressure, policy denials, expensive lanes, trace summaries, and adoption
signals.
