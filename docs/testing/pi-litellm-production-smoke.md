# Pi and LiteLLM production smoke checklist

This checklist defines the evidence required before calling Pi or LiteLLM company-pilot ready.
Local mocks and unit tests are useful, but they are not enough for the readiness claim.

## Shared setup

Run Noether behind the company boundary using the company-pilot deployment shape. This example uses
SQLite; use `NOET_DATABASE_URL` for PostgreSQL deployments:

```bash
sudo noet config init --profile server
noet up --config /etc/noet/config.yaml
```

Before testing:

- verify `GET /health` returns `status=ok` and `policy_loaded=true`;
- use a policy that can produce one allow, one warn, and one deny;
- confirm body/prompt capture is not enabled unless the deployment deliberately accepts that
  retention risk;
- record the exact Noether commit, integration package version, tool version, policy file, and
  decision mode;
- record the active storage backend. For SQLite pilots, include the database path. For PostgreSQL
  pilots, include the DSN identity without secrets.

## Pi smoke

Required real-tool scenarios:

1. **Allow path**
   - Pi extension is enabled in a normal Pi session.
   - One provider request is authorized before provider send.
   - The run finalizes usage when Pi exposes assistant usage.
   - `/runs` shows the project, subject, provider/model, decision, and usage evidence.

2. **Deny path**
   - Policy produces a deny before provider send.
   - Extension calls Pi's abort mechanism and provider traffic does not happen.
   - Noether records the denial and the reason.

3. **Self-approval path**
   - Configure Pi with user-approved policy behavior.
   - A policy decision requiring approval prompts inside Pi.
   - Approved and rejected outcomes are both recorded as events.
   - Noether reports enough metadata to audit the override later.

4. **Unavailable sidecar path**
   - `fail_open` continues and records/surfaces authorization error evidence when possible.
   - `fail_closed` aborts before provider send.

5. **Privacy path**
   - Prompt/body content is not present in Noether authorization payloads or normal events.
   - Only shape metadata, selected tools, context file names, model information, attribution, and
     usage metadata are present by default.

Evidence to save:

- redacted Noether `/runs` screenshot or JSON;
- `noet report usage --json` output;
- trace report for the tested run;
- Pi extension config and relevant environment variables;
- statement of whether raw debug hook logging was disabled or enabled.

## LiteLLM smoke

Required real-proxy scenarios:

1. **Allow path**
   - LiteLLM `async_pre_call_hook` calls Noether before provider traffic.
   - Provider request succeeds.
   - Success hook finalizes observed usage and cost.

2. **Deny path**
   - Noether deny prevents the LiteLLM provider call.
   - LiteLLM returns a rejection to the client.
   - Noether records the deny decision.

3. **Failure path**
   - Provider or LiteLLM failure finalizes with `outcome: "failure"`.
   - The integration does not invent token/cost usage if LiteLLM did not expose it.

4. **Unavailable sidecar path**
   - `NOET_LITELLM_FAIL_MODE=fail_closed` rejects when Noether cannot be reached.
   - `NOET_LITELLM_FAIL_MODE=fail_open` allows only when that is the deliberate pilot posture.

5. **Privacy path**
   - Message content is not sent to Noether by default.
   - Per-request metadata can carry project, subject, trace id, and estimates.

Evidence to save:

- LiteLLM config with callback enabled;
- Noether usage and decisions reports;
- one successful trace and one denied trace;
- failure/cancellation finalization evidence if available;
- statement of provider/model visibility and any missing usage fields.

## Readiness verdict

Use these verdicts:

- **SHIP for pilot**: all required scenarios pass on the real installed tool/proxy with bodyless
  defaults and clear fail-mode behavior.
- **ITERATE**: core allow/deny paths work, but usage, self-approval, failure finalization, or privacy
  evidence is incomplete.
- **BLOCK**: deny cannot prevent provider spend in the tested integration path, or Noether receives
  prompt/body content by default.
