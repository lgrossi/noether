# Noether — Production-Readiness Audit

**Date:** 2026-05-30
**Branch audited:** `noether/db-port` (audit input) cross-referenced to `origin/main` @ `2e1d3d5`
**Goal of audit:** assess whether Noether is robust and reliable enough to run as an internal tool in any enterprise / business / personal space — and produce an actionable plan to get there.

> **Methodology note.** The original 22-dimension audit was executed against `noether/db-port` with sonnet sub-agents (the harness coerces all sub-agent model selections to sonnet regardless of the requested model — confirmed across `model: opus`, `model: claude-opus-4-7`, `model: claude-opus-4-7[1m]`). Each finding was then independently verified by two further sonnet agents (refutation lens + severity lens). The findings were re-classified against `main` by 22 further sonnet agents that read main-version source. The synthesis you are reading was written directly in the main session loop (opus). This is captured here for full transparency; the substance of the findings is unaffected because the verifier + gap-analysis passes are evidence-grounded reads of real code.

---

## 1. Executive Summary

This audit should be read as a production-direction document, not as a mandate to implement every
finding. The useful conclusion is narrower and clearer than the raw count suggests:

> Noether's next production milestone should be a controlled internal sidecar pilot: one team or
> company runs Noether behind its existing security boundary, with honest integration claims,
> durable hot-path behavior, operable logs/metrics, and stored evidence that governed integrations
> actually block spend before provider traffic.

That target preserves Noether's strongest product identity: a local-first governance sidecar for
agent work, not a model gateway and not a generic multi-tenant enterprise platform. It also changes
the priority order. The 351 findings are evidence; the roadmap is the following narrowed sequence:

1. **Make the claim safe:** CI, license, version/release discipline, and a repeatable install path.
2. **Make the hot path trustworthy:** storage transactions, PG lock/statement/acquire timeouts,
   proxy/request timeouts, replay caps, and reachable panic removal.
3. **Make the trust boundary explicit in code:** keep IAP/reverse proxy as the recommended outer
   boundary, but add opt-in `--api-key`/`NOET_API_KEY` middleware, non-loopback warnings, and
   key-derived actor attribution for approval and policy audit. Do not jump straight to full RBAC.
4. **Make the system operable:** request IDs, JSON logs, structured allow/warn/ask/deny events,
   `/metrics`, richer `/health`, and slow-query/slow-authorize visibility.
5. **Make integration claims honest:** Pi and LiteLLM need stored live-smoke evidence that denial
   prevents provider spend; Claude Code, Codex, and OpenCode stay explicitly limited unless
   stronger hooks are proven.
6. **Pay down architecture where it blocks confidence:** first fix SDK error semantics, shared
   contract types, and SDK/integration HTTP behavior because those affect pilot integrations. Then,
   after contract tests exist, split `ledger.rs` by domain, move dashboard rendering out of
   `cli.rs`, and split thick server handlers.
7. **Make policy/API contracts match reality:** complete OpenAPI coverage, add contract tests,
   classify internal app routes, and fix policy-engine mismatches where docs claim enforcement that
   code does not provide.

The defer list below is a product strategy overlay on top of the audit findings, not an audit
conclusion. The audit identifies real gaps; this production direction decides which ones are not
needed for the first controlled sidecar pilot. Defer unless the product target changes:
multi-tenant schema/RBAC, browser sessions, SQLCipher/encryption-at-rest productization, a React
rewrite, broad SDK publishing, OTLP/SBOM programs, and full privacy/SAR machinery. SDK correctness
bugs and README/API usability bugs remain in-pilot work; publishing every SDK broadly is the part
deferred.

**Baseline note after rebase.** This report was originally synthesized against `origin/main` at
`2e1d3d5`. The PR has since been rebased over `73c730c` (core binary releases and updates),
`2c3e6f0` (advisory notifications and configurable cadence), and `d6b3462` (noet lifecycle and
deployment UX). Those commits likely resolve or partially resolve release, install, lifecycle, and
advisory-cadence findings. Before executing Phase 1 or Phase 7, run a focused delta pass against
the rebased main and trim anything already landed.

### 1.1 Current-main delta pass

The focused pass against the rebased branch changes the execution plan in these areas:

| Area | Current-main state | Planning impact |
| --- | --- | --- |
| CI | `.github/workflows/ci.yml` now runs formatting, `cargo test --locked`, and release binary build. | Treat "no CI" as partially resolved. Remaining CI work is PG service coverage, pi-noether/npm tests, integration-probe smoke, and scheduled `cargo audit`. |
| Release artifacts | `.github/workflows/release.yml` builds Linux x86_64, macOS arm64, and Windows x86_64 binaries, uploads checksums, writes `noether-release.json`, creates a prerelease, and publishes a container image. | Remove "no binary releases" and "no release workflow" from pilot blockers. Keep release smoke hardening and manifest/version checks as maintenance. |
| Container path | `Dockerfile`, `docs/deployment/container.md`, and README container instructions now exist. | Remove "no Dockerfile/container path" from pilot blockers. Keep runtime auth/metrics/health concerns separate. |
| Lifecycle/install UX | README and CLI now document/support `noet config init/show`, `noet up`, `up -d`, `status`, `logs`, `open`, `down`, `update check`, and `update apply`. | Treat lifecycle/onboarding as materially progressed. Pilot work should validate the flow, not redesign it. |
| Version command | `#[command(version)]` is wired for `noet --version`. | Mark the original `--version` gap resolved. |
| Advisory cadence | Advisory notifications and configurable cadence landed after the audit baseline. | Recheck related observability/storage-schema findings before turning them into implementation tasks. |
| Still open | No `LICENSE*` file is present; no explicit `[profile.release]`; no `--api-key`; no `/metrics`; no JSON log mode; no request IDs; storage timeout/transaction issues still appear open; SDK correctness/publishing work remains open. | These stay in the pilot-readiness backlog, with SDK correctness separated from broad SDK publishing. |

### 1.2 Controlled sidecar pilot scope

Pilot readiness means Noether can be run by one team/company behind an existing security boundary
and can truthfully prove what it governs. It does not mean generic enterprise/multi-tenant
readiness.

| In scope for pilot readiness | Deferred unless the product target changes |
| --- | --- |
| LICENSE and current release/install validation | Multi-tenant schema and per-tenant query isolation |
| CI completion: PG service test, extension tests, integration probes, audit job | Full RBAC or browser sessions |
| Storage integrity: SQLite transactions, PG acquire/statement/lock timeouts, WAL policy | SQLCipher/encryption-at-rest productization |
| HTTP hot-path reliability: reqwest/proxy timeouts, replay caps, bounded errors | React rewrite or frontend framework migration |
| Minimal in-process auth: optional API key, non-loopback warning, actor attribution | Broad SDK registry publishing before there is a consumer pull |
| Operability: request IDs, JSON logs, structured decision events, `/metrics`, richer `/health` | OTLP/SBOM/supply-chain program beyond basic audit/dependency hygiene |
| Integration evidence: Pi and LiteLLM live smokes proving deny prevents spend | Full privacy/SAR program |
| SDK correctness: no silent 4xx/5xx masking, compiling README examples, API-key support | Shared multi-user policy draft isolation |
| API/policy truth: OpenAPI coverage, contract tests, policy-doc enforcement mismatches | Generic enterprise IAM replacement |

This scope is the input for implementation tasking. Work outside the right column can still be
valuable, but it should not block the first pilot unless the target shifts from "controlled
sidecar" to "shared enterprise platform."

### 1.3 Pilot-readiness task set

The implementation backlog should be tracked as vertical workflows, not as the original 12 audit
phases. The first three workflows are the pilot-readiness foundation and should be implemented
before deeper SDK/API/architecture work:

