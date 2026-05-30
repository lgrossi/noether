# Company pilot deployment

This is the supported company-pilot shape for Noether today:

```text
trusted users / trusted integrations
        |
        v
company security layer
(IAP, authenticated reverse proxy, private network, service mesh)
        |
        v
single noet process
        |
        v
durable storage
```

Noether remains intentionally unauthenticated internally. The company security layer decides who can
reach it.

## SQLite start command

SQLite remains the default backend when no `NOET_DATABASE_URL` is configured. Initialize service
config and run one foreground Noether process:

```bash
sudo noet config init --profile server
sudo noet up --config /etc/noet/config.yaml
```

Bind to localhost when a local reverse proxy or IAP sidecar runs on the same host. Bind to a private
interface only when the network boundary already prevents untrusted access.

## PostgreSQL start command

Use PostgreSQL for serverless, multi-instance, or company-operated database deployments:

```bash
NOET_DATABASE_URL='postgres://noether:REDACTED@postgres.internal/noether' \
NOET_POSTGRES_PROFILE=strict \
noet up --config /etc/noet/config.yaml
```

Use `--postgres-profile performance` only when the deployment accepts the durability tradeoffs
documented in [Storage backends](../storage-backends.md).

## Storage selection

| Backend | Status | Use |
| --- | --- | --- |
| SQLite | Current supported backend | Local and early company pilots with one `noet up` process and a durable volume. |
| PostgreSQL | Current supported backend | Serverless, multi-instance, or company-operated database deployments. |

Company-readiness report/domain logic should stay storage-neutral. SQLite and PostgreSQL adapters
provide durable data for those seams.

## Durable paths for the current SQLite pilot

Recommended layout:

```text
/etc/noet/config.yaml             active runtime config
/etc/noet/policy.yaml             active policy managed as config
/var/lib/noet/noet.sqlite         durable ledger
/var/lib/noet/fixtures            controlled debug capture artifacts
/var/lib/noet/simulations         generated simulation artifacts
/var/lib/noet/policy.proposed.yaml local policy draft used by the app
/var/lib/noet/policy.previous.yaml rollback snapshot
/var/lib/noet/policy-audit.log     policy enforce/rollback audit log
```

The SQLite database is the source of truth for SQLite deployments. When PostgreSQL is selected, the
database is the source of truth for decisions, reservations, usage observations, events, budget
windows, and allocation buckets. The `/var/lib/noet` artifact paths still matter for policy drafts,
rollback snapshots, audit logs, fixtures, and generated simulation artifacts.

## Example systemd unit

An SQLite pilot unit is available at
[`examples/deployment/noether-company-pilot.service`](../../examples/deployment/noether-company-pilot.service).

A PostgreSQL pilot unit is available at
[`examples/deployment/noether-company-pilot-postgres.service`](../../examples/deployment/noether-company-pilot-postgres.service).

It assumes:

- the `noet` binary is installed at `/usr/local/bin/noet`;
- policy lives at `/etc/noet/policy.yaml`;
- SQLite durable state lives under `/var/lib/noet`, or PostgreSQL state lives in the configured
  database;
- an external proxy or private network controls access.

## Sensitive route inventory

All routes should be behind the company security boundary. If the external layer supports path-based
groups, treat these as different sensitivity levels:

| Route group | Sensitivity | Notes |
| --- | --- | --- |
| `/v1/authorize`, `/v1/reservations/*/finalize`, `/v1/events` | trusted integration write path | Only harnesses, SDKs, gateways, or controlled wrappers should call these. |
| `/policy`, `/runs`, `/replay`, `/`, `/app/*` | browser app | Includes policy editing, replay, and run evidence surfaces. |
| `/v1/app/policy*`, `/v1/app/replay*` | policy mutation and replay | Highest sensitivity; can save drafts, enforce policy, or rollback. |
| `/v1/app/runs*`, `/v1/reports/*`, `/v1/simulations*`, `/simulations` | reporting/read path | May expose usage, project, subject, model, trace, and event metadata. |
| `/openapi.json`, `/docs`, `/health` | support/read path | Still keep inside the boundary so deployment posture is not leaked publicly. |

## Pilot validation checklist

After deploying behind the company boundary:

1. Confirm unauthenticated public access is impossible from outside the boundary.
2. Confirm an allowed user can load `/policy`, `/runs`, `/replay`, and `/docs`.
3. Confirm `GET /health` returns `status=ok`, the expected `decision_mode`, and
   `policy_loaded=true`.
4. Run one trusted integration authorization through `POST /v1/authorize`.
5. Finalize the returned reservation through `POST /v1/reservations/{id}/finalize`.
6. Confirm `/runs` and `GET /v1/reports/usage` show the finalized run.
7. Save a policy draft, run replay, and enforce only after reviewing the replay result.
8. Confirm the active storage backend is included in the company's backup process. For SQLite, that
   means `/var/lib/noet/noet.sqlite` and its WAL/SHM side files. For PostgreSQL, that means the
   company's PostgreSQL backup mechanism plus Noether policy/artifact paths.

## Supported pilot boundaries

Supported:

- one `noet up` process;
- durable storage using SQLite or PostgreSQL;
- external security boundary;
- trusted callers;
- local-first policy file management;
- bodyless/default integration posture where supported.

Not supported yet:

- built-in Noether auth, RBAC, or browser sessions;
- direct public internet exposure;
- multi-writer SQLite;
- Noether-owned provider routing or provider credential management;
- central blocking human approval queue.
