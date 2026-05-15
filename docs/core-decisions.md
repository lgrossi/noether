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

## 2026-05-15: Transparent proxy routes strip only local wrapper prefixes

Decision: add a local `--routes` YAML contract with `routes[].id`, optional `path_prefix`, optional `header_name`/`header_value`, and `upstream_base_url`. When a path-prefixed route matches, Noether strips that local wrapper prefix and forwards the remaining path/query to the configured upstream.

Rationale: harnesses already construct provider-correct requests and credentials. A small wrapper prefix such as `/providers/openai` gives local routing identity without requiring provider request translation or real credential changes. Header matching supports wrapper setups that cannot or should not change paths.

TODO for human review: decide whether route IDs should be persisted in fixture schema v2 as non-secret provider identity metadata.

## 2026-05-15: Transparent proxy streams upstream responses progressively

Decision: transparent proxy mode returns SSE and chunked/no-`content-length` upstream response bodies progressively while capturing bounded chunk metadata in fixtures after stream completion or failure.

Rationale: buffering streamed agent responses breaks interactive agent UX and changes provider semantics. The proxy must preserve streaming behavior on the hot path, so capture stores chunk summaries asynchronously instead of waiting for the full upstream body before returning data to the client.

TODO: decide whether fixture schema v2 should separate full-body captures from streaming summaries more explicitly.

## 2026-05-15: Pi subscription mode uses a normal extension before proxy route-through

Decision: for Pi subscription-backed runs, make the primary Noether path a normal Pi extension installed/enabled by the user through Pi's extension mechanism, not a `noet pi` launcher wrapper and not a transparent `baseUrl` override.

Rationale: Pi already owns subscription auth, provider routing, request shaping, streaming, and session parsing. The extension can ask Noether for authorization in `before_provider_request`; on `deny` it calls `ctx.abort()` before provider send, while `allow` and `warn` let Pi continue normally. This keeps Noether as a harness-level control plane instead of a provider protocol translation layer.

Fallback: keep transparent proxy route-through and launcher/debug helpers only for deterministic capture and cases where Noether intentionally sits on the HTTP path.

TODO: keep a local deny regression proof for Pi upgrades because `ctx.abort()` is the practical hard-deny mechanism, not a formal provider-policy return value.