1. **Finish baseline readiness**
   - Keep: LICENSE, release-profile decision, PG CI service test, pi-noether/npm CI test,
     integration-probe smoke, and scheduled dependency audit.
   - Already progressed by current main: GitHub Actions exists, release workflow exists, release
     binary build exists, Docker/container path exists, lifecycle commands exist.
   - Acceptance: PR CI exercises Rust plus the important integration/package tests; external users
     can legally evaluate/use the repository; release builds stay validated.

2. **Fix storage and timeout safety**
   - Implement SQLite authorize/finalize transaction boundaries, PG acquire/statement/lock
     timeouts, WAL autocheckpoint policy, and hot-path panic removal.
   - Acceptance: crash/timeout failure modes are bounded; multi-write ledger updates are atomic;
     tests cover SQLite and PostgreSQL paths where available.

3. **Bound HTTP, proxy, and replay failure modes**
   - Add upstream/proxy request timeouts, streaming idle timeout, replay job cap/cleanup, request
     body limit, and sanitized server errors.
   - Acceptance: slow or hostile upstreams cannot hang the sidecar indefinitely; replay cannot grow
     without bound; client-facing 5xx bodies do not leak internal SQL/DB details.

Then continue with:

4. **Add minimal auth and audit actor** (`NOET_API_KEY`, bearer middleware, non-loopback warning,
   actor propagation).
5. **Add operability** (request IDs, JSON logs, structured decision events, `/metrics`, richer
   `/health`).
6. **Collect integration smoke evidence** (Pi and LiteLLM live smokes; explicit weaker-integration
   limits).
7. **Fix SDK correctness and shared contracts** (no 4xx/5xx masking, Python exception narrowing,
   Rust README/API correctness, API-key support, contract crate if needed).
8. **Align API and policy truth** (OpenAPI coverage, contract tests, policy/doc mismatches).
9. **Split architecture after tests stabilize** (`ledger.rs`, dashboard rendering from `cli.rs`,
   thick server handlers).

The original audit summary follows.

Noether has crossed the line from a working prototype into a coherent governance layer. `main` is materially better than `noether/db-port`: the two most dangerous reliability blockers (the `sqlite_conn()` panic on the Postgres backend and the `HotState` mutex-poison cascade) are gone; the Postgres path is real and benchmarked; a deployment story (systemd units + IAP reverse-proxy guidance + operator runbook) exists where there was none; an `approval_audit` subsystem gives operators something they can point at when asked "who approved what?".

**7 of the 8 original P0 findings are resolved in main.** The single remaining P0-class blocker is the architectural one the team has explicitly chosen to defer: Noether has **no in-process authentication**. The deployment story is "put it behind IAP and trust the front door". That works for a single-tenant company pilot. It does not work for a generic enterprise tool, and it does not work the moment the front door is misconfigured.

The other dominant blockers across the codebase are not bugs but absences: **no CI**, **no LICENSE**, **no Dockerfile**, **no published SDK packages**, **no /metrics endpoint**, **no structured logging on the hot path**, **no tenant boundary in queries**, **no SQLite transactions around the authorize write cycle**. Several of these are explicit product decisions documented in `docs/company-readiness.md` ("Noether should not implement built-in end-user auth, RBAC, or browser sessions for now") rather than oversights. That choice has to be reversed — at minimum as opt-in — before Noether can be deployed by anyone who isn't holding all the pieces.

**351 distinct findings** were carried through the audit, verifier pass, and gap analysis. Net counts after applying main-version reality:

| Severity | Count | Status |
|---|---|---|
| **P0** | 8 | 7 RESOLVED in main, 1 CONFIRMED (no auth) |
| **P1** | 55 | enterprise-deployment blockers; mix of new gaps and persistent ones |
| **P2** | 171 | quality gaps that bite at scale |
| **P3** | 78 | polish |
| Dropped / resolved | 31 | already addressed by main |

`main` reflects roughly **10% improvement** on the original audit surface — about 30 findings genuinely resolved, 33 partially resolved, 17 new findings introduced by net-new main code (`src/approval_audit.rs`, `src/bin/noet-db-conn-bench.rs`, the new docs and deployment artifacts).

**Only 2 of 351 findings require a fix that breaks user-visible behavior.** The remaining 349 are additive or internal — a meaningful constraint to honor: production-readiness work here is mostly compatible with existing surfaces.

---

## 2. Vision

Noether's positioning — *"the policy file for agent work: written once, simulated honestly, enforced quietly, and explained by every decision it makes"* — is sharp and defensible. It is not trying to be a gateway, it is not trying to own provider transport, it is not trying to be a generic enterprise policy DSL. The product surface (Policy → Runs → Replay → Policy) is well-scoped. The local-first posture, the explicit fail-modes, the integration capability matrix, and the bodyless authorization default are all genuine differentiators.

**The target Noether for the next 6–12 months is:**

- A sidecar that an engineering team can deploy in an afternoon (pre-built binary, systemd unit, single `--api-key` flag), point their agents at, and trust to enforce budgets, route approvals, and produce an honest decision log.
- Operable by a small ops team with a runbook, a `/metrics` endpoint, JSON logs, and a backup procedure.
- Integratable from any of the three SDKs without re-reading the source. The SDKs publish to npm, PyPI, and crates.io with retries, typed errors, and authentication.
- Auditable: the `approval_audit` surface tells you who approved what *with attribution you trust* (caller-key → fixed identity), not whatever subject string the caller chose to send.
- Honest about what it is not: not multi-tenant inside a single instance, not a Pi-level identity broker, not a substitute for an IAM. The product surface should *show* those boundaries, not hide them.

**Constraints the plan respects.** No functional regressions in existing UI, CLI, HTTP, or SDK surfaces. Stack stays Rust + vanilla JS frontend + SQLite/Postgres. New tech is introduced only where strictly necessary (e.g. `tower-http` already there, just unused for `cors`/`limit`/`auth`) and called out explicitly.

---

## 3. Where main stands today, by dimension

