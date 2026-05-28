# Storage backends

Noether supports two ledger backends with different deployment guarantees.

## SQLite backend

SQLite is the embedded backend. It is the default when no `NOET_DATABASE_URL` or
`--database-url` is configured.

Use SQLite for:

- local development
- CLI and single-process use
- single-node deployments with a persistent disk
- lowest-latency authorization on the same host

SQLite is not a fit for stateless cloud functions when the function filesystem is
ephemeral. In that environment an empty or reset SQLite file loses budget window
state, active reservations, usage history, and any pending writes.

## PostgreSQL backend

PostgreSQL is the production/serverless backend. Configure it with
`NOET_DATABASE_URL` or `--database-url`.

Use PostgreSQL for:

- stateless cloud functions
- multi-instance deployments
- environments where the ledger must survive instance restarts without a local
  persistent disk
- teams that want one operational database for authorization history and
  reporting reads

PostgreSQL adds a durable write to the synchronous authorization path. Local
benchmarks show that even a minimal durable indexed PostgreSQL write has a
roughly 0.5 ms p50 floor, so PostgreSQL authorization is expected to be slower
than embedded SQLite.

## Compatibility model

The backends are selectable implementation modes, not a SQLite-to-PostgreSQL
replication setup. Both backends persist the same authorization, reservation,
usage, event, and reporting concepts, and server handlers route through the
selected backend.

Operationally:

- SQLite owns state for embedded/single-node mode.
- PostgreSQL owns state for serverless/multi-instance mode.
- Reports and rerun pages read from the configured backend.
- Cloud-function deployments should use PostgreSQL unless a separate stateful
  Noether service owns the hot ledger.

## Choosing a backend

| Deployment | Recommended backend |
| --- | --- |
| Local development | SQLite |
| Single VM with persistent disk | SQLite or PostgreSQL |
| Cloud Run / ECS / Kubernetes service | PostgreSQL for multi-instance, SQLite only for single-node persistent disk |
| Cloud functions / serverless functions | PostgreSQL |
| Lowest possible local authorization latency | SQLite |
| Stateless production with company-operated database | PostgreSQL |
