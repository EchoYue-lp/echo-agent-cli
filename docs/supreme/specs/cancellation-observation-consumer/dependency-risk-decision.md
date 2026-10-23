# Current CLI dependency risk decision

## Status

accepted

## Human decision

On 2026-10-16 the user instructed: “glib、proc-macro-error、bincode 先记录，暂不升级。既然稳定的依赖依然采纳，就不要动。你继续处理其他的问题”. This accepts only the current EKO CLI delivery's three residual risks. Earlier unselected upgrade/backport proposals remain ignored history.

## Original three-risk scope (historical lock cut)

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


## Bounded consumer reassessment after executor fixture admission repair

The original human continuation instruction and exact three-risk deferral remain the authority. The unchanged gate rejected the new consumer before invoking the scanner. Independent source/necessity and exposure review report C0/I0/M0 for the two-file increment: a thin private application wrapper keeps the original process admission at capacity5, while four mock gate fixtures pass independent existing admissions at the same capacity. The single framework RuntimeTaskService body, DAG and CAS remain unchanged. Two fixtures now require their original first four tasks durably Completed with no claim before interruption, within the original five-second barrier. No GTK/glib, related macro or syntect/bincode caller, dependency, feature, platform or accepted identity changed.

The same original binary first passed the cancel/revision pair, then failed the natural cancel/pause/revision combination. Failure-only diagnostics captured the pause fixture missing one gated dispatch while process admission was active5, file IO had all8 permits free and the original driver remained pending. No early driver error was observed. This supports test scope interference; it is not a claim about the whole interval or permanent resource leakage. The exact repaired four fixtures plus three normal production-wrapper controls pass 7/0/0 with zero warnings and no input drift on clean CF6563. Full qualification remains separate.

After that finite review and actual pass, the consumer digest is explicitly rebound from `63e04671ec50ae98f7484759cc9733c6ce90ecf4affac1f52e27ea8ccada258d` to `28312efa214ac20f55c387ae665949511f3e27980740793eb821f30cc37eecb2`. The original Cargo.lock and three exact package/source/checksum/advisory/kind identities and residual risks remain unchanged. Fresh official actual-lock and canonical registry-origin online scans must qualify the new fingerprint; earlier scans and Clippy remain evidence for their original cut.


Current rebound qualification now includes six adapter test methods and two newly executed official online scans. Actual-lock and freshly generated CF6563 canonical registry-origin results each retain scanner1, exactly the original three warnings, zero vulnerabilities and separate gate0 (`noFetch=false`, ignore=[]). Each scanner process alone rewrites the complete official RustSec HTTPS URL to the same repository via SSH; no global configuration or acceptance expansion. The new raw reports stay task-local, distinct from the preserved earlier HTTPS failures and every prior-consumer result. The original complete workspace failure and remaining Rust/GUI/main/native Stop gates are not closed by this focused checkpoint.


## Final local qualification boundary

All six applicable local Rust/GUI commands now pass on unchanged CLI7a33 runtime inputs and clean CF6563, with zero compiler warnings and no source/framework drift. The first GUI attempt was interrupted by the unchanged global free-space guard; its original exit-2/failed progress and raw outputs remain preserved. After the SDK's authorized cache cleanup, the identical GUI command passed under the original9GiB target/5GiB free-space limits. The six risk adapter methods and newly fetched actual/origin exact-three reports remain applicable to unchanged consumer28312efa214ac20f55c387ae665949511f3e27980740793eb821f30cc37eecb2. This documentation does not change the lock, code, features, scanners, accepted identities or residual risks and does not renew acceptance for future consumer changes.

The primary current-checkpoint preflight and the cumulative PR comparison use different real bases. The original primary cumulative request failed because its preflight base7a33 differed from requested mainf57; that error is retained. The already owned frozen candidate view has actual HEADf57, a staged complete tree exactly equal to primary7a33 (945 files,84 changed paths), and a freshly recorded official view/f57 contract; its ordinary and cumulative strict checks pass. This is a candidate validation contract, not retroactive primary pre-edit evidence or native Stop. Primary installed task/repository validation still has the known five historical-file collector errors pending protected plugin repair. Exact main CI and fresh actual merged-main source recovery remain outstanding; earlier scanner/compile/semantic success cannot close them.


## Bounded consumer reassessment for framework main 3560812

The original human instruction to retain the three stable dependencies and continue remains the authority. The coordinating task and independent reviewers supplied implementation and consumer-exposure evidence, not a new human risk approval. The original gate first rejected both the two-field candidate and the final changed consumer before invoking any scanner (`scannerExit: null`).

