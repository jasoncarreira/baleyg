# PR #98 AC8 — frozen and additive local-edit evidence

**Status:** both independent release-mode gates PASSED on the PR #98 runtime. Claude publicly APPROVED WITH FOLLOW-UP, and PR #98 merged into `feature/67` as `0bfa5091f9fb5fa35c055d1618d390f975ec7226`. This is post-publication PR-branch evidence; it does not amend the frozen canonical corpus, floors, original benchmark, or Factory state. Owner-scoped pre-main Status/Serve and reviewer-follow-up runtime edits require separate renewed gates on their final release bytes.

- Host: `Mac17,16`, Apple M5 Pro, arm64. Frozen manifest SHA-256: `8b8deea8592cfd069a1500bcad9d634a8b4d343477e769b2f2aed0dd61bee046`.
- Release executable: `target/release/baleyg` SHA-256 `b5cab066a7af9c631a0ef7e478bb1e9acc601cb16cc69135784c83715e30a7b2` (default Rust 1.98 release build in the isolated PR worktree). Both runners measured these exact executable bytes; no runtime source or runner change occurred between the final runs.
- Explicit Rust 1.99 verification: `cargo +1.99.0 fmt --all -- --check`, `cargo +1.99.0 clippy --locked --all-targets -- -D warnings`, `cargo +1.99.0 test --locked --all-targets` PASS. The unchanged `./tools/verify` PASS (rust/semantic/cohorts/checks). Ignored heavyweight exact-medium (`cargo +1.99.0 test --locked --test index_atomic canonical_medium_java_prefix_literal_local_matches_independent_cold_and_retains_old_pin -- --ignored`) and 40-path AST-only (`cargo +1.99.0 test --locked --lib frozen_medium_large_mixed_edits_have_proved_local_ast_body_shape -- --ignored`) tests were also invoked explicitly; neither is benchmark evidence.
- Method: release CLI wall time including capture, same-root scan, proof, compose, attestation, publish, and status; nearest-rank empirical p95 after all per-revision native-fact floors. Single heavy job at a time. Frozen 20+20 and separate mixed 20+20 each require all samples without exclusion. Medium ≤2 s, large ≤5 s.

## Reproduction and exact raw logs

From this PR branch on the reference host, run `python3 tools/verify-67-performance.py`, then `python3 tools/verify-67-mixed-performance.py --size medium`, then `python3 tools/verify-67-mixed-performance.py --size large`. The additive large runner has a 1700-second deadline and 1750-second alarm; it must complete its independent same-root cold FULL oracle within that budget. The original runner is unchanged in its sample design (only relative `CARGO_TARGET_DIR` resolution was added).

| Relative raw log (full per-run facts, pins, reuse, phases, writer counters and parity) | SHA-256 |
|---|---|
| [`frozen-canonical-final.log`](98-pr-ac8/frozen-canonical-final.log) | `75dce747509bd4a7928279a4426135946cb8526b59e50eda8e65f755b7c0214d` |
| [`mixed-medium-final.log`](98-pr-ac8/mixed-medium-final.log) | `210640b81fd35e8932430e550260e546d9983021c61b3a3ecaaf087091ace672` |
| [`mixed-large-final.log`](98-pr-ac8/mixed-large-final.log) | `d1463ac6c07b381585b43a68b426859c3c29bdc38fde9ee5a6392892576bdd52` |

## Acceptance summary

| Method | Size | Accepted timed revisions | p95 | Limit | Initial FULL cold | Independent cold / FULL fallback |
|---|---:|---:|---:|---:|---:|---:|
| Frozen Python body-leaf | Medium | 20 | 0.7359 s | 2 s | 73.1300 s | separate declaration-surface FULL 69.5590 s; same-root independent cold 73.4499 s |
| Frozen Python body-leaf | Large | 20 | 4.5085 s | 5 s | 669.3323 s | frozen runner's separate medium FULL control above |
| Additive four-language mixed body | Medium | 20 | 0.7295 s | 2 s | 71.4486 s | same-root independent FULL 70.2948 s |
| Additive four-language mixed body | Large | 20 | 4.4986 s | 5 s | 662.6264 s | same-root independent FULL 670.4422 s |

All 80 accepted timed revisions had `mode:local`, exact full-key retained source-set/language/path joins, one changed document and no second changed path, zero reused occurrence reads, and complete postcommit per-family SQLite-bound writer row/byte counters. Each medium selected revision had ≥50,000 native facts in **each** of Java, Python, JavaScript and Rust (observed minima 225350/233554/241750/234719). Each large selected revision had ≥500,000 native facts in total (observed minimum 7552261). Every medium run reused the other 999 native versions, graph and class projections; every large run reused the other 9999. Each family’s class/graph/native version changed on the one selected path.

