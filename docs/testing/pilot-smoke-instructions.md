# Pilot smoke instructions

These checks are the repeatable pilot evidence package for a company-operated Noether sidecar.
They complement unit/integration tests; they do not replace real installed-tool validation.

## Sidecar readiness

Start Noether with the same storage backend and policy posture used for the pilot:

```bash
NOET_API_KEY='redacted-shared-secret' \
NOET_ACTOR_HEADER='x-goog-authenticated-user-email' \
NOET_LOG_FORMAT=json \
RUST_LOG=info \
noet up --config /etc/noet/config.yaml
```

Record:

- Noether commit and `noet --version` output when available;
- storage backend (`sqlite` path or redacted PostgreSQL DSN identity);
- decision mode;
- policy file checksum;
- integration package/tool versions;
- whether `NOET_API_KEY` is set;
- whether `NOET_ACTOR_HEADER` is set and which proxy injects it;
- which external security boundary is in front of Noether.

Validate:

```bash
curl -fsS \
  -H "Authorization: Bearer $NOET_API_KEY" \
  -H "x-goog-authenticated-user-email: accounts.google.com:alice@example.com" \
  http://127.0.0.1:4051/health

curl -fsS \
  -H "Authorization: Bearer $NOET_API_KEY" \
  -H "x-goog-authenticated-user-email: accounts.google.com:alice@example.com" \
  http://127.0.0.1:4051/metrics
```

Expected:

- `/health` reports `status: "ok"`, `policy_loaded: true`, the expected `ledger_backend`, and
  `auth_configured: true` when `NOET_API_KEY` is configured;
- `/metrics` exposes request, decision, error, and replay counters;
- responses include `x-noet-request-id`.
- if `NOET_ACTOR_HEADER` is configured, missing that header returns a clear `401` naming the
  required header and explaining the proxy/IAP fix.

## Pi deny-before-spend smoke

Use the installed Pi extension and a policy that deterministically denies one request before the
provider send. The strongest non-live-provider regression remains:

```bash
npm --prefix extensions/pi-noether run proof:deny
```

For a real Pi pilot smoke, record:

- Pi version and Noether Pi extension version/config;
- `NOET_PI_FAIL_MODE` and policy mode;
- the deny policy rule used;
- Noether decision/report output for the denied run;
- proof that Pi aborted before provider traffic. When live provider traffic cannot be safely
  observed, use the local provider sentinel proof and state that the live run was not attempted.

Required verdict:

- `SHIP`: deny calls Pi's abort path before provider send, no prompt/body content is sent to
  Noether by default, and usage/events appear for allowed runs where Pi exposes them.
- `ITERATE`: authorization works but usage/self-approval/failure evidence is incomplete.
- `BLOCK`: deny cannot prevent provider traffic in the tested path.

## LiteLLM deny-before-spend smoke

Use the LiteLLM callback integration in the proxy deployment under test.

Record:

- LiteLLM version and callback configuration;
- `NOET_LITELLM_FAIL_MODE`;
- model/provider route under test, with credentials redacted;
- Noether decision/report output for an allow and deny;
- LiteLLM client-visible rejection for deny;
- failure/cancellation finalization evidence if available.

Required verdict:

- `SHIP`: `async_pre_call_hook` calls Noether before provider traffic, deny returns a LiteLLM
  rejection before provider send, success finalizes real usage when LiteLLM exposes it, and failure
  paths do not invent usage.
- `ITERATE`: allow/deny work but usage/failure/privacy evidence is incomplete.
- `BLOCK`: deny cannot prevent provider traffic in the tested callback path.

## Evidence archive

Store redacted evidence under `docs/testing/` with date-stamped filenames. Include:

- command transcript or CI/proof output;
- relevant `/health`, `/metrics`, usage, decisions, and trace reports;
- screenshots only when JSON/text evidence is insufficient;
- explicit limitations and whether the smoke used live provider traffic, a local sentinel, or both.