| Dimension | Main state | Net resolved | Outstanding focus |
|---|---|---|---|
| arch-modularity | mostly-ready, minor gaps | 4 of 18 (P0 panic, Backend god-file gone, simulation layering) | `ledger.rs` is now 10,934L; `cli.rs` still embeds ~3,350L of HTML |
| server-correctness | significant gaps | 2 P0 → resolved (mutex poison, PG cold cache) | reqwest timeout, error-content-type, 4xx/5xx discipline |
| server-security | significant gaps | 0 code-level; IAP/systemd improve narrative posture | no auth in process; new approval-audit endpoint is unauthenticated |
| storage-sqlite | critical blockers confirmed | partial; mutex panic risk gone | **authorize/finalize write cycle still not transactional** |
| storage-pg | significantly improved | both P0s resolved; partial TLS | `pg_advisory_xact_lock` per-request is a new tail-latency risk; no statement timeout |
| storage-schema-migrations | minor gaps | 2 of 13 (raw BEGIN/COMMIT, O(n) UPDATEs gone) | init_schema not in transaction; no version-mismatch check |
| policy-engine | significant gaps | 0 | allocation-pool enforcement dead code; doc claims contradict code |
| reliability-rust | mostly-ready | **7 of 13** (two P0s, hot-state refactor) | reqwest no timeout; blocking std::fs in async handlers; new `.expect()` on hot path |
| observability | not-production-ready | partial: approval_audit covers approval flow; PG stage timing added | no /metrics; no JSON logs; SQLite path has zero tracing; policy mutation audit still identity-less |
| config-secrets | mostly-ready | PG password resolved via env; clap env feature wired | server has no auth toggle; rotation story absent |
| testing-coverage | not-production-ready | 0 | **no CI on any branch**; PG tests still `#[ignore]`-gated |
| cli-ux | mostly-ready | 2 of 12 (--db-path/--database-url; report cross-backend) | no --version; no NOET_BIND/POLICY env support; help thin |
| sdk-typescript | unchanged | 0; byte-identical | not published; fail-open silently masks 4xx/5xx; pi-noether re-implemented the patterns instead of importing |
| sdk-python | unchanged | 0; byte-identical | bare `except Exception`; no py.typed; no async; no CI |
| sdk-rust | unchanged | 0; byte-identical | README doesn't compile; 311 transitive deps; not crates.io publishable |
| frontend-app | unchanged | 0; byte-identical | no CSP; "live" panel never subscribes; ⌘K is dead UI; doc/code stack mismatch (vanilla, not React) |
| deployment-packaging | meaningful uplift | 4 of 16 (systemd units, IAP, PG-as-current, sslmode TLS) | no Dockerfile; no binary releases; no --version; no release profile |
| docs-discoverability | major uplift | 4 of 15 (storage-backends, iap-reverse-proxy, runbook, --db-path corrected) | **LICENSE not finalized**; solution-design.md stale; no SDK install path; no changelog |
| privacy-data-mgmt | unchanged | 0 (1 partial: runbook advises against capture) | no retention, no purge, no SAR; redaction is key-name-only; reporting unauthenticated |
| bench-perf | unchanged | 2 of 15 (PG bulk seed; tx API) | no concurrent bench; no UI-query bench; WAL autocheckpoint disabled |
| api-versioning-contract | unchanged | 0; 2 new drifts | OpenAPI now structurally wrong (HealthResponse, approval-audit endpoint not in spec) |
| multi-tenant-rbac | minor partial | 1 of 14 (loopback bind + disclaimer in systemd) | all four P1s survive: no auth, no tenant column, spoofable subject, no per-project query filter |

---

## 4. P0 Catalogue

Eight findings carry P0 severity. Status against main:

| ID | Title | Status | Notes |
|---|---|---|---|
| `MULTI_TENANT_RBAC-1` | Zero authentication on all HTTP routes | **CONFIRMED IN MAIN** | The architectural blocker. Documented as intentional in `company-readiness.md`. Must become at minimum opt-in. |
| `CLI_UX-1` | `--db-path` flag mismatch with docs | RESOLVED in main | ServeArgs now uses `--db-path`; `--database-url` is the PG variant. |
| `CONFIG_SECRETS-1` | PG credentials in simulation report served over HTTP | RESOLVED (refuted by verifiers; residual P3 path-disclosure persists) | The original P0 premise was overstated; remaining issue is filesystem-path leakage. |
| `RELIABILITY_RUST-1` | `sqlite_conn()` panics when PG backend active | RESOLVED in main | `backend.rs` deleted; `LedgerBackendDriver` dispatch via trait. |
| `RELIABILITY_RUST-2` | `spawn_blocking` JoinError crashes server on hot path | RESOLVED in main | `spawn_sync_ledger_task` maps `JoinError` cleanly. |
| `RELIABILITY_RUST-3` | `HotState` mutex poison cascade | RESOLVED in main | `HotState` removed; PG reloads state per transaction; SQLite uses `tokio::Mutex`. |
| `STORAGE_PG-1` | Capture proxy panics with PG backend | RESOLVED in main | Same fix as `RELIABILITY_RUST-1`. |
| `STORAGE_PG-2` | `HotState` not restored from PG on startup | RESOLVED in main | `AsyncPostgresLedger::connect_with_options` reloads all state; per-transaction reload exceeds what was recommended. |

**One P0 remains: zero authentication.** Everything below is gated on closing this or making "no auth" an explicit, narrow, supported configuration.

A second item — `DOCS_DISCOVERABILITY-1` (no LICENSE) — was originally rated P0 and adjudicated down to P1 by verifiers. Treat it as P0 for any external sharing or distribution: without a license the SDKs cannot be published, the binary cannot be redistributed, and external integrators cannot legally use the project.

---

## 5. P1 Catalogue (grouped by theme)

Fifty-five P1 findings. Grouped here by the work they imply.

### 5.1 Identity, auth, and tenant boundary (12)

- `MULTI_TENANT_RBAC-2` No RBAC: policy mutation and reads share access level
- `MULTI_TENANT_RBAC-3` No tenant_id/org_id anywhere in schema or queries
- `MULTI_TENANT_RBAC-4` Policy audit log records no actor identity (partially resolved by `approval_audit` for approval flows only)
- `MULTI_TENANT_RBAC-6` Subject/project are caller-supplied and trusted without validation
- `MULTI_TENANT_RBAC-7` No CORS configuration on any route
- `MULTI_TENANT_RBAC-9` Docs recommend `--bind 0.0.0.0:4040` with no runtime warning when unspecified bind is used (systemd default now 127.0.0.1; runtime warning still absent)
- `MULTI_TENANT_RBAC-11` No per-project query isolation; all reports return entire dataset
- `MULTI_TENANT_RBAC-12` Policy file shared process-global; concurrent users overwrite each other's drafts
- `CONFIG_SECRETS-5` No server-side authentication anywhere
- `PRIVACY_DATA_MGMT-5` Reporting APIs expose all subjects unauthenticated
- `SERVER_SECURITY-1` Zero authentication on `/v1/*`
- `SERVER_SECURITY-N1` (new in main) Approval-audit endpoint exposes forged subject/project/rule data unauthenticated

### 5.2 Storage & data integrity (8)

- `STORAGE_SQLITE-1` Multi-statement writes not wrapped in transactions
- `STORAGE_SQLITE-10` `persist_decision` after in-memory mutation → in-memory/DB divergence on persist failure
- `STORAGE_PG-3` `sslmode=prefer` silently falls back to NoTls (partially resolved via native-tls for `require|verify-ca|verify-full`)
- `STORAGE_PG-4` `init_postgres_schema` runs `batch_execute` outside an explicit transaction
- `STORAGE_PG-5` Pool exhaustion blocks indefinitely (no acquisition timeout)
- `STORAGE_PG-6` No `statement_timeout` or `idle_in_transaction_session_timeout`
- `STORAGE_PG-NEW-1` (new in main) `pg_advisory_xact_lock` per-request creates global serialization point
- `STORAGE_SCHEMA_MIGRATIONS-1` SQLite `init_schema` not transactional

### 5.3 Reliability on the hot path (6)

- `RELIABILITY_RUST-4` Blocking `std::fs` inside async handlers
- `RELIABILITY_RUST-5` `reqwest::Client::new()` — no connect or request timeout (also `SERVER_CORRECTNESS-7`)
- `RELIABILITY_RUST-7` `replay_jobs` map grows unbounded
- `RELIABILITY_RUST-NEW-2` (new in main) `spend_window_projections` `.expect()` on validated policy fields, reachable from authorize hot path
- `SERVER_CORRECTNESS-7` Upstream reqwest client has no timeout (proxy can hang indefinitely)
- `SERVER_SECURITY-2` No rate limiting; trivial DoS on authorize & replay

### 5.4 Observability (5)

- `OBSERVABILITY-1` Core business logic emits zero structured log events
- `OBSERVABILITY-2` No `/metrics` endpoint
- `OBSERVABILITY-3` No request-ID propagation; tower spans not correlated to business events
- `OBSERVABILITY-4` No JSON log format option
- `OBSERVABILITY-5` Policy change audit log has no caller identity (partially resolved by `approval_audit` for approval flow only)

### 5.5 Errors leaving Noether (3)

