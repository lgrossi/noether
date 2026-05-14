# High-level solution design

## Design stance

Noether defines a canonical **control contract**, not a canonical LLM protocol.

Provider-specific request/response formats are adapter inputs. The core product owns subjects, projects, policies, reservations, usage, traces, events, eval annotations, and reports. This keeps Noether focused on governance and observability instead of becoming another broad protocol compatibility layer.

## Core planes

### 1. Hot-path decision plane

The decision plane is synchronous and must remain small. It answers:

> May this request proceed, under which constraints, and against which reservation?

Candidate API:

```text
POST /v1/authorize
POST /v1/reservations/{id}/finalize
```

The decision plane should support:

- allow, deny, warn, and dry-run decisions;
- budget reservation before the model call;
- reconciliation after actual usage is known;
- explainable policy decisions;
- low latency and predictable failure behavior.

### 2. Async ingest plane

The ingest plane records what happened. It should not block model calls unless explicitly configured by an integration.

Candidate API:

```text
POST /v1/events
```

Event families:

- LLM request started/completed/failed;
- usage and cost observations;
- tool calls;
- harness/session state;
- eval annotations;
- policy decisions;
- retries, fallbacks, and provider errors.

### 3. Ledger and reporting plane

The ledger is the source of truth for reservations, spend, and policy-relevant usage. Local mode can start with SQLite. Central mode should be able to move to Postgres without changing the public control contract.

Reports should answer:

- spend by project/user/team/model/provider;
- denied or warned requests;
- budget burn rate;
- expensive sessions;
- missing attribution;
- policy drift and unsafe integration modes.

## Canonical domain objects

Initial contract objects:

- `Subject`: user, service, team, org, auth principal.
- `Project`: repo, campaign, app, product area, or cost center.
- `RequestContext`: model, provider, estimated tokens, tools, purpose, integration mode.
- `PolicyDecision`: allow, deny, warn, route hint, constraints, explanation.
- `Reservation`: budget hold created before the model call.
- `UsageObservation`: actual tokens, cost, latency, cache usage, stop reason.
- `TraceEvent`: request/session/tool/eval lifecycle event.
- `ToolEvent`: tool name, duration, result metadata, safety labels.
- `EvalAnnotation`: human or automated outcome labels.
- `BudgetLedgerEntry`: reservation, debit, credit, adjustment, expiration.

## Integration modes

### Capture proxy

Current spike. Accepts common provider-shaped paths, records redacted fixtures, and optionally forwards upstream.

Purpose:

- learn real harness request/response shapes;
- build a fixture corpus;
- validate attribution and event contracts;
- avoid premature provider translation scope.

### Enforcement proxy

A gateway or small local proxy calls Noether before forwarding upstream. This is the strongest enforcement mode because Noether can block before spend happens.

### Harness adapter

A harness plugin, wrapper, or local adapter calls Noether with richer workflow context. This gives better tool/session visibility than a proxy but depends on harness integration quality.

### SDK/library

Apps can embed Noether checks directly. This is useful for internal product teams but weaker than a central proxy if adoption is inconsistent.

### Async-only ingest

Clients send usage and trace events after the fact. This is valuable for observability but cannot provide hard budget enforcement.

## Data and privacy defaults

Noether should default to metadata-first operation. Prompt and response body retention must be explicit and scoped.

Recommended posture:

- redact secrets in headers and known credential fields;
- store prompt/response bodies only in local capture spikes or when policy enables it;
- record whether an event was bodyless, redacted, sampled, or full-fidelity;
- make retention configurable by project and deployment mode;
- never require browser cookies or private web-session scraping.

## Deployment model

### Local mode

```text
noet serve
~/.noet/ or .noet/
SQLite
policy.noet.yaml
CLI reports
```

Used for dogfooding, personal budgets, harness experiments, and fixture collection.

### Central mode

```text
noetd
Postgres
service auth
gateway/proxy hooks
audit/export APIs
```

Used by teams that need centralized governance and hard spend control.

## Current implementation boundary

The repository currently contains only the capture spike:

- `noet serve`;
- capture endpoints for `/v1/chat/completions`, `/v1/messages`, and `/v1/responses`;
- mock responses without an upstream;
- optional upstream passthrough;
- redacted fixture files.

The next architectural boundary is to add the control contract without making the capture proxy responsible for perfect OpenAI, Anthropic, or Vertex compatibility.
