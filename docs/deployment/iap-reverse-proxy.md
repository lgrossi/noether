# IAP and reverse-proxy security recipe

Noether should be deployed behind a company-owned security layer. Do not add provider credentials,
user passwords, or session management to Noether itself for the company-pilot path.

## Required boundary

Noether by itself trusts its callers. A company deployment must provide at least one of:

- Identity-Aware Proxy in front of the Noether host;
- authenticated reverse proxy;
- private network plus service-to-service auth for integrations;
- service mesh authorization policy;
- VPN/private access plus host firewall rules.

Do not expose Noether directly to the public internet.

## Access model

Recommended external access groups:

| Group | External-layer access | Reason |
| --- | --- | --- |
| Integration callers | `POST /v1/authorize`, `POST /v1/reservations/*/finalize`, `POST /v1/events`, `GET /health` | Harnesses, SDKs, gateways, and wrappers need hot-path and async ingest access. |
| Operators/readers | `/runs`, `/replay`, `/v1/app/runs*`, `/v1/reports/*`, `/v1/simulations*`, `/simulations` | Usage, decision, replay, and simulation review. |
| Policy maintainers | `/policy`, `/v1/app/policy*`, `/v1/app/replay*` | Can edit drafts, enforce policy, and rollback. |
| Support/docs | `/docs`, `/openapi.json`, `/health` | Keep inside the same boundary unless there is a deliberate internal-docs exposure. |

If the security layer cannot enforce path-based groups, use a single allowlist that includes only the
users and service identities trusted to operate Noether.

## Reverse proxy requirements

The proxy should:

- require company identity before forwarding browser traffic;
- require service identity, mTLS, or equivalent for integration write paths;
- preserve HTTP methods and request bodies;
- support streaming responses for Noether app/report update paths;
- forward `Host`, `X-Forwarded-For`, and `X-Forwarded-Proto` if the company logging layer depends on
  them;
- enforce request size limits appropriate for metadata-first usage;
- log request metadata without storing prompt/body content by default.

## Minimal nginx shape

This is intentionally a shape, not a full company auth configuration:

```nginx
server {
    listen 443 ssl;
    server_name noether.internal.example.com;

    # Company-owned auth goes here: IAP connector, auth_request, mTLS, SSO proxy, or VPN-only access.

    location / {
        proxy_pass http://127.0.0.1:4040;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_buffering off;
    }
}
```

## Minimal Caddy shape

```caddyfile
noether.internal.example.com {
    # Company-owned auth goes here: forward_auth, mTLS, IAP connector, or private-network-only access.

    reverse_proxy 127.0.0.1:4040
}
```

## Security checklist

- Public internet access to Noether is denied.
- Browser access requires company identity.
- Integration write paths are restricted to trusted service identities or trusted network locations.
- Policy mutation routes are restricted more tightly than read/report routes when possible.
- Operators know Noether does not verify caller identity internally.
- Backups and logs are handled under the company's data-retention policy.

