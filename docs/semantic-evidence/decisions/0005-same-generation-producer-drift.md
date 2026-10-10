# Decision 0005: same-generation executable drift (PR #98 owner ruling)

> The project was renamed from Baleyg to Trellis on 2026-10-10 (#125). This record uses the new name.

**Status:** owner-ratified PR repair for schema 8 / current native extractor.

`native_producers.executable_hash` is the immutable **generation-origin** descriptor, not a gate against an ordinary executable rebuild. A new executable hash keeps the generation, old pins and `requests.db`. It requires FULL fresh capture, measurement, extraction and validation. New revisions retain their own executable Present SHA and an additive per-revision binding over canonical persisted selector observations, executing producer identity and revision-header hashes. Selected new-pin reads recompute the binding and refuse missing or mismatched fields. Existing validated pins remain readable under their original T00 trust-as-written publication boundary.

Decision 0003 still governs identity: executable SHA belongs to revision provenance, not `extractionContext`, `occ:v2`, `document:v1` or native producer version. Fully remeasured facts matching an existing per-document normalized witness may attach that immutable version under new revision provenance. Different measured facts for equal content/context/producer version **must fail** with an actionable producer-version-bump error and no publication. Real schema/extractor mismatch continues through the separate obsolete-marker ruling; ordinary binary drift does not recreate or rotate the index.

**T00 limit:** The new binding is an *unkeyed persisted consistency check*. It detects missing and unilateral/corrupt fields. It is not an external MAC and does not resist a coordinated same-user rewrite of inputs, binding and header. Never call it tamper-proof. Selected source/native/graph/class row attestation and ordinary SQLite corruption refusal remain required.