- `SERVER_SECURITY-5` Internal error messages (including DB error strings, SQL text) leak to HTTP clients
- `SERVER_SECURITY-8` Subject/project caller-supplied and trusted (compounds with approval-audit)
- `CONFIG_SECRETS-6` Non-JSON proxy bodies stored unredacted as plaintext

### 5.6 SDKs (6)

- `SDK_RUST-1` README example does not compile
- `SDK_RUST-2` SDK depends on whole server crate (311 transitive packages)
- `SDK_RUST-3` Not publishable to crates.io (path dep, no metadata)
- `SDK_PYTHON-1` `authorize()` catches bare `Exception` — silently masks programming errors as synthetic decisions
- `SDK_TYPESCRIPT-1` `authorize()` converts HTTP 4xx/5xx into synthetic fail-mode decisions
- `API_VERSIONING_CONTRACT-2` No `/v2` strategy / versioning policy (partial — `rules/update-versioning.md` covers process)

### 5.7 Other (15)

- `DOCS_DISCOVERABILITY-1` No LICENSE (P0-equivalent for external use)
- `DOCS_DISCOVERABILITY-2` `--db-path` flag mismatch (resolved)
- `CLI_UX-7` `noet report` couldn't query PG (resolved in main)
- `CONFIG_SECRETS-2` PG connection string in stdout (refuted; residual path disclosure persists)
- `CONFIG_SECRETS-3` PG TLS hardcoded NoTls (partially resolved)
- `CONFIG_SECRETS-4` No env-var fallback for PG password (resolved)
- `FRONTEND_APP-2` No CSP header — XSS has no server-enforced mitigation
- `INTEGRATIONS-N5` (new) `pi-litellm-production-smoke.md` checklist defined but never executed
- `POLICY_ENGINE-3` `allocation_bucket_available_usd` is dead code; `policy-capability-matrix.md` now overstates this as "Hard enforceable"
- `RELIABILITY_RUST-6` SQLite conn take/restore leak (resolved)
- `RELIABILITY_RUST-8` HotState reservation map growth (resolved)
- `RELIABILITY_RUST-13` HotState not restored from PG on startup (resolved)
- `API_VERSIONING_CONTRACT-1` OpenAPI covers only 4 of 20+ routes
- `TESTING_COVERAGE-1` No CI pipeline anywhere
- `STORAGE_PG-7` Startup backfill UPDATEs inside DDL (resolved)

---

## 6. Cross-cutting themes

These are the patterns. Most of the P0/P1 finding catalog collapses into ~6 themes — fixing the theme closes multiple findings at once.

### Theme A — The trust boundary is a documented hope, not a technical control

**Findings touched:** `MULTI_TENANT_RBAC-1/2/4/6/7/9/11/12`, `CONFIG_SECRETS-5`, `SERVER_SECURITY-1/2/5/8/N1`, `PRIVACY_DATA_MGMT-5`

IAP-at-the-front, systemd loopback-binding, and "operators decide who can reach the box" are now documented in `docs/deployment/iap-reverse-proxy.md` and the company-pilot service files. None of that is enforced by Noether's code. The moment IAP is misconfigured, or someone runs `noet serve --bind 0.0.0.0:4040`, every endpoint is open to the world — including the new approval-audit surface that exposes subject, project, rule_id, and cost. Subject/project come from caller-supplied JSON. The approval audit is built on data Noether cannot verify.

**Collective response:** add `--api-key` as an opt-in bearer-token middleware. Zero behavioral change when omitted (current users unaffected). When set, all `/v1/*` routes require the header. The approval-audit endpoint either requires a separate elevated key, or carries an explicit `attribution_verified: false` field in its response so consumers know the data is trust-delegated. Document `--api-key` as the recommended setting for any deployment that exposes beyond loopback. This single addition closes the headline finding, hardens the new approval-audit surface, and makes IAP the strict outer layer it is sold as.

### Theme B — Storage atomicity is incomplete

**Findings touched:** `STORAGE_SQLITE-1/10`, `STORAGE_PG-3/4/5/6`, `STORAGE_PG-NEW-1`, `STORAGE_SCHEMA_MIGRATIONS-1`, `RELIABILITY_RUST-NEW-2`, `RELIABILITY_RUST-NEW-3`

The SQLite authorize cycle (`persist_limit_windows` → `persist_allocation_buckets` → `persist_decision`) runs as three separate autocommit statements with no surrounding transaction. A crash between steps leaves the ledger inconsistent. Per-request `pg_advisory_xact_lock(...)` on Postgres serializes all writes through a single global lock with no timeout. Pool acquisition has no deadline. `init_postgres_schema` relies on implicit DDL transaction semantics for a multi-statement batch. The new `spend_window_projections` and `BudgetLedger::authorize()` call sites `.expect()` on policy-validated fields, panicking on what should be a `Result`.

**Collective response:** wrap the SQLite write cycle in a single rusqlite `Transaction`. Add `tokio::time::timeout()` around PG pool acquisition. Set `statement_timeout` and `idle_in_transaction_session_timeout` via `apply_postgres_connection_options`. Wrap `init_postgres_schema_async` in explicit BEGIN/COMMIT. Set `lock_timeout` before `pg_advisory_xact_lock` to bound deadlock recovery. Convert the new `.expect()` call sites to `Result` propagation. None of these break callers.

### Theme C — There is no automated quality gate

**Findings touched:** `TESTING_COVERAGE-1/2/6/7/8/9/10`, `DEPLOYMENT_PACKAGING-6`, `SDK_PYTHON-10`, `SDK_RUST-12`, `SDK_TYPESCRIPT-8`, `API_VERSIONING_CONTRACT-8`

No `.github/workflows/`, no `.gitlab-ci.yml`. The 169 Rust `#[test]` + `#[tokio::test]` annotations across `src/`, the `parity_server.rs` matrix, the pi-noether 2,099-line test file, the integration probes — none of it runs anywhere automatically. PG tests are `#[ignore]`-gated. SDK packages have minimal test suites and no CI to run them. Contract tests against the live OpenAPI spec don't exist.

**Collective response:** land a CI pipeline as Phase 1. GitHub Actions, run `cargo test` for both backends (spin up PG in service container), run pi-noether extension tests, run the three integration-probe fixtures as smoke tests, run cargo-audit weekly. Once CI exists, every other finding becomes verifiable — and regression-resistant.

### Theme D — There is no distribution path

**Findings touched:** `DEPLOYMENT_PACKAGING-3/5/8/11/14/15`, `DOCS_DISCOVERABILITY-1/9/11`, `SDK_RUST-3`, `SDK_PYTHON-6`, `INTEGRATIONS-N6`

Adoption requires cloning the repo and running `cargo run`. There is no Dockerfile, no OCI image, no pre-built binary, no `cargo install` path, no published SDK packages (npm/PyPI/crates.io), no `--version` flag, no `[profile.release]` configuration (debug binaries by default), and no LICENSE.

**Collective response:** finalize LICENSE (this is a one-line product decision blocking everything downstream). Add a release profile. Add a `--version` flag and a `build_version` field in `/health`. Build a Dockerfile (multi-stage, distroless) + a GitHub release workflow that produces Linux/macOS binaries for both backends. Extract `noether-contract` as a separate crate so the SDK can have a small dep tree. Publish all three SDKs to their respective registries with auto-versioning tied to the server release.

### Theme E — Observability is silent

**Findings touched:** `OBSERVABILITY-1/2/3/4/5/6/7/8/9/11/12`, `DEPLOYMENT_PACKAGING-8`

The hot path emits no structured log events. There is no `/metrics` endpoint. Request IDs are not propagated. The `/health` endpoint is liveness-only. JSON log format does not exist. OpenTelemetry / OTLP is not wired. The PG path gained `stage_timing` debug-level traces; the SQLite path has none. Deny decisions produce no warn-level log event — the most important enforcement event in the system is silent.

