# Pi extension integration

Date: 2026-05-15

## Architecture

For Pi subscription-backed usage, Noether does not translate provider protocols or forward provider streams. Pi continues to own provider authentication, provider/model routing, provider-specific request shaping, transport/streaming, response parsing, and session persistence.

The primary integration is a normal Pi extension installed and enabled through Pi's extension/package mechanism:

```text
Pi normal extension
        |
        | before_provider_request
        v
 POST /v1/authorize on local Noether
        |
 allow/warn continues, deny calls ctx.abort()
        |
 after_provider_response/message_end/turn_end/agent_end
        v
 POST /v1/events
 POST /v1/reservations/{id}/finalize
```

The extension package lives at [`extensions/pi-noether`](../../extensions/pi-noether). It is commit-safe: no credentials, no real Pi config, and no captured prompts are stored in the repository.

## Installation and enabling

Pi auto-discovers extensions from:

- `~/.pi/agent/extensions/*.ts`
- `~/.pi/agent/extensions/*/index.ts`
- `.pi/extensions/*.ts`
- `.pi/extensions/*/index.ts`

For local development, either copy/symlink the package into one of those locations or add it to Pi settings as a local package:

```json
{
  "packages": [
    "/absolute/path/to/noether/extensions/pi-noether"
  ]
}
```

Alternatively, point Pi's `extensions` setting directly at the TypeScript entrypoint:

```json
{
  "extensions": [
    "/absolute/path/to/noether/extensions/pi-noether/src/index.ts"
  ]
}
```

Pi settings paths are user-controlled. Noether does not edit `~/.pi/agent/settings.json`, `models.json`, or `auth.json`.

For an ad hoc run, Pi also supports:

```bash
pi --extension "$PWD/extensions/pi-noether"
```

This is useful for local proofing, but the recommended personal setup is the normal Pi extension mechanism so the user can enable or disable Noether in Pi.

## Runtime configuration

Run local Noether:

```bash
cargo run --bin noet -- serve --policy examples/policy.noet.yaml --decision-mode enforce
```

Configure the extension with environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `NOET_URL` | `http://127.0.0.1:4040` | Local Noether API URL. |
| `NOET_PI_PROJECT` | unset | Project metadata sent to `/v1/authorize`. |
| `NOET_PI_SUBJECT` | unset | Subject/user metadata sent to `/v1/authorize`. |
| `NOET_PI_FAIL_MODE` | `fail_open` | Use `fail_closed` to abort provider sends when Noether is unavailable. |
| `NOET_PI_INCLUDE_BODY` | unset | Set to `1` or `true` only to include sanitized body-shaped metadata. |
| `NOET_PI_EXTENSION_VERSION` | `dev` | Version metadata for events/authorization. |

## Privacy posture

The extension is bodyless by default. Its authorization request includes configured `subject` and `project`, provider/model from Pi context when available, estimated context tokens from `ctx.getContextUsage()` when available, and sanitized metadata such as cwd, model API, payload type, top-level payload keys, and shape summaries.

It does **not** send prompt/body content by default. Prompt-like keys such as `messages`, `input`, `instructions`, `prompt`, and `system` are summarized by type/length only.

Body inclusion exists only as an explicit escape hatch:

```bash
NOET_PI_INCLUDE_BODY=1 pi
```

Do not use that mode with real prompts unless the receiving Noether endpoint and retention policy are appropriate.

## Deny behavior

The extension runs on `before_provider_request`, calls Noether asynchronously, and handles outcomes as follows:

- `allow`: return normally and Pi sends the provider request;
- `warn`: return normally and Pi sends the provider request;
- `deny`: call `ctx.abort()` before Pi sends the provider request.

Pi extension errors are not a denial mechanism; Pi catches thrown handler errors and continues. The hard-deny mechanism for this integration is `ctx.abort()`, as validated in [`docs/integrations/pi-wrapper-research.md`](./pi-wrapper-research.md).

Noether unavailability is configurable:

- default: `fail_open` so local development does not break when the sidecar is down;
- strict: `fail_closed` so provider sends are aborted if authorization cannot be obtained.

## Local safe proof

The safe proof script uses only local mock Noether and provider servers plus a mock Pi extension lifecycle. It writes no credentials and cannot reach a real provider:

```bash
npm --prefix extensions/pi-noether run proof:deny
```

The script starts a mock Noether endpoint that returns `deny`, registers the Noether extension against a small mock Pi API, emits `before_provider_request`, and only sends to the mock provider if the extension did not abort. It asserts Noether saw one authorization request, the provider saw zero requests, and prompt text was not sent to Noether.

Generated proof files stay under ignored `.noet/`.

## Known limits

- The extension uses Pi 0.74.0 hook behavior. `ctx.abort()` should remain under regression proof because `before_provider_request` is not documented as a dedicated policy API.
- `after_provider_response` exposes status and headers, not parsed usage. Usage is reported later from assistant messages in `message_end`/`turn_end`.
- User-controlled extension enablement means plain Pi runs without the extension are intentionally outside Noether's local personal setup.
- Usage/reservation matching is sequential and prototype-level. Parallel provider sends would need stronger correlation if Pi adds them.
