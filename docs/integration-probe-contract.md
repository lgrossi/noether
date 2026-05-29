# Integration probe contract

This contract defines local stub/probe evidence for integrations that are not yet fully governed.
It prevents Noether from overclaiming support while still giving each integration an executable next
step.

## Posture categories

| Posture | Required proof |
| --- | --- |
| Governed | A deny decision happens before provider traffic and the integration proves provider traffic did not occur. |
| Wrapper-gated | A deny decision happens before launching a wrapper-controlled process, but no claim is made about internal provider lifecycle hooks. |
| Tool-governed | A deny decision can block tool/action execution, but not the main model provider call. |
| Observed | Events are reported after the fact; no provider-spend blocking claim is made. |

## Probe result shape

Each probe fixture should answer:

- integration name and version under test;
- installed tool version or mocked public hook version;
- attempted posture;
- whether pre-provider authorization is documented;
- whether deny prevented provider traffic;
- whether usage/cost finalization is reliable;
- whether event-only mode is the honest fallback;
- evidence files or commands used for the proof.

Example fixtures live under [`examples/integration-probes`](../examples/integration-probes).

## Acceptance rule

An integration may only move to a stronger posture when the probe can be run locally without live
provider credentials and still demonstrates the control point. Real installed-tool smoke is still
required before company-pilot readiness.

