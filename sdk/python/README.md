# Noether Python SDK

Dependency-free Python client for the Noether decision sidecar API.

Noether does not call providers. Your integration owns provider transport:

```python
import os

from noether_sidecar import NoetherClient

noether = NoetherClient(
    url="http://127.0.0.1:4051",
    timeout=1.0,
    fail_mode="fail_closed",
    api_key=os.environ.get("NOET_API_KEY"),
)

decision = noether.require_authorization({
    "project": "noether",
    "subject": "user:local",
    "provider": "openai",
    "model": "gpt-4.1",
    "estimated_tokens": 1500,
    "metadata": {
        "harness": "my-harness",
        "agent_run_id": "run_123",
        "request_id": "req_456",
        "trace_id": "trace_789",
    },
})

# Your integration calls the provider here.
provider_result = call_provider()

noether.finalize(decision["reservation"]["id"], {
    "actual_cost_usd": provider_result.cost_usd,
    "usage": provider_result.usage,
    "metadata": {"trace_id": "trace_789"},
})
```

## Fail modes

- `fail_closed` returns a synthetic deny decision when the sidecar is unavailable.
- `fail_open` returns a synthetic allow decision when the sidecar is unavailable.

Use `require_authorization` or `with_decision` when a deny should prevent work
from running.
