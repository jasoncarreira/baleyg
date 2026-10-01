# Decision 0003: per-document occurrence identity, revision-scoped semantic validity

- **Status:** proposed amendment for owner ratification. It changes normative text in `../contract-v1.md` (#22) and `../publication-rejoin-vectors-v1.md` (#11A). It changes no deployed schema by itself; implementation follows in separate reviewed work (see [Implementation impact](#implementation-impact)).
- **Scope:** occurrence identity (`OccurrenceId`) and the rules that relied on occurrence IDs being revision-bound. Syntax identity (`SyntaxId`), canonical bytes, `Revision.id`, coverage, freshness, warnings-v1, and the 11A raw envelope and proof rules are unchanged.
- **Compatibility:** pre-release, no backward compatibility required. Legacy #26 `SemanticCapture` `formatVersion:1` bytes and their published example hashes stay immutable (Decision 0002). They keep the v1 occurrence derivation as historical identity.

## Problem

Under v1, an occurrence ID hashed `{revisionId, ownerSyntaxId, kind, ordinal}`, and `Revision.id` is a complete-snapshot identity covering every source document, the toolchain, config, dependency captures and the native extractor. Any edit anywhere therefore produced a new revision, and with it a new ID for **every** call, control region and reference in **every** document.

A store that materializes native evidence must rewrite O(workspace) rows on each publication, inside the writer transaction. #67 found that this cannot meet its bounded writer hold or its single-file update targets (medium p95 ≤2 s, large ≤5 s) at the ratified 50,000/500,000-fact floors. The alternative of deriving revision-bound IDs at read time keeps the cost permanently: per-read derivation, plus an index to resolve occurrence IDs that can't be inverted. It also complicates fact caching (#71), worktree reuse and incremental rejoin.

## What revision binding was protecting

The v1 rule "occurrence-keyed evidence never crosses revisions" protects **semantic validity**, not syntax. A document can be byte-identical across revisions while a fact *about* one of its occurrences becomes wrong: an unchanged call to `g()` whose target `g` changed in another document. Revision-bound occurrence IDs enforced this by making every binding expire at every revision. The same guarantee can be stated directly on semantic records, where the staleness actually lives.

## Decision

1. **Occurrence identity is per document version.** The occurrence input is exactly

   ```text
   {contentHash:Hash, nativeProducerId:Text, nativeProducerVersion:Text, ownerSyntaxId:SyntaxId, kind:call|reference|control, ordinal:UInt}
   ```

   - The domain is `baleyg.occurrence.v2\0`, and the emitted form is `occ:v2:` followed by the first 32 lowercase hex characters (16 bytes) of the full SHA-256.
   - `contentHash` is the containing document's exact content digest.
   - `nativeProducerId`/`nativeProducerVersion` are the native producer descriptor's `id` and `version`.
   - `ownerSyntaxId` is the emitted 128-bit owner ID, which already binds source set, path and language.
   - Ordering, ordinal namespaces, duplicate rejection and collision handling are unchanged from v1.
2. **Consequence.** A byte-identical document measured by the same native producer version keeps identical occurrence IDs across revisions. Any byte change to a document changes **all** of that document's occurrence IDs and no other document's. A change of native producer version changes all occurrence IDs.
3. **Native producer versioning.** Any change to the native extractor that can change a measured occurrence's owner, kind, ordinal, range or spelling **must** change `nativeProducerVersion`. This is the same rule #71's fact cache depends on. The executable hash stays in `Producer.executableHash`, provenance and `Revision.id`, but is not occurrence identity, so a rebuild of the same extractor version doesn't churn IDs.
4. **Occurrence records keep their shape.** `Call`, `ControlRegion` and `Reference` still carry `.revisionId`, now defined as the **containing revision** in which the record is published or answered, supplied by the revision's document manifest. It is not part of occurrence identity. `.ordinal` is document-version-local.
5. **Semantic validity stays revision-scoped.** This restates the old guarantee directly:
   - Every semantic record, binding and join keyed by an occurrence ID (`CallBinding`, `Reference` resolution and targets, `Join` with occurrence candidates) is valid **only at the revision of its provenance**.
   - That revision is the captured revision for captured evidence, and the destination revision for publication-rejoined evidence.
   - An occurrence ID shared by two revisions never carries a binding, resolution, target, join or provenance from one revision to the other.
   - After a failed or omitted r2 refresh, r2 occurrences have no selected-producer binding even when their IDs equal r1's.
   - Freshness rule 2 (`possiblyStale`), `staleTarget=false` and the no-expansion rules are unchanged.
6. **11A rejoin.** For an unchanged-byte document under the same native producer version, the r2 native occurrence ID **equals** the r1 ID.
   - Rejoin still mints **new r2 provenance** (with the verified `derivedFrom`) and r2-scoped associations. All 11A validity checks are unchanged: identical authenticated bytes and key, exactly one distinct compatible r2 native candidate at the converted span and kind, the same-`SyntaxId` target rule, and the A-`failed` / B-`partial` dispositions.
   - ID equality is a join convenience, never a validity proof.
   - "No r1 call ID is attached to r2" becomes: **no r1 provenance or binding is attached to r2**.
7. **Cross-revision links.** Stable syntax IDs, and occurrence IDs of byte-identical documents under the same native producer version, are the only cross-revision identity links. Neither carries semantic validity across revisions. The #47 historical-evidence scope is unchanged: no old call binding is applied.
8. **Legacy v1 identity.** `occ:v1` with domain `baleyg.occurrence.v1\0` and the revision-bound input remains defined **only** as the identity of immutable legacy #26 `SemanticCapture` `formatVersion:1` artifacts and the existing #57 v1 vector rows. A checker validates each artifact under its own format's occurrence version. New native evidence and new `CapturedScipFactV1`-derived publications use `occ:v2`, and v1 and v2 IDs are never compared or mixed in one revision.

## Storage consequence (informative)

Publication can write only the changed documents' native rows plus a revision → document-version manifest. Pinned reads of an older revision resolve through that revision's manifest, and document versions that no retained revision references can be garbage-collected. This is the model #67's native-correctness work should implement. No read-time ID derivation or occurrence-ID resolution index is needed.

## Vectors

[`../publication-rejoin-vectors-v1.md`](../publication-rejoin-vectors-v1.md#occurrence-identity-v2-decision-0003) gives the exact v2 rows for the #57 fixture's A.js call and reference. They were computed from #22 canonical bytes with `tools/semantic-contract/json.mjs::canonicalBytes` and independently with Python `hashlib`. The same method reproduces the existing v1 row `r1/call` (`ccc4d599…`).

## Implementation impact

None of this is done by this docs change:

- **`src/native_ids.rs`:** occurrence registration switches to domain `baleyg.occurrence.v2\0`, prefix `occ:v2:` and the new input. `src/native_evidence.rs`: bump the native producer version.
- **`src/mcp/catalog.rs`, `src/mcp/tools.rs`:** the `occ:v1:` ID patterns become `occ:v2:`.
- **`tools/semantic-contract/identity.mjs`** and the semantic-contract tests and fixtures that compute occurrence IDs (`formats`, `identity`, `normalization`, `record-*`, `graph*`, `answers`, `counts`, `example-fixture`): v2 for new-format fixtures, v1 retained only for legacy `formatVersion:1` bytes.
- **Native tests** (e.g. `tests/python_indexer.rs`) that pin occurrence IDs.
- **#67:** the native-correctness slice is re-scoped to per-document storage plus a revision manifest. **#87–#90:** wording that assumes r2-minted occurrence IDs (#89 rejoin and #90 in-transaction rejoin) is updated to "same IDs for unchanged documents; new r2 provenance and associations".
