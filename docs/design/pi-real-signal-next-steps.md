# Pi real-signal implementation plan

Date: 2026-05-15

## Direction

Noether remains a harness-level control plane for Pi, not a provider router. The standard Pi
extension is the integration point. It authorizes before provider send, then reports summarized
lifecycle/usage/tool observations asynchronously.

The real Pi hook logs change the next step in two important ways:

1. `before_provider_request` is rich and reliable enough for hot-path authorization and request
   shape summaries.
2. `after_provider_response` is not reliable for the observed Pi/provider path and should not be a
   normal-path dependency.

Noether must not degrade Pi UX. Only the authorization decision is allowed on the hot path, with a
tight timeout and explicit fail mode. All persistence, reporting enrichment, debug logging, and
response-level ingestion must be asynchronous or best-effort.

## Non-negotiable runtime rules

- `before_provider_request` blocks only for `/v1/authorize`.
- Authorization uses a strict deadline. Default remains `fail_open`; `fail_closed` is explicit.
- `message_end`, `turn_end`, `agent_end`, `tool_call`, and `tool_result` must not wait on network
  persistence or SQLite/report completion.
- Raw/debug hook logging is disabled by default and is lowest priority when enabled.
- Under pressure, normal observation events may be dropped or coalesced, but they must not delay Pi.
- Reservation finalization is important but still best-effort from Pi's perspective: enqueue, retry,
  and report failure later rather than delaying the user.

## Correlation model

Replace the single mutable `activeRequest` with explicit span correlation. A new provider request
can start before the prior `toolResult`, `turn_end`, or `agent_end` completes, so current
`activeRequest` attribution is unsafe.

Use additive fields in authorization metadata, events, and finalization metadata:

- `trace_id`: Noether story/run trace. Generate once per session/agent run where possible.
- `session_id`: Pi-native id if exposed; otherwise extension-generated.
- `agent_run_id`: extension-generated per agent run.
- `provider_call_id`: current `request_id`, generated per `before_provider_request`.
- `decision_id`
- `reservation_id`
- `turn_index` / derived `turn_id`
- `message_id` or provider `responseId` when available.
- `tool_call_id`
- `attribution_status`: `exact`, `fallback`, or `unmatched`.

Extension state should be a small set of maps, not a broad tracing framework:

- `providerCallsById: Map<provider_call_id, ProviderCallSpan>`
- `providerCallByResponseId: Map<responseId, provider_call_id>`
- `providerCallByToolCallId: Map<toolCallId, provider_call_id>`
- `turnByToolCallId: Map<toolCallId, turn_id>`
- a bounded recent-span list only as fallback for hooks without stable ids.

Attribution order:

1. explicit `provider_call_id`;
2. `responseId` / message id;
3. `toolCallId`;
4. bounded latest-open fallback marked `attribution_status: "fallback"`;
5. otherwise emit with `attribution_status: "unmatched"`.

## Hook ingestion decisions

### `before_provider_request`

Keep this as the canonical pre-send hook.

Hot path:

- build bodyless authorization request;
- call `/v1/authorize` with timeout;
- apply `allow`/`warn`/`deny`;
- return immediately.

Async enqueue after the decision:

- `request.started` or `pi.provider_call.started`;
- authorization summary event;
- request shape summary.

Store/summarize:

- provider/model/model API;
- estimated tokens, context window, context usage percent;
- top-level payload keys;
- OpenAI Responses-shaped request metadata:
  - `input_count`;
  - input item type histogram: message, reasoning, function_call, function_call_output;
  - `tools_count`;
  - `tool_choice` shape;
  - `reasoning.effort`;
  - `text.verbosity`;
  - `include` keys;
  - `parallel_tool_calls`;
  - `store`;
  - `prompt_cache_key_present`, never the value.

Do not store prompt text, instructions, function outputs, tool arguments, cookies, auth headers, or
raw request bodies by default.

### `after_provider_response`

Remove from the normal extension path and docs. It did not fire for the real Pi/provider path and
should not be needed for finalization or reports.

If future debug work reintroduces it, treat it as optional debug signal only.

### `message_update`

Do not persist one event per delta in normal mode.

Use it only to accumulate in-memory stream summaries per provider call/message:

- counts by delta type: `toolcall_start`, `toolcall_delta`, `toolcall_end`, `text_start`,
  `text_delta`, `text_end`;
- first/last stream timestamps;
- tool call ids/names if exposed without bodies;
- content shape only.

Emit at most one summarized `pi.stream_summary` event at `message_end` or provider span close.

### `message_end`

Use as the canonical finalized provider-call result.

Enqueue, without waiting:

