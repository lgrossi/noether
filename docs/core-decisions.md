# Core buildout decisions

## 2026-05-15: Local-only in-memory control tracer bullet

Decision: implement policy, budget, reservations, and event ingest with in-memory state only.

Rationale: the current north star asks Noether to own control contracts and observability without becoming a provider proxy or introducing an external DB. In-memory state keeps the first tracer bullet reviewable and avoids premature persistence choices.

TODO for human review: choose the durable local ledger path once reservation/event shapes settle. SQLite is the likely local-mode default from `docs/solution-design.md`.

## 2026-05-15: Capture dry-run is non-blocking by default

Decision: `noet serve --policy <path>` evaluates capture decisions in `dry-run` mode unless `--decision-mode enforce` is explicitly set.

Rationale: dry-run preserves current capture/mock/upstream behavior and lets policy metadata accumulate in fixtures before spend-blocking behavior is enabled.

TODO for human review: decide whether deployments should require an explicit decision mode when a policy file is provided.

## 2026-05-15: Prompt and response bodies remain retained in local capture fixtures

Decision: capture fixtures keep request and response bodies, with recursive credential-like JSON key redaction.

Rationale: fixture capture exists to learn provider and harness shapes. Dropping bodies now would reduce product-learning value. This remains a local capture-spike behavior, not a central retention default.

TODO for human review: add configurable body retention before shared or central deployments.

## 2026-05-15: Noether is a transparent control proxy first

Decision: Noether's primary integration shape is a transparent or forward control proxy for existing harness/provider traffic, not a provider translation layer.

Rationale: Pi and similar harnesses already know how to build provider-correct paths, headers, bodies, auth, account metadata, and streaming protocols. Noether should leverage that by sitting on the transport path, applying policy/budget/trace/redaction before forwarding, and returning upstream responses without changing provider semantics. Provider emulation remains useful for local mocks and deterministic tests, but it is not the default product direction.

TODO for human review: define the wrapper configuration contract for mapping intercepted provider traffic to original upstream base URLs, including how provider identity is carried without leaking secrets into fixtures.