The only changed application Rust input is `echo-agent-app-core/src/managed_conversation.rs` (SHA256 `012909c965c0dffe03e621d880b1a2f48160c4d0c1fc1e02af6d024dd1a96c2d`). Its two ordinary-import cursor literals explicitly have no compression-derived bindings. Import now delegates current rendered-root creation to the existing framework resume path, and matching-checkpoint counts use actual non-System messages rather than assuming one synthesized root. It retains canonical rows, validation, receipts, epoch, CAS and all six original test bodies. Framework main `3560812caede84998a1f6f26d7bf3fa81ea0486c` is the clean sibling consumer input; the canonical relative manifest paths remain unchanged.

Independent implementation, necessity and exact exposure review report C0/I0/M0: no GTK/glib, related macro, syntect/bincode production caller, serialization path, feature, manifest, build entry or dependency edge is added. Framework manifests and lock bytes also remain unchanged from the earlier accepted consumer framework. The first actual original six-case run compiled without warnings but failed three restoration assertions (3 pass/3 fail, 0 ignored, 1650 filtered, message length 3 rather than 2). Its raw output and original executable remain preserved. After the bounded root/count repair, the same original command passes 6/0/0 with 1650 filtered, zero warnings, no source drift and clean exact framework356. The original 4 GiB target / 5 GiB free-space limits were not raised or triggered.

Only on that finite review and actual pass, the consumer fingerprint is rebound from `28312efa214ac20f55c387ae665949511f3e27980740793eb821f30cc37eecb2` to `e2d5f0effc12b6df6952efb40eb56119cdf83edacabf170ba83c290658536043`. Cargo.lock SHA256 `8e9512837eab1cab81bd9097d0636fbb7fc9afd00b809bd69eedde8c5345aa4c`, all three exact versions, registry sources/checksums, advisories, kinds and residual risks are unchanged. No upgrade, backport, replacement, scanner ignore or SDK-specific exception is introduced. Fresh adapter tests and actual-lock/canonical-origin online scans remain separate qualification; the prior results cannot validate this new consumer. Full Rust/GUI/main CI and installed native Stop remain separate outstanding delivery gates.


### Historical pre-approval online qualification was blocked

The six original adapter test methods pass after the bounded fingerprint update. Both freshly fetched official online scans completed: the actual CLI lock and the registry-origin view generated by the clean framework's canonical helper. Each has `noFetch=false`, empty scanner ignores, zero vulnerabilities and six warnings, with official scanner exit1 **and** the separate exact-risk gate exit1. The gate correctly rejects the additional unaccepted advisories; this is not scanner1/gate0 or a clean audit. Both reports use RustSec database `999ec89d5d3f10f20be14750ec8c2d8b44e0ac9b` (1338 advisories). Their actual report bytes are identical, SHA256 `a5cd6d2845470cdb3f6e5f94ddc21980e80317a8505fcf68ae6aa588063034f6`; raw reports, stderr, exits and unchanged-source guards remain task-local.

The original exact glib0.18.5, proc-macro-error1.0.4 and bincode1.3.3 risks remain frozen and accepted only within the existing decision. The following newly reported standalone-crate maintenance risks are **not accepted** by that decision or its Finding:

