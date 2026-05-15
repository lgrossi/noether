# Pi real-signal smoke evidence

Date: 2026-05-15

Branch: `lgrossi/pi-real-signal-hardening`

## Commands

```bash
npm --prefix extensions/pi-noether test
npm --prefix extensions/pi-noether run proof:deny
cargo fmt
cargo clippy -- -W clippy::all
cargo test
./examples/vertical-mvp-demo.sh
```

## Results

- Extension tests passed.
- Deny proof passed: Noether deny aborted before the mock provider request.
- Rust formatting passed.
- Rust clippy passed with no issues.
- Rust tests passed: 24 tests across 3 suites.
- Vertical MVP demo passed.

## Vertical demo IDs

- `trace_id`: `demo-trace-1778862215`
- `decision_id`: `cd366b26-9f1f-4151-82c4-16d425746706`
- `reservation_id`: `2cc71e73-9c30-481d-966a-556024f5eb99`
- DB: `.noet/demo/vertical-mvp.sqlite`

## Observed report behavior

The demo showed:

- authorization and reservation creation;
- usage finalization to `$0.0019`;
- persisted `request.completed`, `tool.observed`, and `eval.annotation` events;
- story-shaped trace output with decision, finalized usage, request completion, tool observation,
  and eval annotation.

Raw hook logs were not needed for the demo report path.
