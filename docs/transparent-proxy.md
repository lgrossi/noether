# Transparent proxy mode

Transparent proxy mode lets Noether sit in the transport path for provider-shaped traffic that a harness already knows how to construct. Noether evaluates policy and budget metadata, records redacted fixtures, forwards allowed requests to the original upstream, and returns the upstream response without translating provider protocols.

## Route config

Start with a local YAML file:

```yaml
routes:
  - id: openai
    path_prefix: /providers/openai
    upstream_base_url: https://api.openai.com/
```

`path_prefix` is stripped before forwarding. A request to:

```text
/providers/openai/v1/responses?stream=false
```

is forwarded upstream as:

```text
https://api.openai.com/v1/responses?stream=false
```

Routes can also match a header:

```yaml
routes:
  - id: openai
    header_name: x-noet-provider
    header_value: openai
    upstream_base_url: https://api.openai.com/
```

When both `path_prefix` and `header_name` are set, both must match. `header_value` is optional; if omitted, the header only needs to be present.

## Run

```bash
cargo run --bin noet -- serve \
  --routes noet.routes.yaml \
  --policy examples/policy.noet.yaml \
  --decision-mode dry-run
```

Legacy single-upstream mode still works:

```bash
cargo run --bin noet -- serve --upstream http://127.0.0.1:11434/
```

If neither `--routes` nor `--upstream` is configured, Noether returns local mock responses for capture fixtures.

## Forwarding behavior

Transparent proxy forwarding preserves:

- HTTP method;
- path and query after the wrapper prefix is stripped;
- request body bytes;
- authorization, account, provider-specific, and custom headers except hop-by-hop headers;
- upstream response status, non-hop-by-hop headers, and body bytes.

Noether strips hop-by-hop headers such as `connection`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `te`, `trailer`, `transfer-encoding`, and `upgrade`.

Fixture files redact secret-like request and response headers and credential-like JSON body keys. The actual upstream request and downstream response are not redacted by Noether.

## Policy behavior

Policy decisions run before forwarding:

- `dry-run` records allow/warn/deny decision metadata and still forwards;
- `enforce` denies before any upstream request when the decision outcome is `deny`.

## Current streaming limitation

This slice buffers upstream responses before returning them. That proves byte-preserving non-stream forwarding, but it is not exact streaming pass-through yet.

TODO: forward `reqwest` response byte streams directly to the Axum response body while capturing bounded chunk metadata without delaying the client response.