- `pi.message_end`;
- reservation finalization command;
- optional `pi.stream_summary`.

Extract:

- assistant provider/model/API;
- `responseId` / message id;
- stop reason;
- input/output/total tokens;
- cache read/write tokens;
- cost breakdown;
- content shape;
- tool calls with `tool_call_id`, name, and argument shape only.

Only assistant messages with usage finalize reservations. Non-assistant messages can be lifecycle
observations but must not finalize usage.

### `turn_end` and `agent_end`

Use for boundaries and reconciliation, never as blocking persistence.

`turn_end` enqueues:

- turn index/id;
- linked provider call if resolvable;
- usage summary if present;
- `toolResults` fallback converted to `tool.observed` using `toolCallId`.

`agent_end` enqueues:

- message count;
- provider call count;
- unmatched/fallback-attributed event counts;
- total observed usage/cost if derivable.

### Tool hooks

Keep `tool_call` and `tool_result` when Pi fires them.

Enqueue:

- `tool_call_id`;
- tool name;
- duration;
- success/error;
- summarized input/content/details shapes.

Never persist command text, file contents, answer text, or raw tool output by default.

## Async queue shape

Implement a small extension-local queue for all non-authorization work:

- bounded size;
- low overhead;
- background flush;
- coalescing for stream summaries;
- drop policy by priority;
- no unhandled promise rejection that affects Pi.

Priority order:

1. authorization decision result metadata already returned by `/v1/authorize`;
2. reservation finalization from `message_end`;
3. `tool.observed`;
4. `pi.turn_end` / `pi.agent_end`;
5. `pi.stream_summary`;
6. raw/debug logs.

Queue items should be typed enough to send either:

- `POST /v1/events`; or
- `POST /v1/reservations/{id}/finalize`.

Retries should be bounded. If retries fail, enqueue/report a later `pi.delivery_failed` event when
possible, but do not block Pi.

## Server-side persistence stance

The Noether server should also protect latency-sensitive paths:

- `/v1/authorize` returns after policy evaluation and reservation creation.
- SQLite persistence must stay fast enough for local MVP, but the design should not require report
  enrichment to finish before responding.
- `/v1/events` and finalization are async-observation surfaces from Pi's perspective. They can write
  durably server-side, but Pi must not be waiting on them in normal hooks.

If server-side write latency becomes visible, add a server-side persistence worker/outbox later.
Do not add that complexity before extension-side non-blocking delivery and correlation are fixed.

## Privacy defaults

Persist by default:

- correlation ids;
- provider/model/API;
- token/cost/latency/stop reason;
- payload and content shape summaries;
- tool names and ids;
- tool input/output shape summaries;
- stream delta counts;
- eval labels/scores.

Raw/debug-only:

- full provider request payload;
- prompt/input/instructions;
- assistant text;
- tool arguments/results;
- raw Pi hook `event`/`ctx`.

Any bodyful mode must mark stored events with `body_mode: "raw_debug"` or
`body_mode: "included"`.

## Report UX changes

Reports should make raw hook logs unnecessary.

### `noet report usage`

Show:

- project, subject, provider, model;
- finalized vs active reservation counts;
- input/output/cache/total tokens;
- total cost;
- source: reservation finalization vs async observation.

### `noet report decisions`

Show:

- trace/session/agent/provider-call ids;
- outcome;
- provider/model;
- estimated tokens/cost;
- reservation id;
- compact request-shape summary: tools count, input count, reasoning effort, text verbosity.

### `noet report trace <trace_id>`

Show a chronological story grouped by agent/session, turn, provider call, and tool call:

- authorization outcome;
- reservation id;
- provider-call start;
- stream summary counts;
- finalized usage/cost;
- stop reason;
- tool observations;
- turn/agent boundaries;
- unmatched/fallback-attributed events.

### `noet report observations --kind tool --trace <trace_id>`

Show:

- tool call id;
- provider call / turn;
- tool name;
- success/error;
- duration;
- summarized input/output shapes.

## Debug hook logging replacement

Remove the temporary broad hook dump from normal config.

Replace with deliberate raw debug mode:

- `NOET_PI_DEBUG_HOOKS=raw`
- optional `NOET_PI_DEBUG_HOOK_LOG_DIR`
- separate files by actual hook name:
  - `before_provider_request.raw.jsonl`
  - `message_update.raw.jsonl`
  - `message_end.raw.jsonl`
  - `turn_end.raw.jsonl`
  - `agent_end.raw.jsonl`

Do not write post-provider lifecycle hooks into `after_provider_response.jsonl`. Do not write startup
marker records into raw log files. Docs must state raw debug logs may include prompt/tool/body data
and are local/delete-after-inspection only.

