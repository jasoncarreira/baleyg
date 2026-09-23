# MCP read-only contract

Status: **direction accepted by the owner (2026-09-23); the mechanics in this document are proposed
until Stage 1 ratifies them.** It binds to MCP specification revision `2026-07-28` and its stdio
transport, and defines the agent-facing surface of the [local topology](local-topology.md). It
supersedes the earlier grant-based pilot contract: grants, principals, budgets, enrollment latching,
grant handoff files and HTTP tool routes are removed.

## Boundary

- `baleyg mcp` is a stdio MCP server launched by an agent client. It serves exactly one workspace,
  chosen by the [discovery order](local-topology.md#workspace-discovery), for its whole lifetime, and
  opens no network listener.
- Tools read only cached source and indexed evidence. No tool triggers indexing, runs a semantic
  producer, builds, downloads, writes durable data, calls a provider, executes repository code, opens
  a terminal, reads the live working tree, or selects another workspace.
- The catalog has four tools: `baleyg_workspace_describe`, `baleyg_find_symbols`, `baleyg_inspect`,
  `baleyg_read_source`. The first release of `baleyg_inspect` has three views: `declaration`,
  `outgoing_calls` and `incoming_calls`, over syntax-tier evidence. Later stages add `call_paths`,
  `usages`, `type_hierarchy`, `implementations` and `coverage`, and semantic evidence in every view,
  without changing existing schemas.

Direct terminal agents, agents in Herdr panes, and agents reached through ACP use the same server:

```json
{ "mcpServers": { "baleyg": { "command": "baleyg", "args": ["mcp"] } } }
```

A remote agent cannot reach a local stdio server; that needs a separately secured bridge.

## Protocol and lifecycle

- MCP `2026-07-28`, stateless: every request carries its protocol version and capabilities in `_meta`.
  The server answers `server/discover`, `tools/list` and `tools/call`. Stage 1 decides which earlier,
  `initialize`-based revisions it also accepts, based on the agent clients in use.
- stdio framing: one newline-delimited UTF-8 JSON-RPC message per line. Stdout carries only protocol
  messages; diagnostics go to stderr. Cancellation arrives as `notifications/cancelled`.
- The server exits when stdin reaches end-of-file. It never daemonizes or outlives its client.
- **Startup.** The server tries the leader lock. As leader it runs the catch-up scan, or the initial
  index, on a background thread; this is part of process start, not of any tool call.
  `server/discover` and `tools/list` answer immediately. Until the index is reconciled, describe
  reports `indexing` or `reconciling` with progress, and evidence tools return `index_not_ready`.

## Evidence basis and pins

Every evidence result comes from one SQLite read transaction and reports
`evidenceBasis: {indexGeneration, indexRevision}`. Pins are optional:

- `expectedBasis` (`indexGeneration` and `indexRevision` together; a revision alone is rejected as
  `invalid_request`), when supplied and not current, returns `revision_conflict` with `currentBasis`.
- `baleyg_read_source` may take `expectedContentHash`; if the cached file's hash differs it returns
  `revision_conflict` with the current hash.

## Result fields

Every evidence item reports `evidenceTier` (`syntax` or `semantic`), `semanticBasis` (`null` for
syntax), `freshness` (`fresh`, `possiblyStale`, `stale`, `unavailable`), `staleBecause` (reasons; empty
when fresh), and for bindings a `disposition`: `resolved`, `external`, `ambiguous`, `unresolved`,
`unsupported`, `dynamic`, `declarationOnly` or `staleTarget`. Freshness never promotes a disposition.
A `staleTarget` binding has no current target ID and is never presented as resolved. In the first
release every item is `syntax`, so these fields are present with syntax values and later stages only
add semantic values.

## Tools

All inputs are JSON objects with `schemaVersion: 1`; unknown fields are rejected. Tool annotations
are hints, not permissions.

| Tool | Input | Output |
| --- | --- | --- |
| `baleyg_workspace_describe` | none | Workspace label (not an absolute path), current basis, index state and progress, per-language extraction tier and coverage, tool and schema versions, limits |
| `baleyg_find_symbols` | `query` (literal name/ID substring, 1–256 UTF-8 bytes), optional `limit` (default 20, 1–50), optional pin | Symbol summaries: stable ID, name, kind, relative path, range, content hash, result fields; no source body |
| `baleyg_inspect` | `symbolId` (1–8192 bytes), `view`, view-specific bounds, optional pin | `declaration`: declaration metadata. `outgoing_calls` / `incoming_calls`: depth-one call sites with call-site IDs, ranges, target or caller IDs, result fields, and bounded literal `calleeText`; max 50 calls |
| `baleyg_read_source` | `path`, `startLine`, `endLine`, optional `expectedContentHash` or pin | Cached text only, content hash, exact line/byte range; max 200 lines and 16 KiB |

Paths are validated relative indexed paths: no absolute path, backslash, colon, NUL, empty, `.` or `..`
segment; max 4096 UTF-8 bytes. Lines are 1-based inclusive within the cached file; byte offsets are
0-based UTF-8 half-open. Tools never open a source path on disk.

An unresolved call site still records measured call syntax. Never promote a candidate or unresolved
target to a resolved or runtime binding. Serialize an explicit projection, never native DTOs:
`calleeText` is capped at 1024 UTF-8 bytes per call at a code-point boundary with
`calleeTextTruncated: true`. Symbol lookup uses literal `lower`/`instr` semantics with deterministic
ordering (exact name, name prefix, other substring; then name/ID). Calls are ordered by path, range and
ID. Fetch one extra record to detect truncation; never report a clipped list as complete.

Success envelope: `{schemaVersion: 1, requestId, evidenceBasis, data, warnings, truncated,
truncationReason}`.

## Bounds

16 KiB request, 64 KiB complete response, 16 KiB source text, 5-second deadline, small per-process
concurrency cap. Over-cap limits are rejected, not expanded. There are no per-session budgets; per-call
limits do not bound aggregate use across many processes of one user.

## Errors

Failures return a tool result with `isError: true` and
`{schemaVersion: 1, error: {code, message, retryable, currentBasis}, requestId}`, where `currentBasis`
is the current basis or `null`. Malformed requests use protocol errors. Errors never expose SQL,
absolute paths outside the workspace, or source.

| Code | Meaning |
| --- | --- |
| `invalid_request`, `range_too_large` | Malformed or over-limit input |
| `revision_conflict` | A supplied pin or content hash is not current; re-query |
| `index_not_ready` | Initial index or post-restart reconciliation still running; retryable |
| `not_found` | Missing symbol or cached path at the current basis |
| `too_many_requests` | Concurrency cap reached; retryable |
| `deadline_exceeded` | Bounded work cancelled; no partial evidence |
| `store_unavailable` | Index unreadable and being rebuilt, or the workspace root no longer names the directory this server started in; no repair from a tool call |

On cancellation, interrupt the read where supported and emit nothing further for that request.

## Threat limits

The boundary is the OS user; tools add bounded, typed, read-only access, not isolation. Configuring the
server discloses the whole indexed checkout to the agent and its provider. MCP cannot attest the
downstream provider. Repository text is untrusted evidence. Jev/ACP allowances are untouched.

## Acceptance tests

Fixtures and a synthetic MCP client; no model, provider, repository commands or builds.

1. **Happy path:** describe → find → inspect (all three views) → read_source, with matching basis and
   hashes, from a direct-terminal client and an ACP-bridge fixture, with identical schemas.
2. **Workspace:** launched in a subdirectory, a linked worktree, a submodule, and a non-Git root, the
   server serves exactly the chosen workspace; two worktrees never see each other's evidence; home and
   filesystem root are refused without `--workspace`.
3. **Lifecycle:** stdin end-of-file and `kill -9` of the client leave no server process; a restart
   needs no owner action.
4. **Startup:** with no index, `server/discover` and `tools/list` answer within their deadlines while
   indexing runs; evidence tools return `index_not_ready`, then succeed.
5. **Leader:** with several servers on one checkout, exactly one watches and writes; killing it makes
   another take over, reconcile, and serve again; edits made while no leader ran are picked up before
   any evidence is served; a crashed leader's `reconciled` marker is never accepted once the successor
   has recorded its incarnation (the documented takeover window serves only already-reconciled,
   basis-labelled evidence). Moving the
   checkout, or putting a different directory at its path, stops the old server from serving it.
6. **Live edits:** an edit is reflected within the ratified incremental budget; a body-only edit keeps
   every declaration ID; an added declaration resolves a previously unresolved call elsewhere.
7. **Pins:** a stale `expectedBasis` or `expectedContentHash` conflicts with the current basis; a
   revision without its generation is rejected; omitted pins answer from the current revision; no
   answer mixes revisions.
8. **No side effects:** tool calls start no indexing, producer, build, download, provider call or
   durable write, and never read the live working tree.
9. **Bounds and cancellation:** malformed or oversized inputs fail or report exact truncation; a
   cancelled or timed-out read emits nothing afterwards.
