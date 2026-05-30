# Container deployment

Noether publishes a core container image as:

```text
ghcr.io/lgrossi/noether:<version>
ghcr.io/lgrossi/noether:latest-preview
```

The image runs the `noet` binary and uses Linux service paths:

```text
/etc/noet/config.yaml
/etc/noet/policy.yaml
/var/lib/noet/noet.sqlite
```

Logs go to stdout/stderr. Container auto-update is off by default; update the container by pulling a
new image instead of mutating the binary inside the running container.

## Run with Docker

```bash
docker run --rm \
  -p 127.0.0.1:4051:4051 \
  -v "$PWD/examples/deployment/noet-container-config.yaml:/etc/noet/config.yaml:rw" \
  -v "$PWD/examples/deployment/noet-container-policy.yaml:/etc/noet/policy.yaml:rw" \
  -v noet-data:/var/lib/noet \
  ghcr.io/lgrossi/noether:latest-preview
```

## Run with Compose

```bash
docker compose -f examples/deployment/docker-compose.yml up
```

The container entrypoint is equivalent to:

```bash
noet up --config /etc/noet/config.yaml
```
