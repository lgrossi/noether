# Noether TypeScript SDK

Dependency-free TypeScript client for the Noether decision sidecar API.

Noether does not call providers. Your integration owns provider transport:

```ts
import { NoetherClient } from "@noether/sidecar";

const noether = new NoetherClient({
  url: "http://127.0.0.1:4051",
  timeoutMs: 1000,
  failMode: "fail_closed",
  apiKey: process.env.NOET_API_KEY,
});

const decision = await noether.requireAuthorization({
  project: "noether",
  subject: "user:local",
  provider: "openai",
  model: "gpt-4.1",
  estimated_tokens: 1500,
  metadata: {
    harness: "my-harness",
    agent_run_id: "run_123",
    request_id: "req_456",
    trace_id: "trace_789",
  },
});

// Your integration calls the provider here.
const providerResult = await callProvider();

await noether.finalize(decision.reservation!.id, {
  actual_cost_usd: providerResult.costUsd,
  usage: providerResult.usage,
  metadata: { trace_id: "trace_789" },
});
```

## Fail modes

- `fail_closed` returns a synthetic deny decision when the sidecar is unavailable.
- `fail_open` returns a synthetic allow decision when the sidecar is unavailable.

Use `requireAuthorization` or `withDecision` when a deny should prevent work from
running.
