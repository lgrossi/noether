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
durable SQLite volume
```

Noether remains intentionally unauthenticated internally. The company security layer decides who can
reach it.

## Start command

Run one Noether process with explicit bind, policy, database, fixture, and simulation paths:

```bash
noet serve \
  --bind 127.0.0.1:4040 \
  --policy /etc/noet/policy.noet.yaml \
  --decision-mode enforce \
  --db-path /var/lib/noet/noether.sqlite \
  --fixture-dir /var/lib/noet/fixtures \
  --simulation-dir /var/lib/noet/simulations
```

Bind to localhost when a local reverse proxy or IAP sidecar runs on the same host. Bind to a private
interface only when the network boundary already prevents untrusted access.

## Durable paths

Recommended layout:

```text
/etc/noet/policy.noet.yaml        active policy managed as config
/var/lib/noet/noether.sqlite      durable ledger
/var/lib/noet/fixtures            controlled debug capture artifacts
/var/lib/noet/simulations         generated simulation artifacts
/var/lib/noet/policy.proposed.yaml local policy draft used by the app
/var/lib/noet/policy.previous.yaml rollback snapshot
/var/lib/noet/policy-audit.log     policy enforce/rollback audit log
```

The SQLite database is the pilot source of truth for decisions, reservations, usage observations,
events, budget windows, and allocation buckets.

## Example systemd unit

An example unit is available at
[`examples/deployment/noether-company-pilot.service`](../../examples/deployment/noether-company-pilot.service).

It assumes:

- the `noet` binary is installed at `/usr/local/bin/noet`;
- policy lives at `/etc/noet/policy.noet.yaml`;
- durable state lives under `/var/lib/noet`;
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
8. Confirm the SQLite file is included in the company's backup process.

## Supported pilot boundaries

Supported:

- one `noet serve` process;
- durable SQLite;
- external security boundary;
- trusted callers;
- local-first policy file management;
- bodyless/default integration posture where supported.

Not supported yet:

- built-in Noether auth, RBAC, or browser sessions;
- direct public internet exposure;
- multi-writer or HA Noether server topology;
- Noether-owned provider routing or provider credential management;
- central blocking human approval queue.

