# Noether LiteLLM integration

LiteLLM owns provider routing and transport. Noether is only the decision sidecar:

**Capability limit:** provider-spend prevention depends on the LiteLLM proxy honoring Noether's
authorize decision before it sends to the provider. Keep this callback in the hot path and use
`NOET_LITELLM_FAIL_MODE=fail_closed` for enforcement deployments.

```text
client -> LiteLLM Proxy -> Noether /v1/authorize
client -> LiteLLM Proxy -> provider
client <- LiteLLM Proxy -> Noether /v1/reservations/{id}/finalize
client <- LiteLLM Proxy -> Noether /v1/events
```

## Install locally

Install the Noether Python SDK and expose this package on `PYTHONPATH`, or package both together
for your LiteLLM deployment. When running from this repository, include both `sdk/python` and
`integrations/litellm` on `PYTHONPATH`.

## LiteLLM proxy config

```yaml
model_list:
  - model_name: gpt-4.1
    litellm_params:
      model: openai/gpt-4.1

litellm_settings:
  callbacks: noether_litellm.proxy_handler_instance
```

Configure Noether through environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `NOET_URL` | `http://127.0.0.1:4051` | Noether sidecar URL. |
| `NOET_API_KEY` | unset | Optional bearer token for `NOET_API_KEY`-protected sidecars. |
| `NOET_LITELLM_FAIL_MODE` | `fail_closed` | `fail_open` allows when Noether is unavailable; `fail_closed` rejects. |
| `NOET_LITELLM_TIMEOUT` | `1.0` | Noether request timeout in seconds. |
| `NOET_LITELLM_PROJECT` | unset | Default project sent to `/v1/authorize`. |
| `NOET_LITELLM_SUBJECT` | unset | Default subject sent to `/v1/authorize`. |
| `NOET_LITELLM_BUDGET_ID` | unset | Default budget id sent to `/v1/authorize`. |
| `NOET_LITELLM_ENTITIES` | unset | Comma-separated trusted entities. |

Per-request LiteLLM metadata can override project, subject, trace id, and estimated values:

```json
{
  "model": "gpt-4.1",
  "messages": [{"role": "user", "content": "not sent to Noether"}],
  "metadata": {
    "project": "noether",
    "subject": "user:local",
    "trace_id": "trace-123",
    "noether_estimated_tokens": 1500,
    "noether_estimated_cost_usd": 0.12
  }
}
```

The integration sends prompt/body shape metadata only. It does not send message content to Noether.
