# Overnight Monitor Log

Target repo: `/home/lgrossi/projects/noether`

## 2026-05-14T23:58:29+02:00 - Pass 1

- Repository state:
  - `git status --short`: clean.
  - `git log --oneline --decorate -10`: `58b1712 (HEAD -> main) Add Noether north-star docs`, `1f5450a Initial Noether capture sidecar`.
  - Current files include `src/main.rs`, `Cargo.toml`, `Cargo.lock`, and north-star docs under `docs/`; no `examples/` directory yet.
- Stream 1 Pi validation:
  - Session: `/home/lgrossi/.pi/agent/sessions/--home-lgrossi-projects--/2026-05-14T21-57-19-887Z_0b8c37e7-cb1f-44f1-91a2-202a3723da62.jsonl`
  - Pane `%248` is present and running `pi` in `/home/lgrossi/projects/noether`.
  - Status: active. Pane shows ongoing Pi/model/config validation work, including Pi docs inspection, `pi --version`, `pi --help`, and offline model listing checks.
  - Action taken: none; lane is moving.
- Stream 2 core buildout:
  - Session: `/home/lgrossi/.pi/agent/sessions/--home-lgrossi-projects--/2026-05-14T21-57-39-050Z_2c895caa-f94b-490c-ac56-7eb1d5288baf.jsonl`
  - Pane `%249` is present and running `pi` in `/home/lgrossi/projects/noether`.
  - Status: active. Pane shows early repository inspection and implementation/TDD skill setup for the requested product-core slice.
  - Action taken: none; lane is moving.
- Decisions:
  - No steering needed on first pass because both lanes are active and have not claimed completion.
- TODO for human review:
  - None yet.

## 2026-05-15T00:28:57+02:00 - Pass 2

- Repository state:
  - `git status --short`: `?? docs/overnight-monitor-log.md`.
  - `git log --oneline --decorate -10`: `3f0d4c9 (HEAD -> main) docs(pi): document noether integration findings`, `58b1712 Add Noether north-star docs`, `1f5450a Initial Noether capture sidecar`.
- Stream 1 Pi validation:
  - Pane `%248` was still open.
  - Status: claimed completion.
  - Evidence: commit `3f0d4c9 docs(pi): document noether integration findings`.
  - Files added: `docs/integrations/pi.md`, `docs/integrations/pi-subscription-findings.md`, `docs/integrations/pi-options.md`, `docs/integrations/pi-decisions.md`, `examples/pi/models.noether.json`.
  - Lane reported validation: `jq empty examples/pi/models.noether.json`, `cargo fmt`, `cargo clippy -- -W clippy::all`, `cargo test`, `cargo build`.
  - Lane reported no `/login` or `/logout`, no edits to real Pi config, matching baseline/final Pi config hashes, and no leftover `noet`/`pi` processes.
  - Action taken: independent completion check deferred to final pass.
- Stream 2 core buildout:
  - Original pane `%249` was missing.
  - Original session file existed but had not progressed beyond early repository inspection.
  - Status: failed/incomplete.
  - Action taken: spawned continuation lane instead of closing original lane.
  - Continuation session: `/home/lgrossi/.pi/agent/sessions/--home-lgrossi-projects--/2026-05-14T22-29-26-550Z_86a12e66-50fe-4fd0-a83f-f1e8b6c2e12d.jsonl`
  - Continuation pane: `%251`
- Decisions:
  - Original Stream 2 was treated as failed because its pane disappeared before code changes or commits.
  - Continuation prompt explicitly referenced the original session and required the same acceptance criteria.
- TODO for human review:
  - Audit whether Stream 1 docs answer subscription route/control/observe clearly.
  - Audit Stream 2 continuation commit when it claims completion.

## 2026-05-15T06:19:23+02:00 - Recovery and final completion check

