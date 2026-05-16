# Noether

**Local-first observability and control for AI agent usage.**

Noether helps teams understand, govern, and improve AI-assisted work without replacing the tools
they already use. It sits beside AI coding agents, harnesses, apps, SDKs, or gateways; receives
hot-path authorization requests before model spend; ingests usage and lifecycle events afterward;
and turns the result into an explainable ledger, reports, and dashboard.

Noether is not a provider router, prompt warehouse, identity system, or generic FinOps clone. It is
an AI work control plane.

```text
AI agent / app / gateway
        |              \
        | authorize     \ events + usage
        v                v
   allow / warn / deny   trace ingest
          \             /
           \           /
      budget + policy + ledger + dashboard
```

## Why Noether exists

AI budgets are not ordinary cloud budgets.

- They are directly tied to productivity.
- They can burn very quickly through model choice, context size, retries, tool storms, and agent
  loops.
- Companies often want to increase adoption, not only cut spend.
- Underuse can be a missed opportunity, while overuse can be a runaway cost or safety problem.
- Provider billing pages cannot explain which project, agent run, tool call, or policy decision
  caused the spend.

Noether's goal is to make AI work legible and governable:

- What ran?
- Who or what did it belong to?
- Which model and tools were used?
- Which budget should pay?
- Was it allowed, warned, denied, or routed to a fallback?
- Did usage look healthy, wasteful, risky, or under-adopted?

## Product pillars

### Observe

Make AI work visible before controlling it.

- request decisions
- provider/model usage
- cost and token accounting
- trace timelines
- tool and agent activity
- eval/annotation events
- local SQLite ledger
- static HTML dashboard

### Attribute

Make usage belong somewhere.

- user/project/team/org/purpose entities
- explicit or inferred budget routing
- project derivation helpers over time
- missing-attribution warnings
- selected-budget explanations

### Control

Apply lightweight, explainable governance.

- allow / warn / deny
- model allowlists
- budget selection and reservation
- spend windows such as 5h / 7d
- request-cost and context-size limits
- future tool-call, retry, and agent-step guards
- fail-open or fail-closed deployment modes

### Improve

Help teams use AI better, not just cheaper.

- low-adoption visibility
- protected adoption pools
- bounded carryover
- underused budget as opportunity signal
- context-heavy or tool-heavy run detection
- model-denial and fallback reporting

### Simulate

Prove product claims with executable scenarios.

- native end-to-end examples for common use cases
- synthetic company simulations with user profiles and strategy comparisons
- report/dashboard assertions for CI
- reproducible examples that require no live provider credentials

## Current status

Noether is early but functional.

Today it includes:

- local `noet` sidecar;
- `POST /v1/authorize`;
- reservation finalization;
- `POST /v1/events`;
- SQLite-backed decision/usage/event ledger;
- story-shaped CLI reports;
- static HTML dashboard;
- Pi extension integration;
- bodyless authorization metadata by default;
- opt-in raw hook debug logs;
- vertical MVP demo with no provider credentials.

The policy model is still v0. The next product phase is the entity-based AI budget model described
in [`docs/design/ai-budget-allocation-standards.md`](./docs/design/ai-budget-allocation-standards.md).

## Quick demo

Run a safe local demo with no provider credentials:

```bash
./examples/vertical-mvp-demo.sh
```

Then generate a visual dashboard:

```bash
cargo run --bin noet -- report \
  --db-path .noet/demo/vertical-mvp.sqlite \
  dashboard \
  --out .noet/noether-dashboard.html
```

Open the generated file in a browser:

```bash
xdg-open .noet/noether-dashboard.html
```

## Use with Pi

Start Noether:

```bash
cargo run --bin noet -- serve \
  --policy examples/policy.noet.yaml \
  --decision-mode enforce
```

Run Pi with the extension:

```bash
NOET_URL=http://127.0.0.1:4040 \
NOET_PI_PROJECT=noether \
NOET_PI_SUBJECT=user:local \
NOET_PI_FAIL_MODE=fail_open \
pi --extension "$PWD/extensions/pi-noether"
```

Inspect results:

```bash
cargo run --bin noet -- report decisions
cargo run --bin noet -- report usage
cargo run --bin noet -- report trace <trace_id>
cargo run --bin noet -- report observations --kind tool --trace <trace_id>
cargo run --bin noet -- report dashboard --out .noet/noether-dashboard.html
```

## Privacy posture

Noether is local-first.

- SQLite by default.
- No cloud service required.
- Normal Pi extension authorization is bodyless by default.
- Prompt-like fields are summarized, not retained.
- Raw hook logging is explicit debug mode only.
- Capture/proxy modes are for controlled local inspection and redact credential-like fields.

## Scenario emulator vision

Noether should ship with two kinds of executable examples:

1. **Native end-to-end scenarios**
   - individual local developer
   - team pooled budget
   - project budget fallback
   - model-denial/fallback
   - runaway agent guard
   - protected adoption pool

2. **Strategy simulations**
   - synthetic company of hundreds or thousands of users
   - different usage profiles such as power users, steady users, low adopters, and loop-risk agents
   - strategy comparison across pooled caps, protected adoption pools, reserved/shared budgets, and
     other future standards

The point is not only testing. It is evidence: every major public claim Noether makes should have a
scenario that demonstrates it.

## Documentation

- [Product vision](./docs/product-vision.md)
- [Roadmap](./docs/roadmap.md)
- [AI budget allocation standards](./docs/design/ai-budget-allocation-standards.md)
- [Control contract v0](./docs/control-contract-v0.md)
- [Policy v0](./docs/policy-v0.md)
- [Pi extension integration](./docs/integrations/pi-extension.md)
- [Capture fixture schema v1](./docs/capture-fixtures.md)
- [Transparent proxy mode](./docs/transparent-proxy.md)

## Non-goals

- Rebuild LiteLLM.
- Own provider protocol correctness as the product.
- Store prompts by default.
- Become a generic enterprise policy DSL.
- Productize consumer-subscription tunneling.
- Encode one company's quota policy as the product model.

## License

License is not finalized yet.
