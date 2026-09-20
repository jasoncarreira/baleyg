# Baleyg extraction tools

A disposable JavaScript spike for joining tree-sitter call sites with SCIP symbols,
viewing bounded static call sequences, and testing SQLite revision publication.
This is not the Rust application or a production sequence engine.

## Offline tests and viewer

Requires Node 24 (the storage tests use experimental `node:sqlite`), npm, and a native
build toolchain if tree-sitter has no prebuilt binary for your platform.
Dependencies are pinned in `package-lock.json`. If they are not installed, a separate
`npm ci` step needs network access; it is not part of the offline tests.

```sh
cd tools/extraction
npm test
npm run view
```

`npm test` parses saved source and SCIP data. It does not execute the copied
Feature Factory project, run an indexer, download packages, or call a provider.
Fresh lifecycle test results go to ignored `.generated/lifecycle-results.json`.
Historical results in `docs/research/extraction/` are not overwritten.

Open http://127.0.0.1:8874/viewer.html. The server binds only to loopback and serves
only the viewer and saved graphs. Stop it with Ctrl-C.

### Separate index-generation commands (not part of offline tests)

`npm run test:reindex` generates SCIP indexes for temporary synthetic fixtures.
`npm run experiment -- --source /path/to/feature-factory` replaces the saved source
snapshot, indexes it, and regenerates fixture artifacts. Both commands invoke SCIP;
only run them with explicit approval. Neither command was run during relocation.

The source option copies only `bin/`, `core/`, `observe/`, and `state/` JavaScript plus
package metadata into `tests/fixtures/extraction/inputs/feature-factory/`. It does not
execute that package, install its dependencies, or edit the original repository.
Omit `--source` to reuse the snapshot. Generated runner reports go to `.generated/`.

## What to inspect

- **Validation fixture → branchLoop:** distinct nested branches and a loop.
- **Validation fixture → callbackReference:** a callback reference is not a call.
- **Feature Factory → readRun:** real cross-file resolution and click-to-source.
- **Feature Factory → transition:** callbacks are boundaries, not automatic execution.

Arrows open read-only source from the graph's snapshot, not the live repository.
The viewer expands internal calls to depth 3 and caps output at 60 messages.
External/unresolved/ambiguous calls are dashed. Source order is **not execution order**.
All three selected real flows currently hit the cap: this is a readability finding,
not evidence that M4 is finished. Branch contexts are labels, not full UML frames.

## Files

| File | Purpose |
| --- | --- |
| `run.mjs` | Reproducible indexing, extraction, and validation runner |
| `extract.mjs` | JavaScript syntax/SCIP join; retains reference facts separately |
| `../../tests/fixtures/extraction/fixture/`, `fixture-expected.json` | Hand-labelled 12-call correctness fixture |
| `../../tests/fixtures/extraction/edge-fixture/` | Accessor, computed-key, guarded-expression and callback regressions |
| `extract.test.mjs` | Offline extraction and freshness tests |
| `extract-reindex.test.mjs` | Opt-in rename/re-index integration test |
| `lifecycle.mjs`, `lifecycle.test.mjs` | Bounded WAL publication/rebuild/annotation experiment |
| `viewer.html`, `viewer-smoke.mjs` | Static SVG viewer and traversal checks |
| `../../docs/research/extraction/` | Historical timings, results and screenshots |
| `../../tests/fixtures/extraction/` | Saved source, graphs, SCIP and hash fixtures |

Snapshots and graph outputs contain source text and are inert test fixtures, not
runnable packages. The lockfile pins the tool versions. No agent inference,
external database connection, or application code execution is part of offline tests.
See [results](../../docs/research/EXTRACTION-RESULTS.md) for findings and limits,
and [research evidence](../../docs/research/extraction/README.md) for provenance.

## Important limits

- This is JavaScript-only. It does not implement class/ERD extraction or LSP.
- Call-site and parser-only identities include source offsets. They are not durable
  across edits. Local SCIP symbols are document-scoped, not guaranteed edit-stable.
- Unknown callback targets, aliases and constructor candidates may remain unresolved
  or ambiguous. Resolution counts are not a precision/recall measurement.
- Any changed/added/deleted JS input or changed root package/config file invalidates
  the entire semantic layer. Fresh syntax still appears. This is conservative, not
  incremental reverse-dependency invalidation. Dependency/lockfile/tool changes need
  a complete rerun; the input hash map alone is not a production cache key.
- Regions cover the tested constructs, not complete JavaScript control flow. Optional
  chaining, exceptions, switch fallthrough and async causality need further work.
- Storage tests inject an AbortError; they do not kill a worker or simulate power loss.
  The two databases do not share an atomic transaction. Rebuild equality is semantic
  row equality, not SQLite file-byte or rendered-pixel equality.
