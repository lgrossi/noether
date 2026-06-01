# Pilot smoke evidence

Date: 2026-05-31

Branch: `lgrossi/pilot-readiness-auth-operability`

## Scope

This evidence covers repeatable local proof paths for workflow 6. It does not claim that a
customer's live provider credentials were exercised. Live company-pilot validation should still
follow [`pilot-smoke-instructions.md`](./pilot-smoke-instructions.md).

## Pi deny proof

Command:

```bash
npm --prefix extensions/pi-noether run proof:deny
```

Result:

```text
proof ok: deny decision aborted before mock provider request
[noether-pi] Request blocked by policy.
```

Interpretation:

- The extension called Noether before provider send.
- Noether returned `deny`.
- The Pi-side abort path prevented the mock provider request.
- This proves the local regression contract for the Pi extension path without live provider spend.

## Pi extension regression suite

Command:

```bash
npm --prefix extensions/pi-noether test
```

Result:

```text
pi-noether extension tests ok
```

The suite covers deny, fail-open/fail-closed, approval, event delivery, and message formatting
behavior for the extension package.

## LiteLLM integration regression suite

Command:

```bash
python3 -m unittest discover -s integrations/litellm/tests
```

Result:

```text
Ran 5 tests
OK
```

The suite covers pre-call authorization, deny rejection, success finalization, failure
finalization, and usage extraction for the LiteLLM callback integration.

## Limitations

- Pi proof uses a local provider sentinel, not a real subscription-backed provider.
- LiteLLM evidence here is callback-level regression coverage, not a live proxy with provider
  credentials.
- Before a company pilot claims production readiness, repeat the live-tool smoke checklist with the
  deployed policy, sidecar auth mode, storage backend, and provider path.
