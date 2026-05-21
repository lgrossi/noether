# Roadmap

This roadmap is now integration-led. It assumes Noether is an agent-governance layer, not a
gateway product. Each phase should prove one public-facing claim about Noether's value beside
existing harnesses and gateways.

## Phase 1: local agent-governance baseline

Goal: make one local agent workflow understandable, governable, and testable without central
infrastructure.

Acceptance:

- Local sidecar starts with SQLite storage and repo-local `.noether/`.
- Policy uses `allow`, `warn`, `block`, and `ask`.
- Authorization, finalization, and lifecycle ingest work end to end.
- Local hot reload supports fast policy iteration.
- Reports explain decisions, attribution, tools, retries, steps, and trace flow.
- Normal mode does not store prompt/body content by default.

## Phase 2: first excellent harness integration

Goal: make one harness feel natively governed by Noether before provider send.

Acceptance:

- Pi remains the reference integration.
- Authorization happens before provider send.
- `ask` approval works in the harness UX.
- Lifecycle and usage delivery is asynchronous and does not degrade UX.
- Real-hook findings are represented in tests.
- Reports explain a real harness run without raw hook logs.

## Phase 3: harness depth, not gateway breadth

Goal: prove Noether works across the main coding-agent harnesses.

Target harnesses:

1. Pi
2. Claude Code
3. Codex
4. OpenCode

Acceptance:

- At least three harnesses have native integrations.
- Each native integration can send hot-path authorization plus async lifecycle/usage events.
- Each harness preserves its own auth, routing, request shaping, and streaming behavior.
- Each harness can surface `warn`, `block`, and `ask` clearly.
- A common attribution contract works across all supported harnesses.

## Phase 4: attribution, approval, and AI-native limits

Goal: prove Noether adds value beyond raw request logs.

Acceptance:

- Requests can carry or infer `budget_id`, entities, project, and session identity.
- Budget routing and fallback are explainable.
- Model allowlists and spend/context/request-cost limits work.
- Tool-call, retry, and agent-step limits are first-class policy surfaces.
- Approval is a first-class policy action, not extension-local behavior.
- Reports highlight risky work, blocked work, and approval-requiring work in human terms.

## Phase 5: native gateway-sidecar integrations

Goal: prove Noether adds agent-governance value beside existing gateways without owning transport.

Initial targets:

1. LiteLLM
2. Portkey
3. a third gateway integration only after a clear user pull

Acceptance:

- Noether can integrate beside at least two gateways natively.
- The gateway remains responsible for transport, routing, provider compatibility, and request brokering.
- Noether contributes hot-path policy decisions where the gateway permits it, plus async attribution and analysis.
- Noether does not need to become a provider translator to support these integrations.
- The product boundary with the gateway is explicit in docs and examples.

## Phase 6: scenarios as policy test rigs

Goal: give users concrete, runnable examples of Noether behavior before rollout.

Acceptance:

- Scenario files can describe budgets, entities, requests, approvals, denials, fallbacks, tool
  activity, and finalization.
- `noet scenario run <file>` replays scenarios through the public contract.
- Generated reports explain the expected story without live provider traffic.
- Initial scenarios cover local developer, team pooled budget, fallback routing, approval flow,
  runaway-agent limit, and protected adoption.

## Phase 7: strategy simulation lab

Goal: compare agent-governance strategies before enforcement.

Acceptance:

- Simulation files model users, teams, projects, behavior profiles, models, budgets, and strategy variants.
- `noet simulate <file>` compares strategies such as pooled caps, approval-heavy policies,
  protected adoption pools, and stricter agent limits.
- Output compares denied requests, useful work blocked, runaway spend prevented, adoption coverage,
  fairness, carryover liability, and exhaustion timing.

## Phase 8: selective team deployment

Goal: make Noether usable beyond one laptop without turning it into a generic gateway product.

Acceptance:

- Shared deployment path is documented.
- Existing local-first behavior remains available.
- Storage can move beyond local SQLite when needed.
- Reporting APIs support harnesses, gateways, and external analysis.
- Team deployment keeps Noether focused on policy, attribution, approval, and simulation rather
  than generic gateway administration.

## Not on the near-term roadmap

- Broad gateway administration UI.
- Provider protocol translation breadth.
- Rebuilding generic request-log explorer products.
- Competing with LiteLLM on routing or provider compatibility.