## Implementation sequence

### 1. Document and fixture the real Pi shapes

Files:

- `extensions/pi-noether/test/noether-extension.test.mjs`
- optional sanitized fixtures under `extensions/pi-noether/test/fixtures/`
- `docs/integrations/pi-extension.md`

Acceptance:

- tests cover OpenAI Responses-shaped `before_provider_request`;
- tests cover `message_update`, `message_end`, `turn_end`, `agent_end` shapes from the real logs;
- normal emitted events contain no raw prompt/body/tool output;
- docs no longer claim `after_provider_response` is normal-path reliable.

Verification:

- `npm --prefix extensions/pi-noether test`

### 2. Make non-authorization delivery asynchronous

Files:

- `extensions/pi-noether/src/index.ts`
- `extensions/pi-noether/test/noether-extension.test.mjs`

Acceptance:

- lifecycle hooks enqueue and return without awaiting `/v1/events`;
- `message_end` enqueues finalization and returns without awaiting finalize HTTP;
- slow/hung event/finalize endpoints do not delay hook completion;
- queue is bounded and has an explicit drop/coalesce policy;
- authorization still has a deadline and honors `fail_open`/`fail_closed`.

Verification:

- extension tests with fake slow/hung fetch;
- tests assert `message_end`, `turn_end`, and `agent_end` complete before deferred HTTP resolves.

### 3. Replace `activeRequest` with provider-call spans

Files:

- `extensions/pi-noether/src/index.ts`
- `extensions/pi-noether/test/noether-extension.test.mjs`

Acceptance:

- provider call B can start before provider call A's `toolResult`/`turn_end`;
- A's usage/finalization remains attributed to A;
- `toolResult` links by `toolCallId`;
- unmatched events are marked `attribution_status: "unmatched"` rather than misattributed;
- fallback attribution is marked `attribution_status: "fallback"`.

Verification:

- regression test for the observed interleaving bug;
- existing deny, privacy, and finalize tests still pass.

### 4. Emit normalized summarized Pi events

Files:

- `extensions/pi-noether/src/index.ts`
- docs examples;
- `src/contract.rs` only if typed usage fields need additive expansion.

Acceptance:

- `before_provider_request` emits summarized provider-call start/authorization data asynchronously;
- `message_update` produces aggregate stream summaries only;
- `message_end` emits finalized usage with cache token/cost details;
- `turn_end` emits boundaries and fallback tool observations;
- `agent_end` emits run summary;
- all events carry correlation ids.

Verification:

- extension tests inspect posted event payloads;
- no normal payload contains raw text, prompts, tool args, or tool output.

### 5. Make reports story-shaped

Files:

- `src/ledger.rs`
- `src/cli.rs`
- `src/server.rs` tests

Acceptance:

- trace reports summarize events instead of dumping raw JSON;
- usage reports include cache tokens/cost when available;
- decisions reports expose correlation ids and request-shape summary;
- observations report supports `--trace`;
- fallback/unmatched attribution is visible.

Verification:

- `cargo fmt`
- `cargo clippy -- -W clippy::all`
- `cargo test`
- `./examples/vertical-mvp-demo.sh`

### 6. Remove temporary hook logging path

Files:

- `extensions/pi-noether/src/index.ts`
- `extensions/pi-noether/test/noether-extension.test.mjs`
- `docs/integrations/pi-extension.md`

Acceptance:

- no raw hook files are created by default;
- debug mode is explicit;
- raw logs are split by actual hook name;
- no `after_provider_response.jsonl` fallback bucket;
- docs clearly mark raw mode unsafe/local/debug-only.

Verification:

- extension tests cover debug disabled/enabled;
- default test run creates no raw logs.

### 7. Real Pi smoke lane

Acceptance:

- run Noether sidecar with SQLite;
- run a real Pi interaction with extension enabled and raw hook logging disabled;
- reports show authorization, reservation, finalized usage/cost, provider-call timeline, stream
  summary, tool observation, and turn/agent boundaries;
- no raw logs are needed to explain the run.

Verification commands:

```bash
cargo run --bin noet -- serve --policy examples/policy.noet.yaml --decision-mode enforce
cargo run --bin noet -- report usage
cargo run --bin noet -- report decisions
cargo run --bin noet -- report trace <trace_id>
cargo run --bin noet -- report observations --kind tool --trace <trace_id>
```

## Out of scope

- provider routing expansion;
- prompt/response retention by default;
- dashboard/Majin UI;
- OpenTelemetry;
- Postgres/central deployment;
- broad policy/budget redesign;
- relying on `after_provider_response` for correctness.
