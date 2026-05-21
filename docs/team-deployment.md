# Team deployment

Noether remains local-first by default. Team deployment is an opt-in operating mode built from the
same `noet serve` process, the same control contract, and the same reporting surfaces now served
from that process.

## Shared server path

Start a shared Noether instance with an explicit bind address, policy path, and durable SQLite
path:

```bash
noet serve \
  --bind 0.0.0.0:4040 \
  --policy /etc/noet/policy.noet.yaml \
  --decision-mode enforce \
  --db-path /var/lib/noet/noether.sqlite \
  --fixture-dir /var/lib/noet/fixtures \
  --simulation-dir /var/lib/noet/simulations
```

Recommended shared-server shape:

- terminate TLS and service authentication in front of Noether;
- expose `/v1/authorize`, `/v1/reservations/{id}/finalize`, and `/v1/events` only to trusted
  callers;
- expose `/v1/reports/*`, `/v1/reports/updates`, `/v1/simulations/*`, `/dashboard`, and
  `/simulations` only behind the same trusted auth/network boundary as the control contract;
- keep `--decision-mode enforce` explicit for shared deployments;
- store the policy file outside the application checkout and deploy it like other config;
- treat fixture capture as a controlled debug path, not a default central retention path.

## Ports and process layout

- default local bind remains `127.0.0.1:4040`;
- a shared instance typically binds `0.0.0.0:4040` behind an internal load balancer or reverse
  proxy;
- one process is enough for the current SQLite-backed implementation; scale-out requires shared
  storage before multiple writers are introduced.

## Fail-mode considerations

Noether itself currently returns decisions; client fail-mode behavior still lives in the caller:

- harnesses like the Pi extension decide whether authorization timeouts/errors are `fail_open` or
  `fail_closed`;
- shared deployments should document which integrations are allowed to fail open and which must
  fail closed;
- central operators should monitor authorization latency because the hot path stays synchronous.

## Storage path beyond SQLite

Current durable seams are the `BudgetLedger` operations:

- `try_authorize`
- `finalize`
- `record_event`
- `usage_report`
- `decisions_report`
- `trace_report`
- `observations_report`

The next storage step should preserve those public behaviors while replacing the backing store.

Recommended migration path:

1. extract a repository/storage trait for decisions, reservations, usage observations, budget
   windows, allocation buckets, and events;
2. keep the reporting HTTP contract and CLI JSON output unchanged while introducing a Postgres-backed
   implementation;
3. preserve backend-independent tests at the authorization/finalization/report level before
   swapping storage in deployment;
4. keep SQLite as the default local backend even after a shared backend exists.

## Trusted-upstream and auth boundary

Noether does not implement end-user identity, browser session auth, or multi-tenant auth today.
For shared deployment, trust is delegated to the upstream caller and the network boundary in front
of Noether.

Trusted metadata inputs today:

- `subject`
- `project`
- `budget_id`
- `entities`
- `trace_id`, `request_id`, and related correlation metadata

Expected trusted upstream injectors:

- the Pi extension;
- controlled SDK/library integrations;
- a deliberate gateway/proxy wrapper operated by the same team.

Security assumptions for shared deployment:

- only trusted upstreams may call Noether directly;
- service-to-service auth, mTLS, or an authenticated reverse proxy sits in front of Noether;
- callers are responsible for truthful attribution metadata;
- the served live dashboard should also sit behind that trusted reverse proxy boundary;
- untrusted browser/mobile clients should not call Noether directly until a stronger auth and
  tenancy story exists.

## Local-first compatibility

Team mode must stay opt-in:

- `noet serve` with no extra flags still binds locally, uses local SQLite, and defaults to
  `dry_run`;
- no cloud service, auth service, or remote DB is required for local development;
- shared-server docs must not replace the existing one-laptop workflow.
