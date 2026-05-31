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

### PostgreSQL hot-path tuning

The PostgreSQL backend has Postgres-only tuning knobs. They do not change SQLite
behavior.

| Setting | CLI flag | Default | Notes |
| --- | --- | --- | --- |
| Profile | `--postgres-profile` / `NOET_POSTGRES_PROFILE` | `strict` | `strict` keeps durable finalize and the database default commit mode. `performance` enables async finalize and `synchronous_commit=off`. |
| Connection pool size | `--postgres-pool-size` / `NOET_POSTGRES_POOL_SIZE` | `4` | Uses prepared statements per connection for hot-path writes. |
| Async finalize | `--postgres-async-finalize` / `NOET_POSTGRES_ASYNC_FINALIZE` | `false` | Returns finalize responses after in-memory finalization and queues PostgreSQL persistence. Authorization remains synchronous. |
| Finalize queue capacity | `--postgres-finalize-queue-capacity` / `NOET_POSTGRES_FINALIZE_QUEUE_CAPACITY` | `1024` | If the queue is full or closed, finalize falls back to synchronous persistence. |
| Synchronous commit | `--postgres-synchronous-commit` / `NOET_POSTGRES_SYNCHRONOUS_COMMIT` | database default | Accepts `on`, `off`, `local`, `remote_write`, or `remote_apply`; `off` can reduce tail latency but can lose the latest commits on database crash. |
| Statement timeout | `NOET_POSTGRES_STATEMENT_TIMEOUT_MS` | `30000` | Bounds individual PostgreSQL statements, including time spent waiting on the serialized ledger advisory lock. |
| Idle transaction timeout | `NOET_POSTGRES_IDLE_TX_TIMEOUT_MS` | `30000` | Bounds sessions left idle inside a PostgreSQL transaction. |
| Lock timeout | `NOET_POSTGRES_LOCK_TIMEOUT_MS` | `0` | Disabled by default so legitimate serialized ledger writes can wait for the active writer; set only when deployment needs a shorter lock-wait fail-fast policy. |
| Stage timing | `--postgres-stage-timing` / `NOET_POSTGRES_STAGE_TIMING` | `false` | Emits debug logs for in-memory and database stages. |

Use `strict` for audit-grade budget enforcement. Use `performance` only when the
deployment accepts bounded crash-window drift for lower latency:

```bash
NOET_POSTGRES_PROFILE=performance
```

Performance mode keeps authorization synchronous, but `synchronous_commit=off`
means a database crash can lose writes acknowledged shortly before the crash.
Async finalize is also a latency mode, not a stronger durability mode: finalize
accounting can lag behind the user-facing response and can fall back to
synchronous persistence if the queue is full.

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
