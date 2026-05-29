# Audited self-approval

Noether should not make normal AI work wait on a central human approval queue. That creates the wrong
latency and ownership model for agent workflows.

The target model is self-driven approval plus central audit:

```text
policy says ask / override required
        |
        v
harness asks the current operator when it can
        |
        v
operator approves or rejects locally
        |
        v
Noether records the decision evidence
        |
        v
reports surface unusual override patterns
```

## Semantics

- `allow`: proceed and reserve when applicable.
- `warn`: proceed, but record the policy warning.
- `block`: do not proceed when the integration can enforce before spend.
- `ask`: the integration should request local operator confirmation when it has suitable UX.

If the integration cannot collect confirmation, it should fail safe according to its configured
policy mode. For strict company pilots, `ask` without an approval UX should be treated as blocked.

## Evidence to capture

When an integration supports self-approval, Noether events should include:

- original decision id;
- reservation id when one exists;
- policy action requested by Noether;
- operator outcome: approved or rejected;
- decision reason shown to the operator;
- project, subject, trace id, request id, and agent run id where available;
- integration and integration version;
- timestamp.

Prompt or provider body content is not required for audit and should remain omitted by default.

## Audit signals

The useful central review surface is not "pending approvals"; it is exception analysis:

- repeated approvals for the same rule;
- repeated approvals by the same subject;
- high-cost approvals;
- approvals that later finalize much higher than estimated;
- approvals on unattributed or weakly attributed work;
- approvals followed by lifecycle-limit report-only detections;
- rejections that indicate policy is too strict or noisy.

## Product non-goals

- no central approval inbox as the primary workflow;
- no default requirement for a second human to unblock routine AI work;
- no prompt/body retention solely to justify an approval.

