# API handoff: Baleyg local daemon v1

## Purpose

Inspect indexed JavaScript without executing the workspace. This is an evidence API,
not a question-answering service. Keep the measured graph separate from later ACP/Jev
presentation decisions. Display static call views, not runtime sequences.

## Integration

Use the same origin as the bound daemon (default `http://127.0.0.1:8877`). All `/api/*`
requests require `Authorization: Bearer <64-lowercase-hex-token>`. Read the private token
file outside the browser and paste it; never put it in URLs, browser storage or logs.
No CORS dev-server exception. Credentials are not cookies. Disconnect must clear cached
source as well as the token. Never render source, names, labels or notes as HTML.

[Canonical endpoint table and operational constraints](daemon-v1.md#http-and-security).
Graph schema version 1; database schema version 2. `src/model.rs` defines serialized DTOs.

## Main flow

1. `GET /api/status`. Revision 0 means no snapshot; show an explicit Index action.
2. `POST /api/index` with `{}` (or `{expectedRevision: n}`) starts a background job, 202.
3. Poll `GET /api/jobs/{id}` only while running/cancelling. Indexing never starts on a read.
4. `GET /api/symbols?q=transition&limit=50` returns `{revision, items: Symbol[]}`.
5. `POST /api/query` with the selected ID and bounded options returns `ViewResult`.
6. `GET /api/source?path=<encoded-relative-path>&revision=<view.revision>` returns
   `{revision, file:{path, hash, language, text}}`. On 409, refresh the query; never pair a
   new source body with old source spans. An already cached old body is valid only for
   that exact revision/path pair.
7. Save queries/notes through the routes below. Read orphan status on every reload.

Source hashes are SHA-256. Ranges use **UTF-8 bytes**, not JavaScript string offsets.
Lines and byte columns are 1-based, and end positions are exclusive. Whole-line highlighting
is safe; exact JavaScript slicing needs a UTF-8 byte-to-string-offset conversion.

## DTOs

```ts
type SemanticState = 'fresh' | 'stale' | 'unavailable';
type Resolution = 'internal' | 'external' | 'unresolved' | 'ambiguous';
interface SourceRange {
  startByte: number; endByte: number;
  startLine: number; startColumn: number; endLine: number; endColumn: number;
}
interface Provenance { source: string; semantic: SemanticState }
interface Symbol {
  id: string; name: string; kind: 'module'|'function'|'method'|'class';
  path: string; range: SourceRange; parent: string|null; accessor: boolean;
  provenance: Provenance;
}
interface CallSite {
  id: string; caller: string; calleeText: string; path: string; range: SourceRange;
  target: string|null; candidateSymbols: string[]; resolution: Resolution;
  ordinal: number; regions: string[]; callbackArguments: string[]; provenance: Provenance;
}
interface ControlRegion {
  id: string; kind: string; label: string; parent: string|null; owner: string;
  path: string; range: SourceRange;
}
interface ViewQuery {
  seed: string; depth?: number; maxNodes?: number; maxCalls?: number;
  includeCallbacks?: boolean; excludePaths?: string[];
}
interface ViewResult {
  revision: number; query: Required<ViewQuery>; nodes: Symbol[]; calls: CallSite[];
  regions: ControlRegion[]; truncated: boolean; omittedNodes: number; warnings: string[];
}
interface SavedView {
  id: string; title: string; query: ViewQuery;
  pins: Record<string,{x:number,y:number}>; hidden: string[];
}
interface SavedViewState { view: SavedView; orphanedIds: string[] }
interface Annotation { id: string; nodeId: string; body: string }
interface AnnotationState { annotation: Annotation; orphaned: boolean }
interface IndexJob {
  id: string; state: 'running'|'cancelling'|'completed'|'cancelled'|'failed';
  progress: {phase: string; completed: number; total: number};
  revision: number|null; error: {code:string,message:string}|null;
  startedAt: string; finishedAt: string|null; // epoch milliseconds
}
```

IndexStatus contains workspaceRoot, revision, indexedAt (epoch-millisecond string or null),
`stats` (files/symbols/calls/regions/internal/external/unresolved/ambiguous/parseErrorFiles,
semanticState, changedFiles), and diagnostics `{path:string|null,code,message}[]`.
Global semanticState measures paired-manifest freshness, not working-tree liveness or
perfect semantic coverage. Surface diagnostics and parse errors separately.

## Saved state

- `GET /api/views` -> SavedViewState[]. `GET /api/views/{id}` -> SavedViewState.
- `PUT /api/views/{id}` with a full SavedView -> SavedViewState. Body ID must match route.
- `DELETE /api/views/{id}` -> 204, or 404 if absent.
- `GET /api/annotations` -> AnnotationState[].
- `PUT /api/annotations/{id}` with a full Annotation -> AnnotationState.
- `DELETE /api/annotations/{id}` -> 204/404.

PUT is full replacement/upsert, not PATCH. Preserve unknown-to-your-UI pin/hidden data
when editing an existing record. The basic inspector only creates queries without layout
overrides. A future canvas must apply pins and hidden IDs itself; `/api/query` returns
facts and does not silently alter them using saved preferences.

Do not discard orphaned records. Local/provisional IDs include a source hash and may
orphan on any file edit. A later byte-identical rebuild can reattach them. There is no
automatic rename reconciliation. Global SCIP IDs retain the indexer's identity semantics.

## Limits and error states

Depth defaults 1, maxNodes 40, maxCalls 200, callbacks false, excluded prefixes empty.
Depth ≤5, nodes 1–150, calls 1–500. Seed is mandatory. Depth 0 is seed only. Keep the
seed even when excluded. A call target can lie outside the displayed node budget.
`truncated` is not an empty-success state; explain it and allow an explicit refinement.
`omittedNodes` only counts encountered omitted targets, not the entire unseen graph.

Unknown request fields are rejected. JSON bodies ≤1MiB. Titles ≤256 bytes, notes ≤65536
bytes, record IDs ≤128 ASCII `[A-Za-z0-9._-]`, opaque symbol IDs ≤8192 bytes, pins/hidden
lists ≤150. These are byte limits, not HTML maxlength character guarantees.

Errors: `{error:{code,message}}`. 401 auth, 403 Host/Origin, 404 missing seed/data/job,
409 revision_conflict or job_active, 413 oversized body, 400/422 malformed input. Failed
job messages are sanitized; use diagnostics from a successfully indexed snapshot for
extraction warnings. Jobs are in-memory (latest 100) and are not restored after restart.
`POST /api/jobs/{id}/cancel` requests cancellation. It may still complete if commit won
that race. Never render cancelled before the job reports it.

## Scope caveats

No live working-tree source endpoint; no historical server-side revisions. No stream/PTY,
provider requests, arbitrary roots/commands, filesystem mutation or model-created edges.
Callbacks, class instance initialization, dynamic dispatch, branches and nested expression
evaluation are not a runtime sequence. Display immediate interactions first. Expand
additional evidence only on request; relevant evidence need not become a visible box.
