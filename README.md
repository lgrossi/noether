# Noether

Noether keeps the invariants of LLM usage: budget, policy, evaluation, and observability across harnesses, proxies, and providers.

The command-line tool is `noet`.

## North-star docs

- [Product vision](./docs/product-vision.md)
- [High-level solution design](./docs/solution-design.md)
- [Roadmap](./docs/roadmap.md)

## Current build

This repository currently contains a local sidecar and CLI tracer bullet:

- accepts OpenAI-compatible `/v1/chat/completions`;
- accepts Anthropic-compatible `/v1/messages`;
- accepts OpenAI Responses-style `/v1/responses`;
- redacts sensitive headers and credential-like JSON body keys;
- writes capture fixtures to `.noet/fixtures`;
- returns mock responses when no upstream is configured;
- forwards to an upstream base URL when configured.
- supports transparent provider-wrapper routes that strip a local wrapper prefix and forward to the original upstream without provider translation;
- includes a normal Pi extension package that authorizes provider requests through local Noether before Pi sends them;
- validates `policy.noet.yaml`;
- evaluates a minimal fixed-window budget;
- exposes `POST /v1/authorize`, `POST /v1/reservations/{id}/finalize`, and `POST /v1/events`.
- persists local decisions, reservations, usage, and events to SQLite;
- reports usage, decisions, trace stories, and observations from the local ledger.

It is not a production router yet. The goal is to collect real harness traffic and shape Noether's control contract without taking ownership of provider protocol correctness.

## Current control flow

Noether is a sidecar control plane. A harness, app, or proxy asks before model spend and reports what
happened afterward:

1. Client calls `POST /v1/authorize` with subject/project/provider/model estimates and correlation metadata.
2. Noether evaluates policy and budget, then returns `allow`, `warn`, or `deny`.
3. `allow`/`warn` creates a reservation. `deny` has no reservation and the integration should not call the provider.
4. The integration sends the provider request normally when allowed.
5. After the response, the integration calls `POST /v1/reservations/{id}/finalize` with actual usage/cost.
6. The integration can also send `POST /v1/events` for timeline, tool, usage, and eval observations.
7. `noet report ...` reads the SQLite ledger and shows usage totals, decisions, trace stories, and observations.

For Pi, the primary integration is `extensions/pi-noether`: the extension runs this flow from Pi hooks,
keeps prompts/body content private by default, and propagates `trace_id` / `request_id` for reporting.

## Vertical MVP demo

Run a safe local demo with no provider credentials:

```bash
./examples/vertical-mvp-demo.sh
```

The demo starts `noet serve`, authorizes one bodyless Pi-shaped request, finalizes usage, ingests
request/tool/eval events, and prints:

```bash
noet report usage
noet report decisions
noet report trace <trace_id>
noet report observations
```

It writes its disposable SQLite ledger under `.noet/demo/vertical-mvp.sqlite`.

## Run

```bash
cargo run --bin noet -- serve
```

With an upstream:

```bash
cargo run --bin noet -- serve --upstream http://127.0.0.1:11434/
```

Transparent provider-wrapper routes:

```yaml
# noet.routes.yaml
routes:
  - id: openai
    path_prefix: /providers/openai
    upstream_base_url: https://api.openai.com/
```

```bash
cargo run --bin noet -- serve --routes noet.routes.yaml
```

Requests to `/providers/openai/v1/responses` are forwarded to `/v1/responses` at the configured upstream. Noether preserves method, query, body, auth/account/provider headers, and response status/headers/body except hop-by-hop headers.

Custom fixture directory:

```bash
cargo run --bin noet -- serve --fixture-dir .noet/fixtures
```

Validate a policy:

```bash
cargo run --bin noet -- policy check examples/policy.noet.yaml
```

Run capture with policy decisions recorded but not enforced:

```bash
cargo run --bin noet -- serve --policy examples/policy.noet.yaml --decision-mode dry-run
```

Run capture with deny decisions blocking before mock/upstream:

```bash
cargo run --bin noet -- serve --policy examples/policy.noet.yaml --decision-mode enforce
```

Health check:

```bash
curl http://127.0.0.1:4040/health
```

Mock OpenAI-compatible request:

```bash
curl http://127.0.0.1:4040/v1/chat/completions \
  -H 'content-type: application/json' \
  -H 'authorization: Bearer secret' \
  -d '{"model":"noether-mock","messages":[{"role":"user","content":"hello"}]}'
```

## Fixture shape

Fixtures use schema `noether.capture.v1` and include:

- trace id;
- captured timestamp;
- redacted request method/path/headers/body;
- response source, status, redacted headers, body, and chunks.
- optional decision metadata when `--policy` is configured.

Prompt and response bodies are captured during this spike. Retention and redaction policy will become explicit before any central or shared deployment.

See:

- [Pi extension integration](./docs/integrations/pi-extension.md)
- [Capture fixture schema v1](./docs/capture-fixtures.md)
- [Transparent proxy mode](./docs/transparent-proxy.md)
- [Control contract v0](./docs/control-contract-v0.md)
- [Policy v0](./docs/policy-v0.md)
