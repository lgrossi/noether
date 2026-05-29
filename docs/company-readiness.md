# Company readiness

Noether's company-readiness target is a transparent sidecar pilot, not an enterprise
application platform.

## Product stance

- Noether should not implement built-in end-user auth, RBAC, or browser sessions for now.
- A company that needs Noether secured should run it behind its existing security layer: IAP,
  authenticated reverse proxy, private network, service mesh, or equivalent.
- Noether can stay simple and transparent if the deployment contract is explicit: trusted callers
  may call Noether; untrusted clients must not.
- Blocking central human approvals are not the desired workflow. Approval should be self-driven in
  the harness where possible, then centrally auditable when an override looks unusual.
- SQLite and single-process operation are the current local/pilot default. Postgres is the
  team/company storage direction once the storage work lands.

## Non-negotiable readiness work

1. **Company pilot deployment kit**
   - Supported single-process command.
   - Current SQLite path layout plus a storage-neutral boundary for Postgres.
   - Validation checklist.
   - Explicit non-goals: built-in auth/RBAC, multi-writer HA, provider routing.

2. **IAP / reverse-proxy security recipe**
   - Noether is intentionally unauthenticated by itself.
   - Do not expose Noether directly to the public internet.
   - Put all routes behind the company security boundary.
   - Separate read/report access from policy mutation access at the external layer when needed.

3. **Operational runbook**
   - Health checks, backup/restore, retention, logs, hot-path latency, and upgrade notes.
   - Storage-neutral where possible; current SQLite and future Postgres responsibilities are called
     out separately.

4. **Pi and LiteLLM production smoke documentation**
   - Real installed-tool validation, not only local mocks.
   - Fail-open/fail-closed expectations.
   - Bodyless/default privacy verification.
   - Report evidence required before calling an integration production-ready.

5. **Audited self-approval and override reporting**
   - Preserve non-blocking user flow.
   - Record override/rejection evidence.
   - Highlight repeated exceptions, high-cost self-approvals, or discrepant behavior centrally.

6. **Claude Code, Codex, and OpenCode integration gap plan**
   - Be explicit about which integrations are governed, wrapper-gated, or only observed.
   - Define evidence needed to improve each weaker integration.

7. **Hard-vs-report-only policy capability matrix**
   - Money/model/context controls are different from lifecycle-derived tool/retry/step signals.
   - Product and docs must not imply hard enforcement where an integration only supports reporting.

## Current implementation slice

- Deployment kit: [deployment/company-pilot.md](./deployment/company-pilot.md)
- External security recipe: [deployment/iap-reverse-proxy.md](./deployment/iap-reverse-proxy.md)
- Pi and LiteLLM production smoke checklist:
  [testing/pi-litellm-production-smoke.md](./testing/pi-litellm-production-smoke.md)
- Audited self-approval direction: [audited-self-approval.md](./audited-self-approval.md)
- Integration gap plan: [integration-gap-plan.md](./integration-gap-plan.md)
- Hard-vs-report-only capability matrix:
  [policy-capability-matrix.md](./policy-capability-matrix.md)
- Operations runbook: [operations-runbook.md](./operations-runbook.md)
- Integration probe contract: [integration-probe-contract.md](./integration-probe-contract.md)
