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

## Bounded consumer reassessment after shared preparation repair

The original policy correctly rejected new `chat_driver.rs` bytes before invoking the scanner. The coordinator explicitly required a real same-entry regression, the minimal existing typed conversion repair, successful AFTER and independent consumer-scope reassessment before rebinding. This is the same authorized task, not automatic renewal for arbitrary future code.

The only changed Rust/build input is `echo-agent-app-core/src/chat_driver.rs` (SHA256 `c3fe62e6308fa0c2ac1130c978a1f4d532f246785b1f379889eead89fa755df3`). Independent review C0/I0/M0 confirmed the private error propagation and tests add no GTK/glib, related macro or syntect/bincode production call, dependency or feature. The same two AFTER tests passed, zero warnings and no input drift. Lock bytes, all three package identities/checksums, advisory kinds and accepted residual risks are unchanged.

On that explicit basis, the sorted application-consumer fingerprint changes from `1e6787799b5258e429b1bca0e7446614d6aed53bf963e20b6e38c9044887a42d` to `77ae4b12adca72c8645f7af484e4ca7fc280d3f0522a1bdfac836a6d3e68a201`. Fresh gate executions and complete affected Rust/GUI/main qualification remain separate evidence; the earlier scanner results do not by themselves validate new consumer bytes.


## Bounded consumer reassessment after full-gate failure repair

The original human migration-and-continue instruction and exact three-dependency decision remain the authority. Coordinating agents and independent reviewers supplied implementation evidence, not a new human approval. The gate first rejected changed Rust input before scanning (`scannerExit: null`); there is no automatic fingerprint refresh.

Fourteen source files repair original owner/debt retention, closed-watch settlement priority, business-progress classification and their actual root/lifecycle/driver fixtures. The last three-file increment shares the original cancellation target with the read-error fallback, factors only the existing cfg(test) physical-completion observer and adds three target controls in one test. Independent source and exposure review found no new GTK/glib, related macro or syntect/bincode production caller, feature, platform or dependency edge. Actual original 48 plus the new case pass together on the frozen current CF988 cut; this is focused evidence, not the complete workspace/GUI/main gate.

The application consumer digest is explicitly rebound from `77ae4b12adca72c8645f7af484e4ca7fc280d3f0522a1bdfac836a6d3e68a201` to `274d3e12027c54e44f6acf89f2b33db74b1ec4ed5f0db505fcb06dabdc4fed14` after that bounded review. Cargo.lock SHA256, the three exact package/advisory/kind/source/checksum identities, their residual risks and the user's deferral remain unchanged. Fresh actual-lock and freshly generated CF988 registry-origin online scans now each preserve official scanner exit1, exactly the same three accepted warnings and zero vulnerabilities, with separate gate0 (`noFetch=false`). Only those processes use Git HTTP/1.1; the original default-transport failure remains recorded. Six adapter test methods pass on the rebound inputs. This does not repair dependencies or establish the remaining full Rust/GUI/main gates.


## Bounded consumer reassessment after CF6563 surface fixture repair

The same original human continuation instruction and exact three-risk deferral remain the authority. The unchanged gate first rejected the changed consumer input before scanning (`scannerExit: null`). Independent source and exposure reviews report C0/I0/M0 for three existing cfg(test) modules only; every production prefix is byte-identical to checkpoint9819. No manifest, lock, feature, build input or GTK/glib, related macro or syntect/bincode production caller changed.

Channel and GUI success fixtures now bind and retain the public original ApplicationLifecycleOwner through real driver settlement and normal Shutdown. The TUI stale source-marker assertion is replaced by the real planned-resume entry with the existing creation-time continuation policy, preserving exact Goal/revision/hash/root, run identity, independent input frontier and foreground-settlement assertions. The final frozen three-case run passes 3/0/0 with zero warnings on CF6563, sourceDrift=[], unchanged clean framework. Earlier failed compilation and the real missing-continuation runtime result remain separate failed attempts.

Only after that finite review and actual focused pass, the consumer fingerprint is rebound from `274d3e12027c54e44f6acf89f2b33db74b1ec4ed5f0db505fcb06dabdc4fed14` to `63e04671ec50ae98f7484759cc9733c6ce90ecf4affac1f52e27ea8ccada258d`. The actual lock SHA256, exact three package/source/checksum/advisory/kind identities, residual risks and deferral remain unchanged. Fresh online actual-lock and canonical registry-origin scans must qualify these bytes separately; focused tests do not establish complete Rust/GUI/main qualification.


Fresh qualification on this rebound consumer: six adapter methods pass. Two actual-lock HTTPS fetch attempts failed with server I/O and zero JSON output; the origin step had not yet started. Those failed attempts, independent curl timeout and native Git connection failure remain retained. Subsequent actual-lock and freshly generated CF6563 canonical registry-origin official online scans both complete with scanner exit1, exactly the same three accepted warnings, zero vulnerabilities and separate gate0 (`noFetch=false`, ignore=[]). Only each scanner process rewrites the full official RustSec HTTPS repository URL to the same repository over SSH; no global Git, registry, lock or scanner-filter configuration changes. Both fresh reports observe advisory DB f28da8c5d386e507fac79153655184056a20c054; this is a new fetch and scan, not borrowed SDK or prior-consumer output. The all-target workspace Clippy command also passes with `-D warnings`, but the remaining complete Rust/GUI/main gates remain pending.
