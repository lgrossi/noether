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
  -v noet-config:/etc/noet \
  -v noet-data:/var/lib/noet \
  ghcr.io/lgrossi/noether:latest-preview
```

The named `noet-config` volume persists `/etc/noet/config.yaml` and `/etc/noet/policy.yaml` across
container replacement while keeping them writable by the container's `noet` user. If you override
those files with bind mounts, ensure the mounted files are writable by the container's `noet` user
or bake a derived image with your policy/config copied in with `--chown=noet:noet`.

## Run with Compose

```bash
docker compose -f examples/deployment/docker-compose.yml up
```

The container entrypoint is equivalent to:

```bash
noet up --config /etc/noet/config.yaml
```
