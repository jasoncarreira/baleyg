# #67 pre-main integration — Status, unchanged Serve and review follow-ups

**Status:** integration worktree under verification; no combined `main` PR yet. Original Claude gave conceptual acceptance of the narrow B4/B8 and B6 deferrals. This is not exact-head PR review or merge approval. Based on merged `feature/67` commit `0bfa5091f9fb5fa35c055d1618d390f975ec7226` in isolated `feature/67-pre-main-integration`. Do not conflate this evidence with the earlier [PR #98 AC8](98-pr-ac8.md) approval into `feature/67`.

- Host: Mac17,16 Apple M5 Pro; frozen corpus manifest SHA-256 `8b8deea8592cfd069a1500bcad9d634a8b4d343477e769b2f2aed0dd61bee046`.
- Rust 1.99 release executable `target/release/baleyg` SHA-256 `426e9ebbcdad81fdc0407d321b7fcc957ea794d89aab47f3ee0d287b65dd1a4e`. It remained byte-identical across final frozen 20+20, additive mixed medium 20, and additive mixed large 20; both runner SHA-256 digests were stable (`bcda02f75be5cefcc7f8658965ab59ed05fce306c211a576e9619f2db3fe50a2` / `1eee5ac38db8a7c1ea6694b841cc2caf3ab5a36dfb4193fe28cb24407f379600`). A prior frozen pass on SHA `f8fdf4d093c7ac153e7af96f06bc76ded048e926b98ffd211dfdd521d82b5fc4` is retained only as superseded historical evidence after a topology documentation comment changed release bytes.
- Explicit Rust 1.99 fmt, `clippy --locked --all-targets --all-features -- -D warnings`, all-target tests PASSED on exact current source (770 passed, 0 failed across 47 suites; ignored tests remain ignored). The unchanged `./tools/verify` PASSED again after the topology #16 documentation comment with `RUSTUP_TOOLCHAIN=1.99.0`: rust, semantic, synthetic-cohort, and checks/browser lanes all exit 0. Both verifier preflight failures are retained as logs: one Rustfmt correction plus missing ignored ACP install, then missing ignored browser install. Lockfile `npm ci --prefix runtime/acp` and `npm ci --prefix tests/browser` supplied these ignored dependencies; no tracked package/lock changes or test bypass.
- A1: CLI Status is an existing-only, observational marker+O_RDONLY SQLite snapshot: virgin Git/HOME, repeated clean close, held use lock, sidecar, obsolete/corrupt marker, and full-tree byte inventory tests. Missing publication returns `index_not_ready`; uncertain SQLite/locks return `storage_busy` without creation, repair, lock mutation, pin advancement, or queue mutation.
- A2: leader unchanged Serve freshly reads and hashes every admitted source and checks same generation, selected stored source bytes, producer binding/executable, extraction context, options, capture inputs, selected version/projection links, and CAS/data-version fences. It publishes a new header+manifest+binding pin only; source/producer/presentation drift takes validated LOCAL/FULL fallback. Deep per-child selected-read attestation remains. Tests cover five-language and empty-root selection, same-length exact-mtime source edit, stored source/context/binding tamper, FK-valid child tamper on selected `symbols`, and no partial pin on refusal. Final frozen-medium smoke results appear below.
- B1/B2/B3/B5/B7/B9: remove historical unselected decode scan without weakening per-reused-version checks; replace clone/self class-F assertion with independently produced F; document bounded-local/#68; default CLI telemetry off with exact `BALEYG_INDEX_DIAGNOSTICS=1` opt-in (runner files unchanged); link 67's historical evidence honestly to 98; tie witness, local preflight and selected reuse to producer's declared extraction inventory, including synthetic nonempty/mismatched captures and old-pin refusal.
- B6: report-only allowlisted v4/v5/two v6/v7/obsolete-v8/present-v8 shape+extractor/root+age classifier. Nine historical fixture cases pass eligible aged report with DB bytes and path unchanged. Separate spoof cases compare full-tree path+byte inventories before and after each report. Recent, ill-typed root, active lock, hot journal, extra table/view/trigger and fake version are not eligible; spoofed shapes return exact `unknown_index_shape`. No derived-index deletion command or automatic deletion exists. Future #16 automatic GC requires a separate guarded public contract; `eligible` never authorizes deletion. Original Claude conceptually accepted report-only B6 and will confirm against the exact committed PR head before merge.
- B4/B8 residual profiling and bounded future optimization issue are in [67-native-local.md](67-native-local.md). Exact-release one-shot medium local 0.6642 s and authenticated limit100 class reads 13.0921/13.5797 s are not AC8 acceptance data. B4's deferred work is redundant SHA-256/UTF-8 decode of **stored** unchanged `document_versions.source_bytes` and stored class extraction during local publish. Fresh **current workspace** source-byte hashing remains mandatory for A2/T00; only the changed document needs stored text decode. Original Claude conceptually accepted this B4 optimization deferral and B8's per-path selected attestation cost. Full frozen+mixed AC8 and exact-head PR confirmation still gate merge, not opening the draft PR. No stat-only shortcut, skipped selected attestation, silent waiver or invented threshold.

## Final A2 medium smoke and AC8 gates

On common release SHA `426e9ebbcdad81fdc0407d321b7fcc957ea794d89aab47f3ee0d287b65dd1a4e` and the exact frozen 1,000-file medium corpus, a fresh FULL `index` took **74.0426 s**; unchanged leader Serve made a **new** revision 1→2 in **0.3489 s**, and observational Status took **0.0087 s** without advancing revision 2. All 1,000 selected document/graph/class IDs and the complete requests.db table digest remained identical at r1/r2/Status. The former merged runtime took 59.9281 s Serve and 60.9930 s Status on the same medium method; the over-attesting prototype took 119.0567 s Serve and is not accepted. Exact release report and raw logs are below; no large unchanged-Serve baseline is claimed.

All **80/80** AC8 revisions passed on the common release SHA with unchanged frozen and additive runners and invocation-level `BALEYG_INDEX_DIAGNOSTICS=1`. Each revision was LOCAL with unchanged IDs outside the edited document, zero reused occurrence reads, required native fact floors, writer counters, and independent same-root cold FULL parity. No corpus, sample, runner, floor or threshold changed; no failure was omitted.

| Independent cohort | Medium p95 (limit 2 s) | Large p95 (limit 5 s) | Samples |
|---|---:|---:|---:|
| Frozen canonical Python return leaf | **0.7559 s** | **4.6741 s** | 20 + 20 |
| Additive four-language mixed body | **0.7579 s** | **4.8475 s** | 20 + 20 |

The frozen baseline used independent cold setups of 74.4589 s medium and 678.5401 s large; the additive mixed large runner finished in 1628.9 s within its 1700-second deadline. Logs below contain all 80 raw samples, per-revision pins/fact counts/reuse, and cold-oracle parity. A combined draft main PR may open under Claude's conceptual deferral answer after the final common-SHA A2/B4/B8 smoke and docs. The original-Claude exact-head integrated Part A+B review and green CI still gate merge.

## Raw evidence (relative to this file)

The `before-topology-comment-*` and `frozen-before-topology-comment.log` files are historical passing measurements on the superseded SHA `f8fdf4...`, **not** accepted common-SHA evidence. The two verify preflight logs document setup failures; neither is counted as a test or performance pass.

| Log | SHA-256 |
|---|---|
| [`rust199-all-targets.log`](67-pre-main/rust199-all-targets.log) | `17719c321ea3f538cb02a2f2baa948b235794bc74aee48bb888259df89df6b0d` |
| [`verify.log`](67-pre-main/verify.log) | `99ea594917fdb4198c8f60ce03122f649d937a12d25fb78831999fb2d5f9a355` |
| [`a2-final-full-index.log`](67-pre-main/a2-final-full-index.log) | `f2f357e9bcc8507747875651fa90500ba8b97667220b5977b38df9a5323e222c` |
| [`a2-final-medium.json`](67-pre-main/a2-final-medium.json) | `88e7673635f7f8732b3bf49efb8b9ba04ef01bcfe4f186a21d16825cdb39d8b2` |
| [`a2-final-medium.log`](67-pre-main/a2-final-medium.log) | `a0f6521b3cc3708b045db3aa95b9464355ef9aac5d5b4b43c95ecd27e9142d34` |
| [`classes-final.json`](67-pre-main/classes-final.json) | `6c02a8e99d04889e1878eb3bc6f9ab8090cf143baac9a533e760fe96c59c57cc` |
| [`classes-final.log`](67-pre-main/classes-final.log) | `c50d77c21a05f69ecf14b90c2de82c3c4693a29057f5eeda212cf76e45fe761c` |
| [`local-one-shot.json`](67-pre-main/local-one-shot.json) | `be19c8375c9fccdbbecae5fd50f9c879262e23f0db9942c949124816d468e7df` |
| [`local-one-shot.log`](67-pre-main/local-one-shot.log) | `1b9fa16fef3b390601297cf605be643a33cda8148d4d5a49a8d6853659725850` |
| [`frozen-canonical-final.log`](67-pre-main/frozen-canonical-final.log) | `e4cee027a4c650b3e0f341ec4381bcbc0d3a04f2969d3f2daeb6b134b51957bb` |
| [`mixed-medium-final.log`](67-pre-main/mixed-medium-final.log) | `15c3d353eb78be7b5bdd4ea98c022cd0056b08e51370975b6f867131ba7c7331` |
| [`mixed-large-final.log`](67-pre-main/mixed-large-final.log) | `71e20a76d630e67f99ff636e2f200ee9cbe4d97dea7d7a460bb724e1f9065252` |
| [`sha-freeze.json`](67-pre-main/sha-freeze.json) | `0ba3b86ed3b13013a0552cbfd5c012a5586bcce9a7a342638ced922a3670a6ff` |
| [`verify-preflight-failure.log`](67-pre-main/verify-preflight-failure.log) | `b25f6d0520c90d4dfc85a902788977d9461c41c986a2bd1bd02c4f0badb6f7f1` |
| [`verify-missing-browser-dep.log`](67-pre-main/verify-missing-browser-dep.log) | `edc310d09eac8bed6d78ad44fb2c2f332583c48e3bd4e010ff6dc42b5cdef0c8` |
| [`verify-before-topology-comment.log`](67-pre-main/verify-before-topology-comment.log) | `37381946017346b0eb4537d7975755620266cea02f96deb42d934e8c1c1d60af` |
| [`frozen-before-topology-comment.log`](67-pre-main/frozen-before-topology-comment.log) | `074a01afbc7f804eac95908878f9f037e13fd736a2e30d6583dfb3b2bb5c1f64` |
| [`before-topology-comment-a2-final-medium.json`](67-pre-main/before-topology-comment-a2-final-medium.json) | `2c89fe5e11ffd56e1a96a244487add21e00adc9d033bb159bbe00d1fb0436b02` |
| [`before-topology-comment-classes-final.json`](67-pre-main/before-topology-comment-classes-final.json) | `f8677aee606998fd71051b5267e7e5d69acad748ce87779083be51f20c27388b` |
| [`before-topology-comment-local-one-shot.log`](67-pre-main/before-topology-comment-local-one-shot.log) | `e9904522063ac683ced4b519c1e75b2a70a6a467acaa87e15fe3086a2f3f0b05` |
| [`before-topology-comment-a2-final-full-index.log`](67-pre-main/before-topology-comment-a2-final-full-index.log) | `699359f7d50dc789931e557c2ad099e9c88a12593446139d7d5b17bf793f3d62` |
| [`before-topology-comment-a2-final-medium.log`](67-pre-main/before-topology-comment-a2-final-medium.log) | `bd845e803015b9a0615fb094ac249dfac23525c7e6791aba589399567d6a5112` |
| [`before-topology-comment-classes-final.log`](67-pre-main/before-topology-comment-classes-final.log) | `34ad707e2227c76385b42bc2fca796bee754ab482db111a520b3fd1ad6428c62` |
| [`before-topology-comment-local-one-shot.json`](67-pre-main/before-topology-comment-local-one-shot.json) | `308f597274c26617aed4b084bfbe85f900fdbdec046a96260d46d23f0fb28d0c` |
| [`before-topology-comment-release-build.log`](67-pre-main/before-topology-comment-release-build.log) | `10f9d1bb2bd3d5e44d271bdb79cc6480ca78a49dc15dd48c14934564a6845709` |