**Collective response:** add a `/metrics` endpoint (Prometheus format — `tower-http` is already in tree, `metrics-exporter-prometheus` is a single dep). Add `tower-http`'s `RequestId` middleware and propagate the header into every tracing span. Add a `--log-format=json|text` flag and switch `tracing-subscriber` accordingly. Emit `info!(decision = ...)` / `warn!(deny = ...)` / `info!(budget_exceeded = ...)` events on every authorize outcome. Expand `/health` with DB ping + version + pool stats. This is a few weeks of work that unlocks every existing-fleet monitoring story.

### Theme F — The SDKs are second-class

**Findings touched:** all of `sdk-*`, `INTEGRATIONS-N6`, `MULTI_TENANT_RBAC-14`

All three SDKs are byte-identical between db-port and main; none have been touched in 19 commits. None are published. The Rust SDK depends on the whole server crate (311 transitive packages). The TypeScript and Python SDKs silently convert HTTP errors into synthetic fail-mode decisions (masking real bugs as policy outcomes). The new pi-noether extension re-implemented the SDK patterns inline (retry, abort propagation, fail-mode surfacing) — a clear signal the SDK is not viable as a library dependency today.

**Collective response:** treat the SDKs as a real product. Extract `noether-contract` crate (drops ~230 packages from Rust SDK transitive deps). Narrow the bare `except Exception` in Python and the catch-all in TypeScript so HTTP errors propagate. Add retries (with jitter). Add typed domain models in Python. Add `py.typed`. Add a publish pipeline. Add an `apiKey` parameter to all three (unblocks Theme A wiring). Add CI-run SDK tests.

---

## 7. Phased Execution Roadmap

Twelve phases, ordered foundations-before-features. Each phase is a self-contained workflow chunk. None breaks existing callers unless flagged. Phases marked **decision required** introduce user-visible behavior or new tech and need explicit sign-off before they run.

The executive direction in §1 intentionally groups the detailed phases into fewer decision buckets:

| §1 direction bucket | Detailed phases |
| --- | --- |
| 1. Claim safety | Phase 1, plus the already-landed release/lifecycle delta noted in §1.1 |
| 2. Hot-path trust | Phases 2 and 3 |
| 3. Explicit trust boundary | Phases 4 and 6 |
| 4. Operability | Phase 5 |
| 5. Honest integration claims | The smoke-evidence items in Phase 12 |
| 6. Architecture where confidence needs it | Phase 8 first; larger `ledger.rs`, `cli.rs`, and `server.rs` splits after contract tests |
| 7. Policy/API contract reality | Phases 9 and 10 |
| Deferred unless target changes | Phase 11 and the optional/non-pilot parts of Phase 12 |

### Phase 1 — Foundations: CI, LICENSE, release engineering basics

- Land GitHub Actions: `cargo test` on both backends (PG via service container), pi-noether `npm test`, integration-probe smoke, `cargo audit` weekly.
- Add `LICENSE` file (decision required: pick MIT/Apache-2.0/dual/etc.).
- Add `[profile.release]` to `Cargo.toml`.
- Add `--version` flag (clap auto-wired from `CARGO_PKG_VERSION`).
- Add `build_version` field to `HealthResponse` and update `openapi.rs`.

Closes: `TESTING_COVERAGE-1`, `DEPLOYMENT_PACKAGING-6`, `DEPLOYMENT_PACKAGING-8`, `DEPLOYMENT_PACKAGING-14`, `DEPLOYMENT_PACKAGING-15`, `DOCS_DISCOVERABILITY-1`, `CLI_UX-3`, `API_VERSIONING_CONTRACT-12`. Unlocks every later phase.

**Decision required:** license choice; CI hosting choice (GitHub Actions assumed).

### Phase 2 — Storage atomicity and timeout discipline

- Wrap SQLite authorize cycle in `rusqlite::Transaction`.
- Wrap SQLite finalize cycle in same.
- Wrap `init_postgres_schema_async` in BEGIN/COMMIT.
- Add `tokio::time::timeout()` around PG pool `connection.lock().await`.
- Add `statement_timeout` and `idle_in_transaction_session_timeout` via `apply_postgres_connection_options` (env vars `NOET_POSTGRES_STATEMENT_TIMEOUT`, `NOET_POSTGRES_IDLE_TX_TIMEOUT`).
- Add `SET lock_timeout = 'N'` before `pg_advisory_xact_lock`.
- Convert `spend_window_projections` and `BudgetLedger::authorize()` `.expect()` to `Result` propagation.

Closes: `STORAGE_SQLITE-1`, `STORAGE_SQLITE-10`, `STORAGE_PG-4`, `STORAGE_PG-5`, `STORAGE_PG-6`, `STORAGE_PG-NEW-1`, `STORAGE_SCHEMA_MIGRATIONS-1`, `RELIABILITY_RUST-NEW-2`, `RELIABILITY_RUST-NEW-3`.

### Phase 3 — HTTP layer reliability

- Configure `reqwest::Client::builder().connect_timeout(10s).timeout(600s)` for the proxy client at `server.rs:867`.
- Wrap the streaming loop in `capture.rs:237` with per-chunk `tokio::time::timeout(60s)`.
- Add `NoetError::GatewayTimeout → 504`.
- Add `tower-http::limit::RequestBodyLimitLayer` (1 MiB default, env-configurable).
- Add `tower-http`-based concurrency cap on `/v1/app/replay` (reject 429 if a job is running).
- Add 30-min eviction sweep for `replay_jobs`.
- Replace blocking `std::fs::*` in async handlers (server.rs:2699, 2741, 2777, 2869, 4210, 4220, 4225, 4401, 4406, 4413) with `tokio::fs`.
- Sanitize error response bodies: opaque messages for `Sqlite/Postgres/PostgresTls/Io/Upstream/Url/Method`; keep descriptive for `InvalidPolicy/InvalidConfig/Json/Yaml/NotFound`.

Closes: `RELIABILITY_RUST-4`, `RELIABILITY_RUST-5`, `RELIABILITY_RUST-7`, `RELIABILITY_RUST-10`, `SERVER_CORRECTNESS-7`, `SERVER_SECURITY-2`, `SERVER_SECURITY-5`, `SERVER_CORRECTNESS-NEW-1` (panic→500 not 400).

### Phase 4 — Authentication (the headline)

- Add `--api-key` flag to `noet serve` (also `NOET_API_KEY` env).
- Install bearer-token axum middleware before `/v1/*` routes; constant-time compare; 401 on mismatch.
- Behavior when flag is absent: unchanged (current users unaffected).
- Add `attribution_verified: false` field to `ApprovalAuditReport` JSON.
- Update `docs/deployment/iap-reverse-proxy.md` security checklist to recommend `--api-key` as in-process fallback.
- Add an `apiKey` parameter to all three SDKs and the pi-noether extension.
- Update `examples/deployment/noether-company-pilot{,-postgres}.service` to read `NOET_API_KEY` from `EnvironmentFile`.
- Add a runtime `warn!` when `--bind` is non-loopback and `--api-key` is unset.

Closes: `MULTI_TENANT_RBAC-1`, `MULTI_TENANT_RBAC-9` (warning), `CONFIG_SECRETS-5`, `SERVER_SECURITY-1`, `SERVER_SECURITY-N1` (disclaimer), `MULTI_TENANT_RBAC-14`, `PRIVACY_DATA_MGMT-5` (when key is set), partial `SERVER_SECURITY-8`.

