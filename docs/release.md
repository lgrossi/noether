# Release process

Noether core releases are tag-driven. Normal PR and `main` CI validates the release binary, but
only pushed `vX.Y.Z` tags publish artifacts.

## Scope

The current release lane publishes the core `noet` binary only. SDKs, harness extensions, and
package-registry publishing are intentionally out of scope.

## Versioning

Before `1.0`, Noether uses the preview-train rules in `rules/update-versioning.md`:

- `0.y.z` patch releases stay on the current preview train and may be auto-update eligible.
- `0.(y+1).0` starts a manual preview train for intentional contract/default changes.

## Local release check

Run this before pushing a release tag:

```bash
cargo run -p xtask -- release check v0.1.0
```

To create the annotated tag locally:

```bash
cargo run -p xtask -- release tag v0.1.0
git push origin v0.1.0
```

The tag push triggers `.github/workflows/release.yml`, which builds Linux, macOS, and Windows
artifacts, generates checksums and `noether-release.json`, and creates a GitHub prerelease.

## Auto-update manifest

The release workflow emits `noether-release.json`. `noet update check` reads the latest preview
release manifest by default. `noet update apply --yes` replaces the current binary only when the
manifest and version step satisfy the auto-update contract.
