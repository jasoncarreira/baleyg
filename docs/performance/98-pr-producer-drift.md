# PR #98: same-generation producer drift regression

Run on the PR branch from the repository root after building both real binaries:

```sh
cargo +1.99.0 build --locked
cargo +1.99.0 build --locked --release
python3 tools/verify-98-producer-drift.py
```

The [regression runner](../../tools/verify-98-producer-drift.py) uses a private temporary HOME, a three-file workspace and **three different real executable files**. The third file is built from a disposable source copy with a deliberate Java/JS normalized lookup change but the **same** `nativeProducerVersion`; no tracked source or Factory state is changed. It queries old and new selected pins through the authenticated HTTP API. Its output is structured as `PR98_PRODUCER_DRIFT_RESULT`.

Observed on the final PR runtime source with Rust 1.99.0 and a separate ignored `CARGO_TARGET_DIR` outside the worktree (the original AC8 performance binary remains a distinct default-toolchain build):

| Binary | SHA-256 |
| --- | --- |
| debug | `152c1be07e0cb61c63b0cf1dee3014bb830d20795e852157b6ee3ef804d425ff` |
| release | `bcdd9e984feddc3ad2b051915a738b11579a679fcdc2a9195c71d164d41e7cba` |
| divergent same-version | `c58d359704a6f65fbd710cf3ebb3805a9aaf5a98e32e9e2f999688f94ad9b29e` |

Debug indexing created pin 1. Release-mode `status` and `index` performed full same-generation publication under the new executable SHA. Both old pin 1 and new pin 4 returned HTTP 200 for selected `A.java` with the correct immutable source. The generation remained one UUID. The `native_producers` origin SHA remained the debug SHA; each revision's executable input SHA equaled its own `revision_producer_bindings.producer_sha`.

The divergent binary emitted `index-mode full` and refused publication with `native_producer_version_required`. Before and after that attempt, the generation, head revision, four retained revision headers, twelve manifest links and four per-revision bindings were identical. Both old and new selected HTTP pins remained readable. The two pre-existing durable queue rows were unchanged; one newly accepted queued request made the queue row count 2 → 3. No pin was released or generation rotated.

This test is **not** a speed measurement. See [Decision 0005](../semantic-evidence/decisions/0005-same-generation-producer-drift.md) for the unkeyed binding's precise T00 limit. The ordinary frozen AC8 runner and separate mixed-body runner are [here](../../tools/verify-67-performance.py) and [here](../../tools/verify-67-mixed-performance.py); their measured results are reported separately in [PR #98 AC8 evidence](98-pr-ac8.md).
