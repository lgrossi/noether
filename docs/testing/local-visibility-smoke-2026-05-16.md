# Local visibility smoke evidence

Date: 2026-05-16

Branch: `main`

## Commands

```bash
./examples/vertical-mvp-demo.sh
cargo run --quiet --bin noet -- report \
  --db-path .noet/demo/vertical-mvp.sqlite \
  dashboard \
  --out .noet/demo/noether-dashboard.html
sqlite3 .noet/demo/vertical-mvp.sqlite \
  "SELECT decision_id, metadata_json FROM decisions;
   SELECT reservation_id, metadata_json FROM usage_observations;
   SELECT kind, payload_json FROM events ORDER BY occurred_at;"
```

## Results

- Demo sidecar started and responded on `/health`.
- Authorization succeeded and created a reservation.
- Finalization updated the reservation to the actual cost.
- Trace events were ingested successfully.
- SQLite data survived reopen through the report commands.
- Usage, decisions, trace, observations, and dashboard outputs all rendered from the demo DB.

## Demo IDs and artifacts

- `trace_id`: `demo-trace-1778945041`
- `decision_id`: `1b417283-2516-4a65-8b89-84827f823470`
- `reservation_id`: `5fb7a4d9-50f1-4aef-87b9-094e2f7ea338`
- DB: `.noet/demo/vertical-mvp.sqlite`
- Dashboard: `.noet/demo/noether-dashboard.html`

## Observed report behavior

- `usage` reported finalized cost `$0.0019` and `1080` total tokens.
- `decisions` reported the selected budget, matched entity, selection reason, model check, and
  remaining budget.
- `trace` reported the end-to-end story in stable tabular order:
  decision, finalized usage, request completion, tool observation, eval annotation.
- `observations` reported the ingested non-decision events in reverse chronological order.
- `dashboard` rendered spend, token, decision, tool/activity, and timeline sections from the same
  DB without any raw hook log dependency.

## Privacy baseline spot-check

SQLite spot-check output from the demo run:

- decision metadata contained only:
  - `body_mode=bodyless`
  - `extension=noether-pi`
  - `harness=pi`
  - `trace_id`
  - `request_id`
- finalized usage metadata contained only:
  - `trace_id`
  - `request_id`
  - `source=noether-demo`
- event payloads contained only the synthetic demo event contents.

No prompt text, provider request body, or response body content was stored in the normal
authorization/finalization path used by this smoke run.

Note: the synthetic `tool.observed` demo event deliberately included
`metadata.command="cargo test --quiet"` because the demo script fabricates that event locally. That
is separate from the bodyless normal Pi authorization path and does not contradict the prompt/body
privacy baseline.
