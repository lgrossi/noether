# Operations runbook

This runbook is storage-neutral where possible. SQLite is the local/pilot default. PostgreSQL is the
team/company backend for serverless, multi-instance, and company-operated database deployments.

## Health checks

Use:

```bash
curl -fsS http://127.0.0.1:4040/health
```

Expected company-pilot posture:

- `status=ok`
- `policy_loaded=true`
- expected `decision_mode`
- expected route count and upstream posture for the deployment

Run the health check from inside the company security boundary. Do not expose `/health` publicly just
because it does not return prompt content.

## Hot-path monitoring

`POST /v1/authorize` is synchronous in the provider path for governed integrations. Monitor:

- authorization latency p50/p95/p99;
- non-2xx authorization responses;
- sidecar unavailability from integrations;
- fail-open/fail-closed activation count;
- policy deny/warn/ask rates.

For strict integrations, alert on authorization errors because they may block provider work. For
fail-open integrations, alert because provider work may proceed without governance.

## Backup and restore

### Current SQLite pilot

Back up:

- `/var/lib/noet/noether.sqlite`
- SQLite WAL/SHM side files when present;
- `/etc/noet/policy.noet.yaml`;
- `/var/lib/noet/policy.proposed.yaml` when a draft is intentionally retained;
- `/var/lib/noet/policy.previous.yaml`;
- `/var/lib/noet/policy-audit.log`;
- generated simulation artifacts only if they are needed for review history.

Restore by stopping `noet serve`, restoring the files, then starting Noether and checking `/health`,
`/runs`, and `/v1/reports/usage`.

### PostgreSQL backend

Back up through the company Postgres backup mechanism. Keep policy files and local artifact paths in
the Noether host backup set because not all artifacts necessarily belong in the database.

## Retention

Default integrations should be metadata-first and bodyless. Retention policy should cover:

- decisions;
- reservations;
- usage observations;
- trace events;
- policy audit log;
- fixture capture artifacts;
- generated scenario/simulation artifacts.

Treat fixture capture as controlled debug retention. Do not leave central prompt/body capture enabled
unless the company explicitly accepts that data posture.

## Logs

Log:

- Noether process start/stop;
- health-check failures;
- authorization errors and latency;
- integration delivery errors;
- policy enforce/rollback audit entries.

Avoid logging raw prompt/provider bodies by default. If a reverse proxy logs request bodies, disable
that for Noether unless explicitly required and approved.

## Upgrade notes

Before upgrade:

1. Back up the active storage backend and policy files.
2. Record the current Noether commit/version and integration versions.
3. Run `noet policy check` against the active policy.
4. Confirm the company security boundary still points to the intended Noether host.

After upgrade:

1. Check `/health`.
2. Run one allow-path authorization/finalization smoke.
3. Run one deny-path smoke for the primary governed integration.
4. Confirm `/runs` and `GET /v1/reports/usage` still show the expected evidence.

Storage-specific migrations belong to the selected backend adapter. Company-readiness report/domain
logic should remain storage-neutral.
