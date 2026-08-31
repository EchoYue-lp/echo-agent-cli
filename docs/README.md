# EKO Documentation

The EKO documentation set is maintained as two equal release trees:

- [中文主源](./zh/README.md) (`zh`): the editorial source for product facts.
- [English translation](./en/README.md) (`en`): publishable only after semantic review.

The long-term target layout is mirrored by relative path. Architecture,
operations, reference pages, project status, and ADRs keep separate duties;
duplicate current facts are merged, while decision history remains in ADRs.

## Parity gate

Before publishing or synchronizing the website, run:

```text
node scripts/check-docs-parity.mjs
```

The gate is fail-closed. It rejects missing language trees, missing pairs,
unreviewed English pages, ADR identity drift, language-mismatched prose, and
legacy files that are still present after migration.
The migration inventory is [doc-parity-manifest.json](./doc-parity-manifest.json).
The staged migration is complete: all long-term pages now live under the
mirrored `docs/zh` and `docs/en` trees, and the inventory records removed legacy
paths for traceability.

The framework documentation is maintained in the sibling `echo-agent`
repository. EKO documents describe application policy and composition only.
