# Naming and path conventions

Use **Noether** for marketable product identity:

- repository and project name;
- GitHub release title;
- package/crate identity where the user is choosing the product;
- container image name, for example `ghcr.io/lgrossi/noether`;
- product docs, diagrams, and copy.

Use **noet** for operational and ergonomic user-facing surfaces:

- binary and CLI commands, for example `noet up`;
- environment variables, for example `NOET_DATABASE_URL`;
- service names, for example `noet.service`;
- config/runtime paths and files;
- local logs, pid files, and database names.

Standard core paths:

```text
~/.noet/config.yaml
~/.noet/policy.yaml
~/.noet/noet.sqlite

.noet/config.yaml
.noet/policy.yaml
.noet/noet.sqlite

/etc/noet/config.yaml
/etc/noet/policy.yaml
/var/lib/noet/noet.sqlite
```

Use YAML for core `noet` configuration and policy files. Existing integration-specific JSON config,
such as the Pi extension's `~/.pi/agent/noether.json`, is compatibility surface and should not be
silently broken while the core `noet` config is introduced.

Avoid creating new `.noether` defaults. Treat existing `.noether` paths as legacy or compatibility
input when migration support is needed.
