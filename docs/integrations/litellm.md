# LiteLLM integration

The LiteLLM integration lives in [`integrations/litellm`](../../integrations/litellm).

Noether remains a decision sidecar:

- LiteLLM owns provider routing and transport.
- The callback calls `POST /v1/authorize` before LiteLLM sends provider traffic.
- A Noether `deny` returns LiteLLM's documented pre-call rejection string.
- Success finalizes observed usage.
- Failure finalizes the reservation with `outcome: "failure"` and records a `litellm.call_failure`
  event.

Use the Noether Python SDK plus `integrations/litellm/noether_litellm.py`; see the integration
README for configuration and environment variables.
