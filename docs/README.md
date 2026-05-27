# Noether docs

This directory holds Noether's north-star product and architecture notes.

- [Product vision](./product-vision.md): what Noether is for, who it serves, and what it must not become.
- [High-level solution design](./solution-design.md): the core architecture, contracts, integration modes, and enforcement boundaries.
- [Roadmap](./roadmap.md): near-term slices that keep the project pointed at the product thesis.
- [Team deployment](./team-deployment.md): shared-server path, storage evolution, trust boundary,
  and local-first compatibility notes.
- [Integration readiness plan](./integration-readiness-plan.md): OpenAPI, SDK, and harness/gateway
  integration sequence for the decision-sidecar product boundary.
- [Export and reporting API contract](./export-reporting-api.md): shipped reporting HTTP endpoints,
  live-dashboard data/update surfaces, artifact-backed simulation routes, and the CLI/HTTP contract
  shape they share.

These docs are intentionally higher-level than implementation tickets. They should change when the product thesis or architectural boundaries change, not for every small code edit.
