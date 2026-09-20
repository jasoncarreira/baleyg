# Extraction experiment results

## Decision

**Proceed with the syntax + SCIP approach, but do not call M4 passed.**
The pipeline works on real JavaScript and the storage pattern survives the tested
failures. The unscoped real diagrams are still too noisy, and a lexical call walk is
not an execution sequence. Prove view scoping and callback navigation before building
the full shell or adding agents.

## Executed scope

- Copied 18 production JavaScript files (~4,800 lines) from
  `opencode-feature-factory/packages/feature-factory` into an isolated input directory.
  Source hashes remained unchanged in the original checkout (18/18).
- Installed pinned tools only under `experiments/extraction`: scip-typescript 0.4.0,
  tree-sitter 0.25.1, JavaScript grammar 0.25.0; TypeScript resolved to 5.9.3.
  Added Node 24 type declarations so built-in APIs have semantic symbols.
- Indexed the real snapshot and two small synthetic fixtures. Extracted actual call
  expressions with tree-sitter; resolved their callee tokens through SCIP. No source
  application, source tests, agent session, database or external application build was executed.
- Ran `npm run experiment`: every stage exited 0. **11 extraction tests and 5 storage
  tests passed**, plus viewer smoke assertions.
- Opened the SVG viewer in a real browser. Checked graph selection, click-to-source,
  and bounded rendering; the viewer worker also checked a 390px viewport.

Reproduce with [the experiment README](../../tools/extraction/README.md). Compact evidence is in
[observations.json](extraction/observations.json); detailed generated command output is
in `extraction/run-results.json`. Screenshots:
[fixture](extraction/fixture-view.png), [real flow](extraction/feature-factory-view.png).

## Measurements

| Metric | Feature Factory |
| --- | ---: |
| JavaScript files | 18 |
| Module/function/method nodes, including callbacks | 372 |
| Syntactic call sites, including constructors | 2203 |
| Resolved internal targets | 805 |
| Resolved external callable symbols | 816 |
| Unresolved | 269 |
| Ambiguous candidates | 313 |
| Syntax parse errors | 0 |
| Reference occurrences with enclosing ranges | 0 |
| Definitions with enclosing ranges | 214 |
| Final-run index command wall time | 2.64 s |
| Final-run extractor wall time, including Node startup/output | 3.01 s |
| Final-run extraction inside process | 2.68 s |

Timings varied across local runs (indexing was about 1.1–2.6 seconds). These are small,
local measurements, not a performance budget or scalability result. Warm, capped
in-memory traversal averaged 0.004–0.007 ms over 1,000 repetitions in the browser;
that excludes database access, graph loading and rendering. No SQLite graph-query
benchmark was performed. Resolution counts are **not** independently measured
precision/recall on all 2,203 call sites.

## Findings that change the spec

### 1. SCIP references are not calls

The indexer emitted **zero reference enclosing ranges** in both the real snapshot and
the main fixture. Definitions did have ranges. The original reference-to-call folding
assumption is not supported by this output.

`register(transform)` resolves both symbols, but only `register` is called here.
`injected(callback)` can resolve the callback parameter's identity without knowing
which function will execute. The fixture verifies both distinctions. Getter access is
another trap: `obj.f()` where `f` is a getter calls its returned value, not the getter
as the invocation target. A regression test prevents that false edge.

Use AST call sites and caller ownership as the structural evidence. Treat semantic
references as resolution evidence, not a ready-made execution graph.

### 2. Functions/modules are first-class nodes

Feature Factory is primarily functions. A class/method-only schema cannot represent
it naturally. Anonymous callbacks also need their own nodes. A callback argument is
shown as a boundary; its body is not automatically appended to the caller's sequence.
The lock callback contains much of the interesting write flow, so selecting/explaining
those callback bodies is required for a useful view.

### 3. Source order is not evaluation order

`readRun` contains `validateRun(JSON.parse(readFileSync(join(...))))`. Sorting by source
location puts the outer call first, although the arguments execute first. Branch arms
are alternatives; callbacks and async handoffs are not synchronous continuations.

The viewer therefore labels its output **static source order, not an execution trace**.
It does not infer return arrows. A production sequence view needs an evaluation-aware
representation plus explicit scheduling/dispatch boundaries.

### 4. Branch regions need identities, not just depth

The fixture distinguishes nested if/else arms, loops and try/catch context. Review
also caught computed method keys assigned to the wrong caller, merged ternary arms,
missing short-circuit guards, and loop initializers incorrectly treated as repeating.
These were fixed and covered by a separate regression fixture. Parenthesized and
member-expression callback arguments now retain known callback identities.

This is still not a full JavaScript control-flow engine. Optional chaining, exception
propagation, switch fallthrough, yields and async causality remain unproven.

### 5. Identity has an explicit limit

An actual fixture body edit followed by re-indexing preserved the exported function's
SCIP id. Renaming that function and re-indexing changed the id, and references resolved
to the new symbol. Durable notes attached to a removed id must become visible orphans,
not silently reattach by name. The SQLite test exercises that orphan behavior.

Local symbols need document scoping. The spike's source-offset call-site/provisional
ids are not a durable identity solution. Moves, versions and annotation migration still
need a defined production contract.

### 6. Conservative staleness works; incrementality is not proven

Changing, adding or deleting an input, or changing root package/config metadata,
invalidates all SCIP resolution in the snapshot while retaining current syntax.
Retained reference facts are marked stale. This avoids silently combining fresh syntax
with old resolved targets, but it is deliberately coarse. No reverse-dependency
maintenance or background watcher was built. Re-run after tool/dependency changes.

### 7. SQLite needs real transactions, not just revision numbers

Three independent WAL connections verified that a pinned reader sees all of revision
A while a writer replaces it with B. A fresh reader sees complete B after commit.
Injected errors and AbortError at deletion, partial insertion and precommit checkpoints
rolled back cleanly; reopening the database and subsequent publication also worked.

Deleting the cache reproduced 13,733 canonical rows from the same graph,
including when node input order was reversed. A note in the separate workspace database
survived rebuilding; changing its target identity produced an explicit orphan.

This proves the bounded transaction pattern, **not** actual worker cancellation,
process-crash/power-loss behavior, multi-database atomicity, production schema design,
or byte-identical SQLite files/pixels.

## Product finding: readability is not solved

The tiny branch fixture produces four readable messages with source links. But all
three real seeds—`readRun`, `transition`, and `coordinateRunJsonTransition`—reach the
60-message cap at depth 3. Validation helpers and library operations dominate the
view; many lifelines require horizontal scrolling.

A cap prevents runaway rendering; it does not make the result useful. The next view
experiment should add explicit expansion/collapse, standard-library and validation
filters, module-level grouping, and callback navigation. Try these deterministic
controls before introducing agent scoping. A user still needs to judge whether the
result beats reading the source.

## Future language fixtures

Private application details from the original report are excluded from the repository.

Mimir was inspected as a future Python/async stress fixture, but not indexed here.
`build_memory_index` is the baseline; search thread handoffs and dispatcher queues are
later partial-resolution tests. Do not modify its live deployment environment.

## Next steps

1. Keep the corrected model assumptions in `SPEC.md`; do not freeze Appendix A yet.
2. Improve scoping/callback navigation on the current real graph and get a usefulness
   judgment before expanding the application shell.
3. Evaluate a separately authorized Java fixture after reviewing compilation-side effects.
   Validate overload selection and stop at framework repository/event boundaries.
4. Only then start the Rust implementation against these fixtures. Preserve the spike
   as evidence and regression data; do not promote this JavaScript code to production.
