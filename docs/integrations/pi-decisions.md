# Pi Stream 1 decisions

## 2026-05-14: Use isolated Pi agent directories instead of editing global config

Decision: Stream 1 did not modify the real `~/.pi/agent/models.json` or `~/.pi/agent/settings.json`. It copied templates and, for the subscription route-through probe, copied credentials into ignored local `.noet/` agent directories.

Rationale:

- the user required backups and minimal additive changes before editing Pi config;
- custom-provider validation did not require global config edits;
- subscription route-through needed a potentially disruptive built-in provider `baseUrl` override;
- isolated `PI_CODING_AGENT_DIR` validation preserved the user's normal Pi setup.

TODO for human review:

- Decide whether Noether should provide an installer/wrapper that applies temporary isolated Pi config, or whether to document an additive global `models.json` snippet.

## 2026-05-14: Do not commit local captures

Decision: Real Pi and Noether captures stayed under `.noet/fixtures/pi-stream1-20260514/`, which is git-ignored.

Rationale:

- fixtures may contain prompts, responses, account metadata, request ids, and usage/cost;
- headers are redacted for obvious secrets, but not all operational metadata is safe to publish;
- docs can describe the shape without committing raw evidence.

TODO for human review:

- Define a sanitizer for Pi/Noether fixtures if these captures should become regression fixtures.

## 2026-05-14: Treat extension-only authorization as weaker than proxy authorization

Decision: Docs classify Pi extension hooks as observation and possible soft-gate mechanisms, not as proven hard budget enforcement.

Rationale:

- `before_provider_request` was validated for payload observation;
- Pi docs describe payload replacement, not a dedicated authorization/deny contract for provider calls;
- hard spend control should fail closed before provider transport, which is clearer in a Noether proxy path.

TODO for human review:

- Build a small Pi extension proof that denies a provider call intentionally, then verify no provider request is sent.

## 2026-05-14: Do not implement Codex Responses compatibility in this stream

Decision: Stream 1 documented the captured `/codex/responses` shape instead of extending `noet` to mock or forward Codex Responses.

Rationale:

- the objective was validation and options documentation;
- implementing provider-specific Codex Responses streaming changes Noether's protocol surface;
- the high-level design says Noether should avoid becoming a broad protocol compatibility layer before the control contract is clear.

TODO for human review:

- Decide whether the next spike should add only enough Codex Responses support for pass-through capture, or a generic event-normalization layer for subscription providers.