| Package/version | Official advisory | Current real consumer |
| --- | --- | --- |
| zbus_names 4.3.4 | [RUSTSEC-2026-0371](https://rustsec.org/advisories/RUSTSEC-2026-0371.html) | Linux/BSD GUI notifications through zbus 5.18.0 and its macros |
| zvariant 5.13.1 | [RUSTSEC-2026-0370](https://rustsec.org/advisories/RUSTSEC-2026-0370.html) | The same zbus 5.18.0 D-Bus wire implementation |
| zvariant_derive 5.14.0 | [RUSTSEC-2026-0372](https://rustsec.org/advisories/RUSTSEC-2026-0372.html) | zvariant 5.13.1 derive macros on that same path |

The existing active path is `gui` → tauri-plugin-notification2.3.3 → notify-rust4.17.0 → zbus5.18.0 → the three packages. `src/tauri/mod.rs` registers the notification plugin and `src/tauri/ipc.rs::native_notify` actually uses its `NotificationExt`; this is not an unused lock entry. This is production source reachability, not a new native GUI or D-Bus execution test. The advisories describe standalone crates merged into zbus6, not a newly demonstrated memory-safety vulnerability. Zero reported vulnerabilities does not settle their maintenance risks.

The 2026-10-20 read-only official upstream check found [notify-rust4.18.2](https://crates.io/crates/notify-rust/4.18.2) still requires zbus^5. The latest stable [tauri-plugin-notification2.7.0](https://crates.io/crates/tauri-plugin-notification/2.7.0) requires notify-rust^4.18 and directly requires zbus^5 on Linux/BSD. Therefore merely updating to those official latest releases would retain the new warnings. The [upstream merge issue](https://github.com/z-galaxy/zbus/issues/2043) records that the standalone packages receive no further maintained releases after the zbus6 merge. This finite source check does not select or approve a migration, fork or new risk acceptance.

The original accepted-risk policy/scanner and its three identities remain unchanged; only the reviewed application-consumer fingerprint changed. No dependency, feature, manifest, build entry, scanner ignore or acceptance expansion is introduced. The new three-advisory blocker, full framework356 Rust/GUI gates, exact CLI main CI and installed native Stop remain unresolved. No commit or main delivery is claimed while these scans fail.


## 2026-10-21 exact six-risk decision and WebPKI lock patch

The user explicitly accepted the additional current `zbus_names4.3.4`, `zvariant5.13.1` and `zvariant_derive5.14.0` maintenance risks together with the reviewed fixed-six input contract. The original 2026-10-16 glib0.18.5, proc-macro-error1.0.4 and bincode1.3.3 versions, identities, residual-risk definitions and event-bounded lifetime remain unchanged. The earlier three-risk rejection above is historical evidence before this additional decision, not a fabricated successful scan.

The current exact CLI lock SHA256 is `d2e7b7ebc035d35940f20fd9a95628a37ddf13bba2c1d5c828cdac142ae41680`; consumer digest is `9c834f6cf60a328b93ef45888e3b88ebdd483c85244dea9b6bd6463a59d76c12`.

On 2026-10-23 the application-consumer fingerprint was recomputed after the parallel remediation batch (MCP name ownership, plugin cleanup receipts, cron occurrence identity, task settlement evidence, GUI sandbox isolation floor). Only application Rust sources changed: `Cargo.lock` still hashes to the recorded value, the dependency graph is unchanged, and the six accepted advisory identities, versions, checksums and kinds are byte-identical to the decision above. No dependency, feature, build entry, scanner ignore or acceptance expansion is introduced, and no new advisory applies; only `consumer_inputs_sha256` moved with the reviewed source bytes. All six entries use `registry+https://github.com/rust-lang/crates.io-index`. The additional accepted identities are:

| Advisory | Package/version | Registry checksum | Kind |
| --- | --- | --- | --- |
| RUSTSEC-2026-0370 | zvariant 5.13.1 | bee2a0bcd2a907786a456fff45aaaaf54c9ba5f50b71ae9ec1a4edd200c94911 | unmaintained |
| RUSTSEC-2026-0371 | zbus_names 4.3.4 | d8bf88b4a3ff53e883001e0e0115b297a9d53c31b9c1edd2bfdd853e3428624e | unmaintained |
| RUSTSEC-2026-0372 | zvariant_derive 5.14.0 | d496a145685283b67e232bd9e47377f6b60ad9d51e3601b23867f77c42477f96 | unmaintained |

Their official `withdrawn` values are null, as required by the unchanged report verifier. GUI plugin registration and `native_notify` reach notify-rust4.17.0 / zbus5.18.0 on Linux/BSD; these maintenance warnings remain real. No native notification/D-Bus execution or repaired maintenance status is claimed. Only these exact added identities are accepted, not arbitrary zbus versions, sources, warnings, vulnerabilities or yanked packages.

The same existing event-bounded decision applies only to this delivery and the exact lock/consumer graph. Changes to lock bytes, dependency identities, consumer inputs, warning kinds or withdrawn state require reassessment. There is no new calendar expiry field, automatic renewal, scanner ignore or framework/SDK exception. Original three-risk acceptance is not extended to another delivery by this record.

The separate new vulnerability RUSTSEC-2026-0382 is fixed by changing only rustls-webpki0.103.14 to0.103.16 and its official checksum `4443b1b6d39d7c663139942cc4558262562d2c84123bb6282ef177009ca8b281`. The other838 package blocks are byte-identical; the policy's lock digest is rebound to the actual patched lock. This does not upgrade the original frozen three packages or the notification chain.

After human approval, both actual-lock and canonical registry-origin official scans report zero vulnerabilities and exactly the six accepted warnings (839 dependencies / 1348 advisories). Each original `cargo audit --json --deny warnings` exits1; the independent fixed-six risk gate and caller exit0. Both reports are11988B / SHA256 `948691d46513016fbea8c8005088582ff9429aee137a1dabf16fccf19737fc8c`. Explicit `--db` plus `--no-fetch` binds an independently fresh official RustSec cut `e6f94f3760d5e6c54c1b05f204002263bc025524` / tree `36f251a0f912c8275985e0306e9fe7715c19a4ad`; this is not described as online fetching or a stale-database exemption. Complete raw JSON/stderr/natural exits and source/DB guards remain task-local.

The original six offline unit methods pass on the approved policy; only the existing input-count guard changes from3 to6 and its original test name/expected count are synchronized. Exact version/source/checksum/kind, unknown-warning, vulnerability, scan-error and configuration-override checks remain unchanged. The report verifier is byte/AST-equivalent.

This is the historical 2026-10-21 focused source/risk checkpoint. F1d's original17 failures pass in25 selected cases, and the old-lock F7ded full Rust/GUI sequence passed with its two explicitly recorded resource budgets; neither is patched-lock engineering qualification. On the exact patched lock, the original managed6 / model-module24 / exact-pool1 commands now pass31 distinct named cases (zero failures/ignores/warnings), with the original17 failures only a subset of25. Raw/source/binary identities and finite independent review are recorded in the existing Evidence. Patched-lock full Rust/GUI gates, final framework main/CI/cold, CLI full main acceptance and native semantic Hook/Stop remain pending. No warning-free audit, complete delivery, publish or release is claimed.


## Final consumer qualification on the accepted framework main

The same exact d2e7 lock/e46 consumer/six accepted identities now have actual full local Rust/GUI qualification on framework main f7bac. The original eight engineering commands and explicit included-factory format check pass with zero compiler warnings and input drift; workspace1978/15existing ignores and GUI226/3existing ignores remain separate configurations. Original six policy unit methods also pass. This changes no accepted dependency, expiry model, scanner ignore, contract or runtime API.

Both original actual-lock and canonical registry-origin audit commands now use default fresh official HTTPS fetching (`noFetch=false`), database4b587f00996ebe5c05a73eaef105ef241e876414/tree523058420679c8a4901e43e8e5fcb1e1406e7436,1348 advisories/839 dependencies. Each reports zero vulnerabilities and exactly the accepted six warnings; original scanner exit1 is retained separately from risk gate/caller0. All earlier rejection/fetch/cold-resource results remain historical facts; acceptance is not a repaired/maintenance-free dependency claim. The existing Evidence records exact raw identities, source projection and limitations. Exact merged-main CI/cold remains a separate delivery condition; no package release or extra canceled native-plugin campaign is implied.


## 2026-10-23 bounded reassessment for reviewed legacy-patch repair delivery

The user authorized completion of the reviewed repair Goals and, after the local result explicitly identified dependency reassessment as a publication prerequisite, instructed “提交、推送、合并”. This continues the same repair delivery. It is not a new approval of a package, a new residual risk, or acceptance for a future task. The unchanged gate rejected the changed consumer before scanning (`scannerExit: null`); that failed receipt is preserved.

Independent finite exposure and authorization review compared the exact six entries with the original fixed-six decision. Cargo.lock remains `d2e7b7ebc035d35940f20fd9a95628a37ddf13bba2c1d5c828cdac142ae41680`; every package name/version/registry source/checksum/advisory/kind is unchanged. Manifests, features, build scripts, Cargo configuration and the scanner are unchanged. No GTK/glib VariantStrIter caller, macro dependency, syntect/bincode serialization path, notification/D-Bus caller, platform entry or dependency edge is added. New TUI Review output is a System message rendered as plain spans, not a new syntect input. The fixes retain the original stable dependencies and do not claim that their residual risks are repaired or unreachable.

Only after that source review, real red/green regressions, independent implementation reviews and the completed local Rust/GUI/consumer gates, the consumer digest is rebound from `9c834f6cf60a328b93ef45888e3b88ebdd483c85244dea9b6bd6463a59d76c12` (294 inputs) through the reviewed `b85ec037` repair base `d807768d3646c075688df3c6c5fea00a9c97fd66002628b1e839a55235c3f4f7` to `47bb8f6a1d67d4ab808903c04750d6eaf8d3ba3944d64e60eccc311f913cc22b` (295 inputs). The added path is the already reviewed plugin cleanup receipt ledger. This is an explicit bounded reassessment, not an automatic fingerprint refresh.

Fresh six-method policy tests and new actual-lock/canonical registry-origin official scans must qualify this rebound input separately. Preserve the scanner's natural exit and complete output; a separate exact-risk gate may accept only the same six identities. Any new vulnerability, warning, yanked package, withdrawn advisory, changed identity or consumer exposure remains blocking. No fresh scanner or exact-main CI success is claimed by this reassessment paragraph alone.


### Fresh delivery scan: repair of a newly yanked Windows clipboard patch

The new official scan against RustSec `f7b2f6bc663452200bbe639ebb75e0c762b3422b` reported zero vulnerabilities and the six unchanged accepted warnings, plus a new `yanked` diagnostic for `clipboard-win 5.4.1`. The fixed-six gate correctly rejected it. This new diagnostic is not accepted, ignored or folded into the six-risk policy.

Official crates.io metadata and downloaded crate checksums confirm that 5.4.1 was yanked on 2026-10-23 and 5.4.3 is an unyanked patch with checksum `052af7878466feccda12ba7a6a67d019d712f296809559b47c12c8cc196d3a9a`. Cargo's targeted `cargo update -p clipboard-win --precise 5.4.3` changes only that package version/checksum; all other 838 package blocks and dependency edges are identical. The unchanged `arboard 3.6.1` Windows dependency requirement permits this patch and keeps the existing `std` feature. No manifest, feature, application source, frozen accepted package or scanner changes.

The exact lock is rebound from `d2e7b7ebc035d35940f20fd9a95628a37ddf13bba2c1d5c828cdac142ae41680` to `d804d2907ad283e6f6831cb37551bc0b09e6a169c24a50abe320a9ea0fddae84` after this bounded patch review. The consumer remains `47bb8f6a1d67d4ab808903c04750d6eaf8d3ba3944d64e60eccc311f913cc22b`. The six package/source/checksum/advisory/kind identities and residual risks are unchanged. Fresh actual-lock/canonical-origin scans, Windows arboard compilation and repeated local final gates qualify the patched lock separately; results are recorded only after execution. The original rejected yanked report is preserved.


Fresh patched-lock qualification: six original policy methods pass. Both newly executed actual-lock and canonical registry-origin scans fetch the official RustSec database and retain scanner exit1, zero vulnerabilities and exactly the unchanged six warnings; each separate exact-risk gate exits0. The actual scan observes database `f7b2f6bc663452200bbe639ebb75e0c762b3422b` and the origin scan `f7b2f6bc663452200bbe639ebb75e0c762b3422b`. HTTPS fetch failures and the rejected yanked report remain preserved. Only each successful scanner process rewrites the complete official RustSec HTTPS URL to the same repository over SSH; no global Git, registry, lock or scanner filtering configuration is changed. Windows-target `arboard` compilation against `clipboard-win 5.4.3` also exits0. This does not assert native Windows clipboard execution or complete patched-lock main CI; those qualification boundaries remain distinct.


### Later fresh database update: Tokio I/O-safety patch

A later official RustSec refresh (1,351 advisories) newly reported `RUSTSEC-2026-0386` against Tokio 1.53.1. This `unsound` diagnostic is not accepted by the fixed-six decision. The official advisory identifies 1.53.3 as patched. Cargo targeted update and published crate/checksum review select unyanked Tokio 1.53.3, checksum `ce3335fa71841cda333a58d7615b03901380ecf09d59b3296d21f8bbac0dde4e`. All other 838 package blocks and all dependency edges remain byte-identical, including the prior clipboard-win 5.4.3 repair; no manifest/feature/API/scanner or accepted package is changed. Cargo full locked metadata accepts this precise graph. Cargo's unrelated edge re-resolution is excluded from this bounded patch.

The exact lock changes from `d804d2907ad283e6f6831cb37551bc0b09e6a169c24a50abe320a9ea0fddae84` to `cca90ea328dbdbb5ebf819b5afb8845714c202e5f5fd1b9c032811c35f0e1307`; consumer remains `47bb8f6a1d67d4ab808903c04750d6eaf8d3ba3944d64e60eccc311f913cc22b`. This continues the already authorized repair delivery, removes the new advisory and accepts no new residual risk. The six exact accepted warning identities remain unchanged. Fresh scans and repeated complete gates for this new lock must establish their own results; the preceding scans/green gates apply to the preceding lock and are not relabeled.


Executed new-lock qualification: the original six policy tests pass. Fresh actual-lock and canonical registry-origin scans each observe RustSec `84d943a6deeb8cc6c6f632531aaf4f1c62275239` (1,351 advisories), zero vulnerabilities, exactly the original six warnings, no yanked package and no `RUSTSEC-2026-0386`. Each preserves scanner exit1 and separate exact-risk gate0, with default fresh fetching. The full Rust/GUI gates and successor main CI for this lock remain separately recorded after execution; the previous lock’s results are not renamed.
