# Update and versioning policy

Noether may eventually support auto-update, but agents should preserve this product contract when
designing, documenting, or implementing update behavior.

## Preview versioning

Before `1.0`, Noether uses preview-train versioning rather than strict stable semver:

- `0.0.z` is the auto-updatable preview train.
- `0.y.0` is a manual train bump for intentional contract/default changes.
- Majors are effectively reserved until the first stable release.

Auto-update may apply `0.0.z` releases that include:

- bug fixes;
- critical enforcement, budget, accounting, audit, or security fixes;
- performance and reliability fixes;
- new capabilities only when fully default-off or unreachable by existing config;
- additive internals, CLI/API/config surfaces that do not affect existing installs.

Auto-update must not change what an existing install intentionally does, except when restoring the
documented/intended contract. A previous allow/deny/audit result may change automatically only when
the old result was a bug against that contract.

Manual upgrade is required for:

- changed policy, enforcement, audit, schema, or default behavior;
- enabled-by-default features;
- compatibility boundaries;
- removals or deprecations of accepted policy/config/API behavior;
- migrations that need conscious operator acceptance.

## Stable semver

After `1.0`, Noether should move to normal semver:

- `x` major: breaking policy/enforcement/audit agreement changes; never auto-update.
- `y` minor: compatible new features; auto-update only if the stable update policy explicitly
  allows compatible minors.
- `z` patch: fixes only; auto-update-safe when it preserves or restores the contract.