**Decision required:** is `--api-key` the right shape, or do you want IAP-claim-trust (read `X-Goog-IAP-JWT-Assertion` and verify) as the default? The disclaimer-only path on the approval-audit endpoint vs requiring a higher-tier key is also a decision.

### Phase 5 — Observability foundation

- Add `tower-http::request_id::SetRequestIdLayer` + `PropagateRequestIdLayer`. Propagate into every tracing span via `tracing_subscriber::fmt::format::FmtSpan`.
- Add a `metrics-exporter-prometheus` `/metrics` endpoint (decision required: include it inside the `--api-key` auth boundary, or expose separately?).
- Add `--log-format=json|text` flag wiring `tracing-subscriber`.
- Emit `info!(decision = "allow|warn|ask|deny", subject, project, rule, cost_usd)` events on every authorize outcome.
- Emit `warn!(deny = ..., reason)` on every deny.
- Expand `/health`: add `database: { backend, reachable, latency_ms }`, `version`, `uptime_seconds`, `pool: { size, available, waiters }`.
- Add slow-query logging: emit `warn!(slow_query = ..., duration_ms)` for any DB call over 100 ms.
- Add `info!` events to SQLite hot path matching PG `stage_timing`.

Closes: `OBSERVABILITY-1`, `OBSERVABILITY-2`, `OBSERVABILITY-3`, `OBSERVABILITY-4`, `OBSERVABILITY-6`, `OBSERVABILITY-8`, `OBSERVABILITY-9`, `OBSERVABILITY-12`.

**Decision required:** OTLP export path (Phase 5b? deferred to Phase 9?); `metrics-exporter-prometheus` dep addition; auth posture on `/metrics`.

### Phase 6 — Identity attribution for policy mutations

- Add an `actor: Option<String>` parameter to `append_policy_audit` extracted from the authenticated key (if Phase 4 landed) or from a configurable header (`X-Noet-Actor`) as a fallback.
- Apply same to PUT/DELETE/suggestion-apply on `/v1/app/policy/*`.
- Persist actor in the policy_audit JSON record.

Closes: `OBSERVABILITY-5` (full), `MULTI_TENANT_RBAC-4` (full), `MULTI_TENANT_RBAC-5`.

### Phase 7 — Deployment & packaging

- Add multi-stage Dockerfile (build with cargo-chef, ship distroless with binary).
- Add GitHub release workflow: `cargo dist`-style binary builds for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`. Both backends compiled in.
- Add `cargo install` instructions to README.
- Add SIGTERM graceful shutdown: `axum::Server::with_graceful_shutdown(...)` waiting on in-flight requests with 30s deadline.
- Make `wal_autocheckpoint` configurable via `NOET_SQLITE_WAL_AUTOCHECKPOINT` (default to 1000 — current value of 0 is unsafe for shared deployments).
- Add backup/restore documentation: `docs/operations/backup-restore.md` covering `sqlite3 .backup`, `pg_dump`/`pg_basebackup`.

Closes: `DEPLOYMENT_PACKAGING-2`, `DEPLOYMENT_PACKAGING-3`, `DEPLOYMENT_PACKAGING-5`, `DEPLOYMENT_PACKAGING-10`, `DEPLOYMENT_PACKAGING-11`, `DOCS_DISCOVERABILITY-6`.

### Phase 8 — SDK uplift

- Extract `noether-contract` crate (serde/serde_json/chrono/uuid only; no clap, no axum).
- Update Rust SDK to depend on `noether-contract` instead of root crate. Add `#[derive(Default)]` to `AuthorizeRequest` / `FinalizeReservation`. Mark `NoetherClientError` `#[non_exhaustive]`. Fix `url::Url::join` base-URL bug. Add retry with jitter for 5xx.
- TypeScript SDK: narrow `authorize()` catch so HTTP 4xx/5xx propagate as `NoetherHttpError`. Add `apiKey` constructor parameter. Add retry with jitter. Add type-validated response parsers (zod or hand-rolled).
- Python SDK: narrow `except Exception` to `urllib.error.URLError | NoetherError`. Wrap `json.loads` in try/except. Add `py.typed` marker. Add typed dataclass response models. Add `apiKey` parameter. Add `[build-system]` to pyproject.toml.
- Add SDK CI test runs in Phase 1 pipeline.
- **Decision required:** publish path. To npm under `@noether/sidecar`, PyPI as `noether-sidecar`, crates.io as `noether-client`? Pick names; reserve them.
- Add a publish workflow gated on tagged releases.
- Add a per-SDK integration test that runs against a local `noet serve`.

Closes: `SDK_RUST-1`, `SDK_RUST-2`, `SDK_RUST-3`, `SDK_RUST-4`, `SDK_RUST-5`, `SDK_RUST-7`, `SDK_RUST-8`, `SDK_RUST-12`, `SDK_TYPESCRIPT-1`, `SDK_TYPESCRIPT-2`, `SDK_TYPESCRIPT-4` (retries), `SDK_TYPESCRIPT-8`, `SDK_PYTHON-1`, `SDK_PYTHON-2`, `SDK_PYTHON-4`, `SDK_PYTHON-5`, `SDK_PYTHON-6`, `SDK_PYTHON-7`, `SDK_PYTHON-10`, `INTEGRATIONS-N6`, `DOCS_DISCOVERABILITY-9`.

### Phase 9 — API contract discipline

- Add every live `/v1/*` route to `openapi.rs`.
- Add a contract test that fetches `/openapi.json` from a running server and compares against snapshot.
- Split `/v1/app/*` from public `/v1/*` — rename to `/internal/app/*` (or document stability classification clearly).
- Bump `HealthResponse` schema in `openapi.rs` to match the live shape (`ledger_backend`, `postgres_async_finalize_failures`, `version`, `database`).
- Add a JSON Schema for `policy.noet.yaml` and publish at `/v1/policy/schema`.
- Add `deny_unknown_fields` to the policy YAML parser only behind a `--policy-strict` flag (decision required — strict-by-default would break existing policies).
- Add `Deprecation`/`Sunset` headers to 410 routes.
- Wire `info!(api_version = ...)` and add `X-Noether-Version` response header.

Closes: `API_VERSIONING_CONTRACT-1`, `API_VERSIONING_CONTRACT-3`, `API_VERSIONING_CONTRACT-4`, `API_VERSIONING_CONTRACT-7`, `API_VERSIONING_CONTRACT-8`, `API_VERSIONING_CONTRACT-9`, `API_VERSIONING_CONTRACT-10` (resolved by Phase 8), `API_VERSIONING_CONTRACT-11`.

**Decision required:** `/v1/app/*` rename (breaks the frontend slightly — same-origin URL change); `--policy-strict` default.

### Phase 10 — Policy engine correctness + observability of failures

- Wire `allocation_bucket_available_usd` into `apply_budget_limits` so the protected adoption pool actually enforces. Update `docs/policy-capability-matrix.md` once the wiring exists.
- Fix default `warn_at_fraction=1.0` filter so the threshold is not silently dropped.
- Add `validate_rule_match` call in the `policies[]` loop.
- Fix `parse_limit_window` Unicode panic.
- Reject duplicate budget rule IDs at parse time.
- Surface policy reload failures via API status field on `/v1/policy` (currently invisible).
- Make `model.match` use the same wildcard rules as `models.allow`.
- Make the token-to-USD fallback rate configurable.

Closes: `POLICY_ENGINE-1`, `POLICY_ENGINE-2`, `POLICY_ENGINE-3`, `POLICY_ENGINE-5`, `POLICY_ENGINE-6`, `POLICY_ENGINE-8`, `POLICY_ENGINE-12`, `POLICY_ENGINE-13`, `POLICY_ENGINE-NET1`.

### Phase 11 (optional) — Tenant safety opt-in + privacy controls

