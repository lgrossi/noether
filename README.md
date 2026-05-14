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
- validates `policy.noet.yaml`;
- evaluates a minimal in-memory fixed-window budget;
- exposes `POST /v1/authorize`, `POST /v1/reservations/{id}/finalize`, and `POST /v1/events`.

It is not a production router yet. The goal is to collect real harness traffic and shape Noether's control contract without taking ownership of provider protocol correctness.

## Run

```bash
cargo run --bin noet -- serve
```

With an upstream:

```bash
cargo run --bin noet -- serve --upstream http://127.0.0.1:11434/
```

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

- [Capture fixture schema v1](./docs/capture-fixtures.md)
- [Control contract v0](./docs/control-contract-v0.md)
- [Policy v0](./docs/policy-v0.md)
