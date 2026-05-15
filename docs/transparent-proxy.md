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

Noether strips hop-by-hop headers such as `connection`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `te`, `trailer`, `transfer-encoding`, and `upgrade`. It also strips extension headers named by the `Connection` header.

Fixture files redact secret-like request and response headers and credential-like JSON body keys. The actual upstream request and downstream response are not redacted by Noether.

## Streaming behavior

Streaming upstream responses are passed through progressively. For SSE and chunked/no-`content-length` responses, Noether starts returning upstream body chunks to the client as they arrive instead of waiting for upstream completion. This preserves agent/provider streaming behavior as far as Axum and reqwest expose it.

Streaming fixtures are written after the stream finishes or fails. They store bounded chunk metadata:

- at most the first 128 chunk previews;
- per-chunk byte counts and UTF-8 text previews when available;
- total streamed byte count as a binary body summary;
- an `error` field when the upstream stream fails or the client closes before upstream completion.

## Policy behavior

Policy decisions run before forwarding:

- `dry-run` records allow/warn/deny decision metadata and still forwards;
- `enforce` denies before any upstream request when the decision outcome is `deny`.
