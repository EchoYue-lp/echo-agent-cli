# Current CLI dependency risk decision

## Status

accepted

## Human decision

On 2026-10-16 the user instructed: “glib、proc-macro-error、bincode 先记录，暂不升级。既然稳定的依赖依然采纳，就不要动。你继续处理其他的问题”. This accepts only the current EKO CLI delivery's three residual risks. Earlier unselected upgrade/backport proposals remain ignored history.

## Exact scope

Registry source for all entries: `registry+https://github.com/rust-lang/crates.io-index`. Actual CLI Cargo.lock SHA256: `8e9512837eab1cab81bd9097d0636fbb7fc9afd00b809bd69eedde8c5345aa4c`.

| Advisory | Package/version | Registry checksum | Current consumer |
| --- | --- | --- | --- |
| RUSTSEC-2024-0429 | glib 0.18.5 | 233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5 | Linux/BSD GTK3 GUI through Tauri 2.12.4, runtime-wry 2.12.2, GTK/WebKitGTK and menu/tray integrations |
| RUSTSEC-2024-0370 | proc-macro-error 1.0.4 | da25490ff9892aab3fcf7c36f08cfb902dd3e71ca0f9f9517bea02a73a5ce38c | Host macro compilation through glib-macros 0.18.5 and gtk3-macros 0.18.2 |
| RUSTSEC-2025-0141 | bincode 1.3.3 | b1f45e9417d87227c7a56d22e471c6206462cba514c7590c09aff4cf6d1ddcad | TUI syntect 5.3.0 parsing and embedded syntax/theme dumps used by src/tui/markdown.rs |

glib0429 is an actual VariantStrIter soundness defect with possible undefined behavior/NULL-pointer crashes. The other entries report unavailable maintenance. No repair, unreachability or absence of undiscovered defects is claimed. The local personal-product setting does not remove these risks.

## Limits and gate meaning

Keep the current versions and functionality; do not upgrade, backport, replace serialization or migrate GTK/Tauri for this task. Changed versions, registry sources/checksums, lock graph or consumer scope require reassessment; advisory IDs alone are not a permanent exemption. No automatic schedule or new upgrade obligation is introduced.

Both actual-lock and fresh CF988 registry-origin official unfiltered scans returned exit1 with these three warnings. Keep their complete outputs and true exits; database-fetch failures remain separate failures. A gate recognizing this decision must separately report exact risk-acceptance verification and must reject every additional warning, vulnerability, yanked dependency or source/consumer expansion. It cannot relabel scanner1 as audit0. Framework/SDK exceptions do not apply; compiler, Clippy, fmt and test requirements are unchanged.

## Delivery state

The application adapter `scripts/check_cli_dependency_risks.py` runs the official unfiltered scanner, preserves raw stdout/stderr and its true exit, then separately checks `.cargo/accepted-dependency-risks.json`. Its two CI invocations cover the actual CLI lock and a fresh view produced by the sibling framework's existing canonical origin helper. No scanner ignore or dependency upgrade is introduced.

The policy binds the complete lock bytes (including dependency edges) and an aggregate of sorted path names and file hashes for application Rust sources, both manifests, build scripts and Cargo configuration. Additions, deletions, changed features or callers invalidate acceptance; a future task must assess its consumer changes rather than automatically regenerating this fingerprint. The broad source binding intentionally fails closed when a narrower exposure proof is unavailable.

An independent review found that cargo-audit's project/Cargo-home `audit.toml` can silently disable fetch or yanked checks without reflecting those settings in JSON. The adapter now rejects either configuration candidate (including symlinks), resolves relative Cargo home against the scanner's actual working directory, and fixes the official database URL; scanner input checks run before and after the scan. Regression cases verify rejection before scanner execution. This is a correction to the gate, not a new dependency exception.

Current focused adapter tests cover exact acceptance and clean reports, changed identities/new advisory, vulnerability/yanked/filter/schema/exit errors, lock/feature/new caller drift and original scanner-output preservation. Actual online/registry-origin execution, independent review and final main CI remain separate qualification until their results are recorded.

Current qualification: six test methods and independent implementation/necessity review pass (C0/I0/M0). Both actual-lock and freshly generated CF988 registry-origin scans completed online with only a per-process Git HTTP/1.1 transport setting; each preserves official scanner exit1/three accepted warnings/zero vulnerabilities and reports separate gate0 (`noFetch=false`). Original default-transport fetch failure is retained and remains a distinct failed attempt. This is no claim of repaired dependencies, default-transport recovery, full Rust/GUI acceptance or final main CI. Raw reports and the bounded review receipt stay in ignored task-local storage.
