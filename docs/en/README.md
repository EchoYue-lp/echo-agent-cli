# EKO Documentation (English)

This directory is the reviewed English release tree for EKO long-term
documentation. It mirrors `docs/zh/` by relative path; tutorials, operations,
references, architecture, project status, and ADRs keep separate duties.

The staged migration is complete. Removed legacy root paths remain recorded in
[`doc-parity-manifest.json`](../doc-parity-manifest.json) for traceability. Run
the gate before publishing:

```text
node ../../scripts/check-docs-parity.mjs
```

Framework capabilities are documented in the sibling `echo-agent` repository. The cross-language
SDKs, protocol contracts, and Host adapters live in the independent
[`echo-agent-sdk`](https://github.com/EchoYue-lp/echo-agent-sdk) repository; this tree describes EKO
application policy and composition.