- Add `tenant_id TEXT NOT NULL DEFAULT 'default'` column to decisions/reservations/events/usage_observations. Default keeps single-tenant deployments transparent.
- Plumb tenant from `--api-key` map (`NOET_API_KEY_TENANTS=key1:tenantA,key2:tenantB`) into every report query.
- Add `noet report purge --before <date>` subcommand for retention.
- Add `--no-capture` flag for the proxy/fixture path. Document recommended deployment posture.
- Extend `src/redaction.rs` to walk nested JSON content (not just key names) — value-based redaction for prompt-like fields.
- Per-caller `replay_jobs` isolation (key-scoped map instead of process-global).
- Per-session policy proposal file (path keyed by api-key hash) — closes the shared-draft overwrite problem.

Closes: `MULTI_TENANT_RBAC-3`, `MULTI_TENANT_RBAC-8`, `MULTI_TENANT_RBAC-11`, `MULTI_TENANT_RBAC-12`, `PRIVACY_DATA_MGMT-1`, `PRIVACY_DATA_MGMT-2`, `PRIVACY_DATA_MGMT-3`, `PRIVACY_DATA_MGMT-4`, `PRIVACY_DATA_MGMT-7`, `PRIVACY_DATA_MGMT-8`, `PRIVACY_DATA_MGMT-9`.

**Decision required:** is single-tenant-per-deployment the durable product stance, or do we want this work now? `docs/company-readiness.md` defers this — Phase 11 inverts that decision.

### Phase 12 (optional) — Frontend polish + integration hardening

- Add `Content-Security-Policy` header to `app_shell` route.
- Wire the live tail panel to the existing SSE endpoint (or remove the "live" affordance and the dead ⌘K UI).
- Fix `decision_mode` escaping (use `textContent`, not `innerHTML`).
- Wire `confirm_replay` flag to actually require confirmation before enforce.
- Debounce the LCS diff (200 ms).
- Cancel in-flight runs filter fetches before issuing a new one.
- Update `README.md` to say "vanilla JS frontend" (or migrate to a tiny framework — but this is the user's call, not the audit's recommendation).
- Execute `docs/testing/pi-litellm-production-smoke.md` and save evidence under `docs/testing/smoke-results-<date>/`. Update integration-readiness-validation. (BLOCK verdict on `deny` failing to prevent provider spend would pause Phase 1 pilot.)
- Add prominent capability-limit warnings in Claude Code and OpenCode integration READMEs.
- Address LiteLLM `asyncio.to_thread` exhaustion (port to async SDK once Python SDK has one).
- Unify env-var prefix across integrations (`NOET_*`).
- HTTPS default in all integrations.

Closes: `FRONTEND_APP-1`, `FRONTEND_APP-2`, `FRONTEND_APP-3`, `FRONTEND_APP-4`, `FRONTEND_APP-5`, `FRONTEND_APP-9`, `FRONTEND_APP-14`, `FRONTEND_APP-16`, `INTEGRATIONS-N2`, `INTEGRATIONS-N3`, `INTEGRATIONS-N4`, `INTEGRATIONS-N5`, `INTEGRATIONS-N8`, `CONFIG_SECRETS-12`.

---

## 8. Extrapolated observations — things you did not explicitly ask about

These were not in the original audit request but matter for the goal. Each is opinionated.

### 8.1 The "trusted boundary" architecture is a product decision, not a temporary state

`docs/company-readiness.md` says explicitly: *"Noether should not implement built-in end-user auth, RBAC, or browser sessions for now."* That's a real strategic stance, defensible for the current target persona. But it has a consequence: until that stance is reversed, the product is a single-trust-boundary deployment — one team, one Noether, one IAP. The roadmap as written above honors the choice through Phase 4 (opt-in `--api-key` is the lightest possible auth without changing the model), and only inverts it in Phase 11. Make that inversion conscious or rename the product positioning. Today the README is positioned for "internal tooling in any enterprise" but the architecture is positioned for "one company pilot at a time."

### 8.2 Supply chain has no published baseline

`cargo audit` has never been run in CI. The Cargo.lock carries 311+ transitive packages. There is no SBOM, no dependency policy, no Renovate/Dependabot config. For a tool that handles credentials and proxies LLM provider calls, this is below industry baseline. **Recommendation:** add `cargo audit` to Phase 1 CI; add a weekly Renovate/Dependabot PR; publish an SBOM at release time (`cargo sbom` or `syft`).

### 8.3 The product surface stack note is wrong

The user's stack description includes "react". The actual frontend is `assets/noether_app/app.js` (~1,215 lines of vanilla ES2022), with React JSX existing only under `docs/design_handoff_noether/prototype/` as design artifacts that are never routed. This is captured as `FRONTEND_APP-1` (P3 after verifier downgrade). **Decision:** keep vanilla JS and update the docs/README to be honest about the stack, *or* commit to a React migration (Phase 12+ scope). Either is fine. Continuing to describe it as React while shipping vanilla is the worst of both.

### 8.4 No formal release process

There is no `CHANGELOG.md`, no versioning policy beyond `rules/update-versioning.md`, no tagged releases on GitHub, no announcement channel. The product is at 0.1.0 in `Cargo.toml` and has been for the audited history. **Recommendation:** as part of Phase 1, adopt a release cadence (every 2 weeks?), enforce semantic versioning, add a `CHANGELOG.md`, and tag releases. SDKs auto-version off server tags.

### 8.5 The approval-audit surface is a governance liability without identity

`src/approval_audit.rs` is real, queryable code that builds risk flags (MissingAttribution, RepeatedSubjectRuleApproval) from `subject`/`project`/`rule_id` fields that any caller can forge. It will be cited in security reviews as evidence of governance posture. Either the data integrity has to become real (caller-key → fixed identity), or the report has to carry an honest disclaimer field. Currently it carries neither. Phase 4 closes this either way.

### 8.6 SQLite at scale needs an operator

`wal_autocheckpoint = 0` (set in `open_sqlite`) disables automatic WAL checkpointing forever. In a single-process single-writer setup that's fine for hours, painful for days, and catastrophic for weeks: the WAL file grows unbounded, fsync amplifies, and recovery time on restart becomes minutes. Combined with no documented backup procedure, no `noet vacuum` or `noet checkpoint` subcommand, and no WAL-size monitoring, the SQLite backend has an operational time bomb. **Recommendation:** add `--wal-autocheckpoint <N>` (default 1000) in Phase 7, add a periodic checkpoint daemon in the serve loop, add a `noet maintain` subcommand for vacuum + reindex + analyze.

### 8.7 No encryption at rest, no encryption-in-transit defaults

SQLite databases hold every authorize/finalize record including subject identifiers, model names, cost data. They are written to the filesystem with default umask, no SQLCipher option, no file-encryption guidance. PG TLS is opt-in via `sslmode=require` rather than default. The bench tool, integration probes, and runbook all use `http://` for callers. For a tool that mediates LLM provider spend with attributed cost data, this is below baseline. **Recommendation:** add a `--db-encryption-key` opt-in for SQLite (SQLCipher behind a feature flag if size matters), make `sslmode=require` the default for PG URLs without an explicit `sslmode` parameter, document HTTPS deployment as required (not just IAP-required), make all SDKs default to `https://` if scheme is omitted.

### 8.8 The Pi extension is doing the SDK's job

`extensions/pi-noether/src/index.ts` is 2,393 lines and re-implements retry, abort propagation, fail-mode surfacing, and HTTP error categorization — patterns that should live in `@noether/sidecar`. It's a clean signal that the SDK is not viable as a library dependency *and* that the patterns are well-understood internally. **Recommendation:** in Phase 8, the SDK extraction work should explicitly look at what the pi-noether extension does and bring those patterns into the SDK. The extension should then import from the SDK, not parallel-implement.

