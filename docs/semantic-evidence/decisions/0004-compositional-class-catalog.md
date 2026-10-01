# Decision 0004: two-step class catalog (per-file extraction, per-revision composition)

- **Status:** approved by the owner on 2026-10-01; ratified when this record merges. It changes normative text in `../../class-diagram-contract.md`. It changes no deployed schema by itself; implementation follows in #67 (see [Implementation impact](#implementation-impact)).
- **Scope:** how the Java/Python class catalog (`Catalog`, `src/classes.rs`) is built and persisted. The class DTOs, IDs, relation kinds, scoped-matching rules, HTTP API and class-diagram behaviour are unchanged.
- **Compatibility: none.** Pre-release, the owner requires no backward compatibility. Catalog output may differ from the current build **only** when a workspace-wide cap is reached (see [Behaviour change](#behaviour-change)).

## Problem

`Catalog::build` is one pass over every Java/Python file in path order. Each file's extraction draws on budgets shared by the whole workspace:

| Shared budget | Constant |
|---|---|
| total class source | `TOTAL_BYTES`, 256 MiB |
| class declarations | `CLASSES`, 20,000 |
| member and relation records | `RECORDS`, 250,000 |
| detail text | `OUTPUT_TEXT`, 64 MiB |
| declaration registry text | `REGISTRY_TEXT`, 32 MiB |
| input size | 100,000 files / 1,000,000 symbols |

It also shares the catalog-wide `warnings` list (deduplicated, at most 100) and `truncated` flag. So a file's output depends on how much budget earlier files used. A file that grows can truncate or drop classes in a later, unchanged file.

#67 publishes native changes by writing only what changed. A pinned read must be byte-identical to a full rebuild, classes included, and single-file updates must meet a p95 (≤2 s medium, ≤5 s large) that includes class projection. Under the one-pass build, the only exact way to update classes is to re-parse every Java/Python file on every publication. That is O(workspace) work per edit.

`resolve()` binds no cross-file targets today: every relation is `unmatched`, with no target or candidates. So the shared budgets and warnings are the **only** cross-file dependency in the catalog.

## Decision

The catalog is built in two steps.

1. **Per-file extraction.**
   - A file's class extraction is a function of only:
     - the file's bytes, path and language;
     - that file's measured `Symbol`s;
     - the **per-file** limits: `FILE_BYTES` (2 MiB), `FILE_CLASSES` (1,000), `FILE_REFS` (8,192), `MEMBERS` (256 per class), `VISITS`, `DEPTH`, `TEXT` (2,048 bytes per string), and the per-declaration type-parameter limits.
   - It reads no workspace-wide budget.
   - Its result is the file's ordered classes, each with its members and relations, its own warnings, and the byte, record and text sizes that the global caps charge.
   - The result is stored with the file's **graph projection**, not just its document version, because the measured `Symbol`s carry captured SCIP display labels from outside the file. It is reused for as long as that projection is.
2. **Per-revision composition.**
   - Composition walks the revision's per-file results in path order, with classes in `(path, start_byte, id)` order.
   - It applies the workspace-wide caps to those **complete** results, one class at a time:
     - **Input limit.** A revision over 100,000 files or 1,000,000 symbols gives an empty, incomplete catalog, as today.
     - **Registry caps** (`TOTAL_BYTES`, `CLASSES`, `REGISTRY_TEXT`). A file whose source would exceed `TOTAL_BYTES` is excluded, along with every later file. Classes past `CLASSES` or `REGISTRY_TEXT` are excluded. Either way, the registry is marked incomplete.
     - **Detail caps** (`RECORDS`, `OUTPUT_TEXT`). Once a class would exceed one, that class and every later admitted class keep their declaration but lose the members and relations past the cap, and are marked `truncated`. Discovery of declarations continues.
     - **Warnings.** Each admitted file's warnings and each cap's warning are added in walk order, deduplicated, keeping the first 100. The two closing warnings ("Declared types are terminal source text…" and "Incomplete class declaration registry…") follow under today's conditions. `truncated` is set when any cap or per-file limit truncated output.
   - Composition reads only stored per-file results. It never re-parses, and it runs in memory, bounded by the caps.
   - Its output is one **class projection per owning path**, plus the revision's catalog `warnings` and `truncated`.

`Catalog::build(files, nodes, cancel)` remains the full-build reference. It is defined as composition over per-file extraction of every file, and it is what #67's independent full-native snapshot uses. #67's full-snapshot parity for classes is measured against this definition, not against the current one-pass output.

## Alternatives rejected

- **Keep the one-pass output exactly, and replay or re-parse to reproduce it.** Reproducing today's mid-class budget cutoffs and warning order on a delta needs every earlier file's extraction state. In practice that means re-parsing every Java/Python file, or replaying stored per-file extraction traces, on every publication. That is O(workspace) work per edit, against a p95 that includes class projection.
- **Exclude classes from parity or from the p95, or let class rows lag behind.** That contradicts #67's acceptance criteria.

## Consequences

- **Exact parity by construction.** Delta publication and the full rebuild run the same two functions over the same inputs.
- **Delta writes.** An edit re-extracts only the changed files. Composition then rewrites only the per-path projections whose content changed: normally just the edited file's. Another path's projection changes only when a cap boundary moves across it.
- **Storage.** Per-file results live on the graph projection and per-document class projections are shared across revisions, so an unchanged file's class rows are stored once.
- **Future cross-file resolution** (binding relation targets across files) would belong in composition, and a target change would rewrite the dependent path's projection. Adding it is a separate decision.

## Behaviour change

Below every workspace-wide cap, output is identical to the current build. At a cap, the cut can fall differently. Today a detail budget can be exhausted partway through extracting a class, and a later file's extraction sees budget already used. Under this decision, the caps are applied after extraction, at class granularity, in a fixed order. Nothing depends on the old cut points.

## Required tests (#67)

- Composition over per-file extraction equals `Catalog::build` on the #72 cohorts and the existing class fixtures.
- At each workspace-wide cap (`TOTAL_BYTES`, `CLASSES`, `RECORDS`, `OUTPUT_TEXT`, `REGISTRY_TEXT`, the input limit), cover:
  - the cut;
  - `truncated`;
  - the warnings;
  - that growing an earlier file moves the cut in a later, unchanged file, and delta publication rewrites exactly that file's projection.
- A per-file result is independent of the other files in the workspace: the same bytes give the same result in any workspace.
- The warnings keep their order, deduplication and 100-entry limit.

## Implementation impact

- **`src/classes.rs`.** Split into a per-file extraction (`extract_file`) and composition (`compose`). `Catalog::build` becomes `compose` over `extract_file`. Global counters move out of extraction and into composition.
- **`src/store.rs` (#67 v8).**
  - The per-file result is stored on the document's graph projection.
  - Per-path class projections (`class_projections`, `classes`, `class_relations`) are named by each revision's manifest.
  - The catalog `warnings` and `truncated` move to the revision header.
  - Composition runs before the publication transaction, and the transaction writes only new projections.
- **`docs/class-diagram-contract.md`.** Its cap rule now points here.
- **#67 plan.** This is its `class-composition` slice. Selected-document attestation compares a path's stored projection against composition for the pinned revision.
