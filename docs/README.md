# Noether docs

This directory holds Noether's north-star product and architecture notes.

- [Product vision](./product-vision.md): what Noether is for, who it serves, and what it must not become.
- [High-level solution design](./solution-design.md): the core architecture, contracts, integration modes, and enforcement boundaries.
- [Roadmap](./roadmap.md): near-term slices that keep the project pointed at the product thesis.
- [Team deployment](./team-deployment.md): shared-server path, storage evolution, trust boundary,
  and local-first compatibility notes.
- [Company readiness](./company-readiness.md): current company-pilot stance, non-negotiables, and
  implementation slices.
- [Company pilot deployment](./deployment/company-pilot.md): supported single-process deployment
  shape, durable paths, sensitive routes, and validation checklist.
- [IAP and reverse-proxy security recipe](./deployment/iap-reverse-proxy.md): how to secure Noether
  behind an existing company security layer without adding built-in auth.
- [Audited self-approval](./audited-self-approval.md): self-driven approval semantics and central
  override audit signals.
- [Integration gap plan](./integration-gap-plan.md): grounded posture for governed, wrapper-gated,
  tool-governed, and observed integrations.
- [Policy capability matrix](./policy-capability-matrix.md): hard-enforceable versus report-only
  policy controls by integration class.
- [Operations runbook](./operations-runbook.md): storage-neutral pilot operations for health,
  backup/restore, retention, latency, and upgrades.
- [Integration probe contract](./integration-probe-contract.md): local stub/probe evidence expected
  before weak harness integrations can claim stronger governance.
- [Pi and LiteLLM production smoke checklist](./testing/pi-litellm-production-smoke.md): real-tool
  validation evidence required before company-pilot claims.
- [Integration readiness plan](./integration-readiness-plan.md): OpenAPI, SDK, and harness/gateway
  integration sequence for the decision-sidecar product boundary.
- [Integration capability matrix](./integration-capability-matrix.md): supported authorization,
  finalize, event, and limitation matrix for SDKs and harness/gateway integrations.
- [LiteLLM integration](./integrations/litellm.md): LiteLLM callback integration that authorizes,
  finalizes, and records outcomes while LiteLLM owns provider transport.
- [OpenCode integration](./integrations/opencode.md): OpenCode plugin integration for documented
  event/tool hooks, with provider-authorization limitations called out.
- [Claude Code integration](./integrations/claude-code.md): Claude Code hook integration for
  tool/action authorization and documented lifecycle events.
- [Codex integration](./integrations/codex.md): Codex `exec --json` wrapper that authorizes before
  launch, records events, and keeps provider/model metadata separate from the harness.
- [Export and reporting API contract](./export-reporting-api.md): shipped reporting HTTP endpoints,
  live-dashboard data/update surfaces, artifact-backed simulation routes, and the CLI/HTTP contract
  shape they share.
- [Storage backends](./storage-backends.md): SQLite and PostgreSQL deployment modes,
  guarantees, and tradeoffs.

These docs are intentionally higher-level than implementation tickets. They should change when the product thesis or architectural boundaries change, not for every small code edit.