- Repository state:
  - `git status --short`: `?? docs/overnight-monitor-log.md`.
  - `git log --oneline --decorate -8`: `06b281f (HEAD -> main) feat(core): add policy budget decision tracer bullet`, `3f0d4c9 docs(pi): document noether integration findings`, `58b1712 Add Noether north-star docs`, `1f5450a Initial Noether capture sidecar`.
- Stream 1 Pi validation:
  - Status: complete.
  - Verified objectives:
    - Pi config baseline and hashes documented; current hashes still match.
    - Custom provider template exists and validates as JSON.
    - Pi-to-Noether-to-mock validation documented.
    - Subscription-backed `openai-codex/gpt-5.5` normal behavior documented.
    - Subscription route-through probe documented as capture-before-upstream, not full end-to-end forwarding.
    - Explicit route/pre-authorize/observe answers documented.
    - Options and decisions documented.
    - `.noet` captures are not tracked.
    - Process scan found no leftover `noet`/`pi` processes.
- Stream 2 core buildout:
  - Status: complete via continuation lane.
  - Verified objectives:
    - Single-file spike refactored into modules: `cli`, `server`, `capture`, `fixture`, `mock`, `redaction`, `error`, `contract`, `policy`, `ledger`.
    - Capture fixture schema v1 structs, docs, and roundtrip tests exist.
    - Recursive JSON/header redaction exists with nested object/array tests.
    - Provider-neutral control contract v0 types and docs exist.
    - `policy.noet.yaml` parser/validator, `noet policy check`, docs, and sample policy exist.
    - In-memory fixed-window budget evaluator has allow/warn/deny tests.
    - Decision API skeleton endpoints exist with endpoint tests for authorize, finalize idempotency, and events.
    - Capture `--policy` and `--decision-mode dry-run|enforce` exist with tests proving dry-run records deny metadata and enforce blocks before mock/upstream.
    - Fixture inspection CLI exists for `fixtures list`, `fixtures show`, and `fixtures redact-check`.
  - Fresh validation run by monitor:
    - `cargo fmt --check`: pass.
    - `cargo clippy -- -W clippy::all`: pass.
    - `cargo test`: pass, 17 tests.
    - `cargo build`: pass.
    - `cargo run --bin noet -- policy check examples/policy.noet.yaml`: pass.
    - `jq empty examples/pi/models.noether.json`: pass.
- Decisions:
  - Both streams meet their completion bars.
  - The remaining gap is not Stream 1 or Stream 2 product work; it is orchestration bookkeeping and the next product milestone.
- TODO for human review:
  - Review the transparent proxy direction below before broadening provider-specific translation work.

## 2026-05-15T06:44:46+02:00 - Step 3 decision

- User clarified product direction:
  - Noether should be a transparent/forward control proxy first.
  - Existing harness/provider clients, starting with Pi, already know how to shape provider-correct requests and auth.
  - Noether should intercept, analyze, authorize, budget, trace, redact, and forward to the real upstream without becoming a provider translation layer.
- Decision:
  - Step 3 should be a transparent provider-wrapper/proxy lane, not a Codex-provider reimplementation lane.
- Acceptance criteria for Step 3:
  - Define a wrapper configuration that maps intercepted providers to original upstream base URLs.
  - Preserve method, path, query, body, auth/account headers, and streaming response behavior.
  - Apply policy/budget decisions before forwarding.
  - Keep dry-run/enforce capture semantics.
  - Add tests using a local upstream server that prove transparent forwarding and deny-before-upstream behavior.
  - Keep provider protocol parsing minimal and only extract metadata needed for Noether decisions.
- Action taken:
  - Spawned Step 3 lane `noether-step3-transparent-proxy`.
  - Session: `/home/lgrossi/.pi/agent/sessions/--home-lgrossi-projects--/2026-05-15T04-52-24-424Z_68f9cb65-7d94-43df-9b3f-78b970c84144.jsonl`
  - Pane: `%257`
  - Window: `l lllm:1`
