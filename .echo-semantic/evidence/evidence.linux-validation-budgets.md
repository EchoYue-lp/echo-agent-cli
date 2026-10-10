---
schema_version: 1
id: evidence.linux-validation-budgets
kind: evidence
observed_at: source:621b3930bfef37174da8309c6fde5c06dd749fd2be9cb4d48fdaf1e8c63628b1
source_refs: [.github/workflows/rust-ci.yml]
supports: [map.eko-framework-consumer-settlement]
limitations: [Exact-head remote acceptance pending; no historical cancellation causality claim]
---

# Independent Linux validation budgets

## 支持的结论

Linux quality checks and default-feature app-core tests now use independent
30-minute job budgets. All original commands, locked/feature flags, framework
main checkout, binding-export isolation, frontend commands and concurrency
settings remain. Aggregate linux-rust fails for failure/cancelled/skipped input.

## 来源与范围

The final workflow exactly matches the previously prepared local CI candidate
6e72a0ad7fc316a638777215ffc84c1b0f9bb60d, without its remote branch's temporary
diagnostics. YAML and all sixteen success/failure/cancelled/skipped combinations
were checked; only success/success passes. The other thread's checkout and
draft PR9 remain unchanged. Compression production sources are unchanged.

## 已知缺口

The first combined job in run38018370296 was cancelled with unavailable logs.
Framework cleanup correction PR179 reached main1486d2f4 after full local and
seven remote checks passed. Exact-head acceptance of this workflow is still
required; independent budgets alone do not prove runtime cleanup.