The additive cohort changed 20 **distinct** paths per size (five Java methods, five Python functions, five exported JavaScript functions, five Rust functions). It contains eight length-growing numeric edits, four fixed-width numeric edits, and eight locally bound **nonnumeric identifier** edits per size. Its final selected document, declarations, calls, regions, graph nodes/calls/regions, classes and class relations matched an independently indexed FULL cold oracle at the **same workspace root** in all nine row-digest families. The original frozen runner retained its independent declaration-surface FULL fallback and same-root cold oracle; see raw logs for all digest counts/hashes and revision pins. No failed attempt was excluded from a completed 20-sample acceptance run.

## Every accepted revision

All rows below are the complete successful cohorts. `Facts` means that revision’s **own** produced native fact total. `Writer` means postcommit total SQLite-bound rows/bytes, not `dbstat` or filesystem growth. For exact per-language floors, full-key ID proofs, phases, and parity digest hashes, use the relative raw logs above.

| Method | Size | # | Changed path | Edit (old → new) | Local seconds | Facts | Writer rows / bound bytes |
|---|---|---:|---|---|---:|---:|---:|
| Frozen | medium | 1 | `python/Cmedium0000.py` | 1 → 2 | 0.6426 | 935373 | 3699 / 1842127 |
| Frozen | medium | 2 | `python/Cmedium0001.py` | 1 → 2 | 0.6466 | 935373 | 3699 / 1842127 |
| Frozen | medium | 3 | `python/Cmedium0002.py` | 1 → 2 | 0.6579 | 935373 | 3699 / 1842085 |
| Frozen | medium | 4 | `python/Cmedium0003.py` | 1 → 2 | 0.6557 | 935373 | 3699 / 1842089 |
| Frozen | medium | 5 | `python/Cmedium0004.py` | 1 → 2 | 0.6646 | 935373 | 3699 / 1842085 |
| Frozen | medium | 6 | `python/Cmedium0005.py` | 1 → 2 | 0.6580 | 935373 | 3699 / 1842127 |
| Frozen | medium | 7 | `python/Cmedium0006.py` | 1 → 2 | 0.6833 | 935373 | 3699 / 1842085 |
| Frozen | medium | 8 | `python/Cmedium0007.py` | 1 → 2 | 0.6846 | 935373 | 3699 / 1842083 |
| Frozen | medium | 9 | `python/Cmedium0008.py` | 1 → 2 | 0.6907 | 935373 | 3699 / 1843137 |
| Frozen | medium | 10 | `python/Cmedium0009.py` | 1 → 2 | 0.6860 | 935373 | 3699 / 1843161 |
| Frozen | medium | 11 | `python/Cmedium0010.py` | 1 → 2 | 0.6850 | 935373 | 3699 / 1843179 |
| Frozen | medium | 12 | `python/Cmedium0011.py` | 1 → 2 | 0.6926 | 935373 | 3699 / 1843179 |
| Frozen | medium | 13 | `python/Cmedium0012.py` | 1 → 2 | 0.7153 | 935373 | 3699 / 1843171 |
| Frozen | medium | 14 | `python/Cmedium0013.py` | 1 → 2 | 0.7093 | 935373 | 3699 / 1843161 |
| Frozen | medium | 15 | `python/Cmedium0014.py` | 1 → 2 | 0.7359 | 935373 | 3699 / 1843137 |
| Frozen | medium | 16 | `python/Cmedium0015.py` | 1 → 2 | 0.7080 | 935373 | 3699 / 1843177 |
| Frozen | medium | 17 | `python/Cmedium0016.py` | 1 → 2 | 0.7112 | 935373 | 3699 / 1843141 |
| Frozen | medium | 18 | `python/Cmedium0017.py` | 1 → 2 | 0.7274 | 935373 | 3699 / 1843159 |
| Frozen | medium | 19 | `python/Cmedium0018.py` | 1 → 2 | 0.7237 | 935373 | 3699 / 1843130 |
| Frozen | medium | 20 | `python/Cmedium0019.py` | 1 → 2 | 0.7417 | 935373 | 3699 / 1843175 |
| Frozen | large | 1 | `python/Clarge0000.py` | 7000 → 7001 | 4.5636 | 7552261 | 12349 / 10328769 |
| Frozen | large | 2 | `python/Clarge0001.py` | 6000 → 6001 | 3.7934 | 7552261 | 12349 / 10328767 |
| Frozen | large | 3 | `python/Clarge0002.py` | 6000 → 6001 | 3.7567 | 7552261 | 12349 / 10328767 |
| Frozen | large | 4 | `python/Clarge0003.py` | 8000 → 8001 | 3.7563 | 7552261 | 12349 / 10328763 |
| Frozen | large | 5 | `python/Clarge0004.py` | 4000 → 4001 | 3.7994 | 7552261 | 12349 / 10328767 |
| Frozen | large | 6 | `python/Clarge0005.py` | 2000 → 2001 | 3.8740 | 7552261 | 12349 / 10328767 |
| Frozen | large | 7 | `python/Clarge0006.py` | 7000 → 7001 | 3.9559 | 7552261 | 12349 / 10328749 |
| Frozen | large | 8 | `python/Clarge0007.py` | 5000 → 5001 | 3.9048 | 7552261 | 12349 / 10328761 |
| Frozen | large | 9 | `python/Clarge0008.py` | 2000 → 2001 | 3.9842 | 7552261 | 12349 / 10338927 |
| Frozen | large | 10 | `python/Clarge0009.py` | 2000 → 2001 | 4.0346 | 7552261 | 12349 / 10338941 |
| Frozen | large | 11 | `python/Clarge0010.py` | 2000 → 2001 | 4.1330 | 7552261 | 12349 / 10338941 |
| Frozen | large | 12 | `python/Clarge0011.py` | 5000 → 5001 | 4.1467 | 7552261 | 12349 / 10338925 |
| Frozen | large | 13 | `python/Clarge0012.py` | 8000 → 8001 | 4.1654 | 7552261 | 12349 / 10338913 |
| Frozen | large | 14 | `python/Clarge0013.py` | 8000 → 8001 | 4.2088 | 7552261 | 12349 / 10338923 |
| Frozen | large | 15 | `python/Clarge0014.py` | 6000 → 6001 | 4.3340 | 7552261 | 12349 / 10338923 |
| Frozen | large | 16 | `python/Clarge0015.py` | 8000 → 8001 | 4.2767 | 7552261 | 12349 / 10338939 |
| Frozen | large | 17 | `python/Clarge0016.py` | 2000 → 2001 | 4.4410 | 7552261 | 12349 / 10338921 |
| Frozen | large | 18 | `python/Clarge0017.py` | 2000 → 2001 | 4.4642 | 7552261 | 12349 / 10338921 |
| Frozen | large | 19 | `python/Clarge0018.py` | 4000 → 4001 | 4.4441 | 7552261 | 12349 / 10338922 |
| Frozen | large | 20 | `python/Clarge0019.py` | 2000 → 2001 | 4.5085 | 7552261 | 12349 / 10338906 |
| Mixed | medium | 1 | `java/Cmedium0000.java` | 70000 → 170000 | 0.6664 | 935373 | 4389 / 1980237 |
| Mixed | medium | 2 | `python/Cmedium0000.py` | 1 → 11 | 0.6685 | 935373 | 3699 / 1842125 |
| Mixed | medium | 3 | `javascript/Cmedium0000.js` | 7000 → 17000 | 0.6393 | 935373 | 3782 / 1875397 |
| Mixed | medium | 4 | `rust/g00/Cmedium0000.rs` | 500 → 1500 | 0.6437 | 935373 | 3693 / 1861845 |
| Mixed | medium | 5 | `java/Cmedium0001.java` | 70000 → 70001 | 0.6649 | 935373 | 4389 / 1980236 |
| Mixed | medium | 6 | `python/Cmedium0001.py` | 1 → 2 | 0.6597 | 935373 | 3699 / 1842124 |
| Mixed | medium | 7 | `javascript/Cmedium0001.js` | 3000 → 3001 | 0.6683 | 935373 | 3782 / 1875387 |
| Mixed | medium | 8 | `rust/g00/Cmedium0001.rs` | 100 → 101 | 0.6701 | 935373 | 3693 / 1861843 |
| Mixed | medium | 9 | `java/Cmedium0002.java` | 60000 → 160000 | 0.6823 | 935373 | 4389 / 1981307 |
| Mixed | medium | 10 | `python/Cmedium0002.py` | 1 → 11 | 0.6992 | 935373 | 3699 / 1843138 |
| Mixed | medium | 11 | `javascript/Cmedium0002.js` | 5000 → 15000 | 0.7000 | 935373 | 3782 / 1876442 |
| Mixed | medium | 12 | `rust/g00/Cmedium0002.rs` | 200 → 1200 | 0.6879 | 935373 | 3693 / 1862901 |
| Mixed | medium | 13 | `java/Cmedium0003.java` | local → peerValue | 0.7042 | 935373 | 4389 / 1981326 |
| Mixed | medium | 14 | `python/Cmedium0003.py` | local → peer_value | 0.7080 | 935373 | 3699 / 1843157 |
| Mixed | medium | 15 | `javascript/Cmedium0003.js` | local → peerValue | 0.7113 | 935373 | 3782 / 1876440 |
| Mixed | medium | 16 | `rust/g00/Cmedium0003.rs` | local → peer_value | 0.7177 | 935373 | 3693 / 1862910 |
| Mixed | medium | 17 | `java/Cmedium0004.java` | local → peerValue | 0.7306 | 935373 | 4389 / 1981314 |
| Mixed | medium | 18 | `python/Cmedium0004.py` | local → peer_value | 0.7295 | 935373 | 3699 / 1843153 |
| Mixed | medium | 19 | `javascript/Cmedium0004.js` | local → peerValue | 0.7276 | 935373 | 3782 / 1876464 |
| Mixed | medium | 20 | `rust/g00/Cmedium0004.rs` | local → peer_value | 0.7250 | 935373 | 3693 / 1862907 |
| Mixed | large | 1 | `java/Clarge0000.java` | 20 → 120 | 4.7058 | 7552261 | 13075 / 10461519 |
| Mixed | large | 2 | `python/Clarge0000.py` | 7000 → 17000 | 3.7061 | 7552261 | 12349 / 10328510 |
| Mixed | large | 3 | `javascript/Clarge0000.js` | 6000 → 16000 | 3.6530 | 7552261 | 12393 / 10348299 |
| Mixed | large | 4 | `rust/g00/Clarge0000.rs` | 6000 → 16000 | 3.8081 | 7552261 | 12248 / 10311449 |
| Mixed | large | 5 | `java/Clarge0001.java` | 40 → 41 | 3.7747 | 7552261 | 13075 / 10461522 |
| Mixed | large | 6 | `python/Clarge0001.py` | 6000 → 6001 | 3.9005 | 7552261 | 12349 / 10328509 |
| Mixed | large | 7 | `javascript/Clarge0001.js` | 7000 → 7001 | 3.8559 | 7552261 | 12393 / 10348314 |
| Mixed | large | 8 | `rust/g00/Clarge0001.rs` | 5000 → 5001 | 3.8835 | 7552261 | 12248 / 10311377 |
| Mixed | large | 9 | `java/Clarge0002.java` | 70 → 170 | 4.0157 | 7552261 | 13075 / 10471682 |
| Mixed | large | 10 | `python/Clarge0002.py` | 6000 → 16000 | 4.0677 | 7552261 | 12349 / 10338669 |
| Mixed | large | 11 | `javascript/Clarge0002.js` | 3000 → 13000 | 4.1209 | 7552261 | 12393 / 10358474 |
| Mixed | large | 12 | `rust/g00/Clarge0002.rs` | 3000 → 13000 | 4.1361 | 7552261 | 12248 / 10321586 |
| Mixed | large | 13 | `java/Clarge0003.java` | local → peerValue | 4.1945 | 7552261 | 13075 / 10471692 |
| Mixed | large | 14 | `python/Clarge0003.py` | local → peer_value | 4.2529 | 7552261 | 12349 / 10338669 |
| Mixed | large | 15 | `javascript/Clarge0003.js` | local → peerValue | 4.2311 | 7552261 | 12393 / 10358487 |
| Mixed | large | 16 | `rust/g00/Clarge0003.rs` | local → peer_value | 4.3629 | 7552261 | 12248 / 10321608 |
| Mixed | large | 17 | `java/Clarge0004.java` | local → peerValue | 4.4272 | 7552261 | 13075 / 10471704 |
| Mixed | large | 18 | `python/Clarge0004.py` | local → peer_value | 4.4191 | 7552261 | 12349 / 10338676 |
| Mixed | large | 19 | `javascript/Clarge0004.js` | local → peerValue | 4.4700 | 7552261 | 12393 / 10358464 |
| Mixed | large | 20 | `rust/g00/Clarge0004.rs` | local → peer_value | 4.4986 | 7552261 | 12248 / 10321592 |

**Failed attempts are not acceptance evidence.** Before these complete successful runs, the additive runner stopped before indexing on a Python script variable collision; an earlier medium attempt selected FULL on the first Java numeric-prefix edit; another first Java attempt stopped on a copied Python-only target-ID assertion; and an attempt stopped on the exported JavaScript edit after two LOCAL revisions. The classifier was narrowed and independently tested against exact frozen source shapes (including import/header/call negatives and same-root cold parity), the target-ID assertion was corrected to the exact changed path with all other keys still required unchanged, and the entire cohort restarted without dropping samples. An intentionally aborted attempt for a preflight requested by the owner provided no samples. These histories are **not** counted in the 80 rows above.

Producer-drift binary-pair evidence and its distinct trust limit are in [98-pr-producer-drift.md](98-pr-producer-drift.md) and [Decision 0005](../semantic-evidence/decisions/0005-same-generation-producer-drift.md).