### 8.9 The `cli.rs` HTML rendering is technical debt at scale

`cli.rs` is 4,829 lines, of which ~3,350 are HTML string-templating for the static dashboard export (`noet report dashboard`). It mixes CLI argument parsing concerns with what is effectively a templating subsystem. Adding any feature here (e.g. PDF export, per-tenant dashboards) requires understanding all 3,350 lines. **Recommendation:** lift dashboard rendering into a dedicated `src/dashboard/` module with a small interface (`render_dashboard(LedgerData) -> String`). Phase 14 work, low priority but compounds.

### 8.10 Threat model has never been formalized

There is no `docs/security/threat-model.md` or equivalent STRIDE walkthrough. The "trusted boundary" model is documented in passing, but the actual threat actors, capabilities, and trust assumptions are not written down. **Recommendation:** as part of Phase 4 (auth), produce a one-page threat model: assets (ledger, policy file, approval audit), trust boundaries (IAP, --api-key, network), in-scope threats (forged attribution, replay, DoS, credential theft), out-of-scope (compromise of the host OS, etc.). Cite from the iap-reverse-proxy guidance.

---

## 9. Risk register

Top operational risks if Noether is deployed today as an internal tool without further work:

| # | Risk | Likelihood | Impact | Mitigation phase |
|---|---|---|---|---|
| 1 | IAP misconfiguration → open API to internet | Medium | Critical (full ledger read + policy write) | Phase 4 |
| 2 | SQLite WAL growth → disk pressure or extended restart | High (over weeks) | High | Phase 7 |
| 3 | Reqwest no-timeout → proxy hangs on slow upstream | Medium | High (resource exhaustion) | Phase 3 |
| 4 | Crash mid-authorize → SQLite ledger inconsistency | Low (per write) but cumulative | High (cannot finalize stale reservations) | Phase 2 |
| 5 | Approval-audit data taken as authoritative despite forgeable attribution | High (over time) | High (governance theater) | Phase 4 |
| 6 | Pool exhaustion under PG load → indefinite request hang | Medium | High | Phase 2 |
| 7 | Provider TLS misconfig from `sslmode=prefer` silent NoTls | Medium | High (credentials in plaintext) | Phase 2 |
| 8 | SDK silent fail-mode masks real bug as policy decision | High | Medium | Phase 8 |
| 9 | LLM prompt data accumulated in `events.payload_json` forever | High (over time) | High (privacy/legal) | Phase 11 |
| 10 | No CI → silent regression on next refactor | Certain | Medium-High | Phase 1 |

---

## 10. What "production-ready" means by phase

- **After Phase 1:** Noether is *demo-able* with confidence. CI catches regressions. License lets it be shared. Versions exist.
- **After Phases 2 + 3:** Noether is *stable under unhappy paths*. Crashes don't corrupt. Timeouts bound failure. Hot path doesn't panic.
- **After Phase 4:** Noether is *deployable as an internal tool* with the auth posture you expect of one. The "no auth" stance becomes "no auth unless you opt in", with a clear migration.
- **After Phases 5 + 6:** Noether is *operable* — an on-call engineer can debug a stuck request and a security reviewer can answer "who approved what?".
- **After Phase 7:** Noether is *installable* by someone without a Rust toolchain.
- **After Phase 8:** Noether is *integratable* without needing to read the source.
- **After Phase 9:** Noether is *contract-stable* — SDK and server cannot silently drift.
- **After Phase 10:** Noether is *policy-correct* — the rules match what the docs say.
- **After Phase 11 (if chosen):** Noether is *multi-tenant-safe* in shared deployments.
- **After Phase 12:** Noether is *polished* in the surfaces users see most.

Phases 1 + 4 + 5 alone get Noether from "demo-ready" to "small-team internal tool ready". Phases 2 + 3 + 7 + 8 add the durability and adoption ergonomics that take it to "any team internal tool". Phases 9–12 take it to "ready to share externally or operate at scale".

---

## 11. Acceptance criteria for this audit

A reader of this report should be able to:

1. Open the per-dimension JSON in `.noet/audit-2026-05-30/per-dimension-gap/<dimension>.json` and inspect every finding, its verifier verdicts, and its main-version status.
2. Identify any finding by ID and cross-reference its evidence (file:line) in `main`.
3. Pick any phase from Section 7 and dispatch a focused workflow that closes exactly its listed finding IDs.
4. See which 31 findings are already resolved and not re-do work.
5. Decide consciously about the 2 findings (`SERVER_SECURITY-8`, possibly `API_VERSIONING_CONTRACT-9` if we rename `/v1/app/*`) whose fixes would change user-visible behavior.

If any of those is not true, the audit is incomplete and should be re-run on the specific dimension.

---

## 12. Appendices

### 12.1 Audit artifacts

- `/.noet/audit-2026-05-30/consolidated.json` — raw extraction of all 1,012 sub-agent structured outputs (3.3 MB)
- `/.noet/audit-2026-05-30/per-dimension/<dim>.json` — finder output + verifier verdicts, one file per dimension (80–200 KB each)
- `/.noet/audit-2026-05-30/per-dimension-gap/<dim>.json` — gap-analyzed findings re-rated against `origin/main`, one file per dimension (13–33 KB each)
- `/.noet/audit-2026-05-30/aggregate.json` — schema-tolerant aggregation (522 KB)
- `/.noet/audit-2026-05-30/synthesis_input.json` — slimmed input used by this synthesis (144 KB)
- `/.noet/audit-2026-05-30/extract.py`, `slice.py`, `aggregate.py`, `synthesis_input.py` — extraction tooling

### 12.2 Severity rubric used throughout

- **P0** — prevents safe production deployment in any setting: data loss, security hole, panic on common input
- **P1** — severe for enterprise/team use: no auth, no audit, no observability, broken happy path on second user
- **P2** — important quality gap: testing, docs, polish that bites at scale
- **P3** — polish or nice-to-have

### 12.3 Counts at a glance

```
Total findings                            : 351
  Original (from db-port audit)           : 326
  Net-new (surfaced in main during gap)   :  25

By final severity
  P0                                      :   8 (7 resolved in main; 1 confirmed)
  P1                                      :  55
  P2                                      : 171
  P3                                      :  78
  Dropped / resolved / N/A                :  39

By status in main
  RESOLVED_IN_MAIN                        :  30
  PARTIALLY_RESOLVED_IN_MAIN              :  33
  CONFIRMED_IN_MAIN                       : 214
  NEW_IN_MAIN                             :  17
  NEEDS_MANUAL_REVIEW                     :   6
  BRANCH_ONLY                             :   1
  Unspecified (varied agent schemas)      :  50

By breaks_existing
  No (additive / internal)                : 349
  Yes (user-visible behavior change)      :   2
```

### 12.4 Dimensions by finding count (after gap analysis)

```
sdk-typescript               25
arch-modularity              20
reliability-rust             18
observability                17
server-security              17
storage-pg                   17
storage-sqlite               17
deployment-packaging         16
docs-discoverability         16
frontend-app                 16
policy-engine                16
bench-perf                   15
api-versioning-contract      14
multi-tenant-rbac            14
sdk-rust                     14
server-correctness           14
storage-schema-migrations    14
config-secrets               13
cli-ux                       12
sdk-python                   12
testing-coverage             12
integrations                 11
privacy-data-mgmt            11
```

---

*End of report. Sources of truth are the per-dimension JSON files in `/.noet/audit-2026-05-30/`. The next concrete step is your call: approve Phase 1 to start, or pick a different starting phase from Section 7.*
