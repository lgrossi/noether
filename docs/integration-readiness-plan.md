# Noether integration readiness plan

Noether is a decision sidecar. Integrations call Noether for authorization,
usage finalization, and events; integrations own provider transport.

Noether does not call model providers as part of the production integration
contract.

## Work sequence

1. **OpenAPI-backed API docs**
   - Add a machine-readable OpenAPI spec for the sidecar API.
   - Serve `/openapi.json` and `/docs`.
   - Test that the spec parses, examples deserialize into Noether types, and
     documented endpoints exist.

2. **SDKs**
   - TypeScript SDK.
   - Python SDK.
   - Rust SDK.
   - Each SDK exposes `authorize`, `finalize`, `event`, `health`, and a
     decision helper.
   - SDKs do not call providers.

3. **Pi extension refresh**
   - Align the existing Pi extension with the OpenAPI-backed API and SDK where
     practical.
   - Verify metadata, events, usage finalization, and fail-open/fail-closed
     behavior.

4. **LiteLLM integration**
   - Implement a LiteLLM callback/plugin integration.
   - LiteLLM owns provider calls; Noether authorizes and records outcomes.

5. **Harness integrations**
   - OpenCode.
   - Claude Code.
   - Codex.
   - Each starts with a capability matrix before implementation:
     pre-call authorization, provider/model visibility, usage visibility, and
     event hooks.

## Non-goals

- No Noether provider proxying.
- No provider SDK wrappers inside the Noether server.
- No fake usage accounting when an integration cannot observe usage.
- No client-side policy logic in SDKs.
