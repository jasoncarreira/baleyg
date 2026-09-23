# MCP read-only contract

Status: **direction accepted by the owner (2026-09-23); mechanics proposed until Stage 1 ratifies
them.** It binds to MCP specification revision `2026-07-28` and its stdio transport. It defines the
agent-facing MCP surface for the [local topology](local-topology.md) and supersedes the earlier
grant-based pilot contract: owner-issued grants, limited principals, budgets, enrollment latching,
grant handoff files and HTTP tool routes are removed.

## Boundary

- `baleyg mcp` is a stdio MCP server launched by an agent client. It serves exactly one workspace,
  chosen by the discovery order in [local topology](local-topology.md#workspace-discovery-and-identity)
  (explicit `--workspace`, Git top level, nearest ancestor with a workspace UUID, working directory;
  home and filesystem root refused unless explicit), for its whole lifetime. It opens no network
  listener.
- It reads committed snapshots of the workspace's index, found through the workspace record
  ([local topology](local-topology.md#placement-and-overrides)). It may also be the watcher leader and
  may publish native refresh; neither role changes what tools can do.
- Tools read only cached source and indexed evidence. No tool triggers indexing, runs a semantic
  producer, builds, downloads, writes durable data, calls a provider, executes repository code, opens
  a terminal, reads the live working tree, or selects another workspace.
- The catalog has exactly four tools: `baleyg_workspace_describe`, `baleyg_find_symbols`,
  `baleyg_inspect`, `baleyg_read_source`. `baleyg_inspect` views are `declaration` and
  `outgoing_calls` below, plus `incoming_calls`, `call_paths`, `usages`, `type_hierarchy`,
  `implementations` and `coverage`, whose input and output schemas Stage 1 ratifies from the semantic
  evidence contract. Every view follows the bounds, freshness, error and pin rules in this document.

Direct terminal agents, agents in Herdr panes, and agents reached through ACP all use the same
server and catalog. Illustrative client configuration:

```json
{ "mcpServers": { "baleyg": { "command": "baleyg", "args": ["mcp"] } } }
```

A remote agent cannot reach a local stdio server. That needs an explicitly secured bridge, such as
the proposed Mimir provider extension, and is outside this contract.

## Protocol and process lifecycle

- Protocol: MCP `2026-07-28`, stateless. Every request carries its protocol version and capabilities
  in `_meta`. The server answers `server/discover` with its supported versions, and answers
  `tools/list` and `tools/call` without any session state. Stage 1 decides which earlier,
  `initialize`-based revisions the server also accepts, based on the agent clients in use; no other
  session, subscription or server-initiated behaviour is added.
- stdio framing: one newline-delimited JSON-RPC message per line, no embedded newlines, UTF-8.
  Cancellation arrives as `notifications/cancelled`.
- Exit on stdin end-of-file. Exit when the parent process dies (Linux `PR_SET_PDEATHSIG`, macOS kqueue
  `NOTE_EXIT`). Never daemonize, never fork a long-lived child, never outlive the client.
- Stdout carries only MCP messages. Diagnostics go to stderr, with no source text or paths outside
  the workspace.
- **Launch-time indexing.** If no revision is published, the process asks the native scheduler to
  index when it starts, on a background thread. This belongs to process start, not to any tool call.
  `server/discover`, any legacy `initialize`, and `tools/list` answer immediately. Describe reports `indexing` with progress;
  evidence tools return `no_published_index` until the first revision lands.
- The spec says a client should restart a server that exits unexpectedly; a restart simply starts a
  new process. There is no credential to consume or reissue.

## Evidence basis and revision pins

`evidenceBasis` is `{indexGeneration, indexRevision}`. The generation is bound to the index file's
identity; a copied, restored or replaced index is quarantined until a full native reconciliation
rotates it ([local topology](local-topology.md#index-identity)), so equal revision numbers from
different indexes never match.

Describe takes no revision and reports the current basis. Every other tool requires
`indexGeneration` and `expectedRevision` (integer > 0). Revision and evidence are read in one SQLite
read transaction. The process checks that its open database is still the file the workspace record
names at the start of the transaction and again before emitting the response. A pin equal to the
current revision is always answered. A pin to an older revision is answered only by
`baleyg_read_source`, and only when that path's content hash is unchanged since the pin
([revision compatibility](local-topology.md#revision-compatibility)); the answer comes from the
current revision, with `evidenceBasis` set to it and `compatibleWith` set to the pinned revision.
Every other stale pin, for every other tool and view, fails with `revision_conflict` carrying
`currentBasis`; the client re-queries. A conflict is not a request to reindex.

## Semantic freshness in results

Every evidence item that carries semantic information reports where it came from. Stage 1 fixes the
exact field schemas; these fields are required:

- `evidenceTier`: `syntax` or `semantic`.
- `semanticBasis`: producer and profile identity, artifact hash, the document content hash the fact
  was derived from, the digest of the build, configuration, dependency and source-set manifest its
  freshness depends on, and the revision at which it was imported. `null` for syntax-only items.
- `freshness`: `fresh`, `possiblyStale`, `stale` or `unavailable`.
- `staleBecause`: machine-readable reasons, such as `documentChanged`, `referencedDocumentChanged`,
  `exportSurfaceChanged`, `lookupSurfaceChanged`, `configChanged`, `targetRemoved`. Empty when fresh.
- `disposition` for bindings and references: `resolved`, `external`, `ambiguous`, `unresolved`,
  `unsupported`, `dynamic`, `declarationOnly` or `staleTarget`, never promoted by freshness. A binding
  whose target node no longer exists is `staleTarget` with no current target ID and its former semantic
  symbol as history; it is never returned as `resolved`.

Describe reports coverage and freshness per producer, language, source set and document summary, not
one global flag. A result that mixes fresh and possibly-stale facts says so per item.

## Tools

All inputs are JSON objects with `schemaVersion: 1`; unknown fields are rejected. Tool annotations
may declare read-only behaviour, but they are hints, not permissions.

| Tool | Additional input | Output data |
| --- | --- | --- |
| `baleyg_workspace_describe` | None | Workspace label (not an absolute path), current basis, index state (`ready`, `indexing` with progress, `no_published_index`, `store_unavailable`), per-language extraction tier, semantic coverage and freshness, tool and schema versions, limits |
| `baleyg_find_symbols` | `indexGeneration`, `expectedRevision`, `query` (literal name/ID substring, 1–256 UTF-8 bytes), optional `limit` (default 20, 1–50) | Symbol summaries: original ID, name, kind, relative path, recorded range, certainty, freshness fields; no source body |
| `baleyg_inspect` | `indexGeneration`, `expectedRevision`, `symbolId` (1–8192 bytes), `view`, plus view-specific bounds | `declaration`: declaration metadata. `outgoing_calls`: depth-one static calls with original call/target IDs and ranges, disposition and freshness fields, and bounded literal `calleeText`; max 50 calls. Other views: schemas ratified in Stage 1, with explicit depth/node/edge bounds, frontiers and truncation |
| `baleyg_read_source` | `indexGeneration`, `expectedRevision`, `path`, `startLine`, `endLine` | Cached text only, file content hash, exact returned line/byte range; max 200 lines and 16 KiB text |

Source paths are validated relative indexed paths: no absolute path, backslash, colon, NUL, empty,
`.` or `..` segment; max 4096 UTF-8 bytes. Lines are 1-based inclusive; end >= start and within the
cached file. Returned byte offsets are 0-based UTF-8 half-open. Read only cached rows: never open a
source path on disk or fall back to a live file.

An unresolved call site still records measured call syntax. Never promote a candidate or unresolved
target to a compiler-resolved or runtime binding. Outgoing calls are static evidence, not runtime
observations. Serialize an explicit projection, never native DTOs: `calleeText` is capped at 1024
UTF-8 bytes per call, clipped at a code-point boundary with per-call `calleeTextTruncated: true` and
envelope truncation. Control-region labels and callback bodies are excluded until a later schema
review admits them.

Symbol lookup uses literal `lower`/`instr` semantics, not regex or FTS. Ordering is deterministic
(exact name, name prefix, other substring; then name/ID). Calls are ordered by recorded
path/range/ID. Fetch one extra record to detect truncation; never report a clipped list as complete.
Clip source only at complete line and UTF-8 boundaries and report the actual range; a single line
over the text budget returns `range_too_large`.

Success envelope:

```json
{
  "schemaVersion": 1,
  "requestId": "server-assigned-id",
  "evidenceBasis": {"indexGeneration": "...", "indexRevision": 12},
  "compatibleWith": null,
  "data": {},
  "warnings": [],
  "truncated": false,
  "truncationReason": null
}
```

## Bounds

16 KiB tool request, 64 KiB complete response (including error and metadata envelopes), 16 KiB
source text, 5-second operation deadline, and a small per-process concurrency cap. Over-cap requested
limits are rejected, not silently expanded. There are no per-session request or byte budgets: the
authority model is the OS user and the whole checkout, and the launching client owns its usage policy.
Per-call limits do not bound aggregate resource use across many `baleyg mcp` processes run by the
same user.

## Errors

Execution failures return an MCP tool result with `isError: true` and
`{schemaVersion: 1, error: {code, message, retryable, currentBasis}, requestId}`. `currentBasis` is
`{indexGeneration, indexRevision}` when an index is published and `null` otherwise; it is required on
`revision_conflict`. Malformed MCP requests use
protocol errors. Never expose SQL, absolute paths outside the workspace, or source in errors.

| Code | Meaning / action |
| --- | --- |
| `invalid_request`, `range_too_large` | Missing pin, unknown fields, malformed path/range, exceeded input limits; fix the request |
| `revision_conflict` | Pin is from another generation, outside the retained window, or not honourable for this operation; carries `currentBasis`; re-query |
| `no_published_index` | No revision published yet (for example, launch-time indexing is running); retryable |
| `not_found` | Missing symbol or cached path at the current basis; never a live-read fallback |
| `body_too_large` | Request exceeds its byte limit |
| `too_many_requests` | Concurrency cap reached; retryable |
| `deadline_exceeded` | Bounded work cancelled; no partial evidence |
| `store_unavailable` | Index missing, unreadable, refused by safe-open rules, quarantined after a copy or restore, placement no longer usable, or of an unsupported schema; describe explains; no repair from a tool call |

## Cancellation

On MCP cancellation, interrupt the SQLite read where supported and suppress any late response.
A bounded read may finish internally; it must not emit after cancellation. Cancellation cannot
unsend data.

## Threat limits

- The boundary is the OS user. Any same-user process can launch `baleyg mcp` or read the index file
  directly. Tools provide bounded, typed, read-only access, not isolation from an agent that also has
  shell or filesystem access. A hostile-agent model needs a separate OS account or sandbox.
- Configuring the server for an agent approves disclosure of the whole indexed checkout, including
  symbol names, paths and call text, to that agent and its provider. There is no per-path scope.
- MCP cannot attest the downstream model provider or stop a client forwarding responses.
- Repository text is untrusted evidence, never an instruction or permission.
- Existing Jev/ACP allowances are untouched; tools cannot call or reset them.

## Acceptance tests

Required future tests, using fixtures and a synthetic MCP client; no model or provider calls, no
builds of inspected projects, no repository commands.

1. **Happy path:** describe -> find -> inspect -> read_source returns matching basis and hashes with
   uncertainty and freshness preserved, from a direct-terminal client and from a fixture representing
   an ACP-side bridge, with identical schemas.
2. **Workspace resolution:** launched in a nested directory, a worktree, and a non-Git root, the
   server serves exactly the enclosing workspace. Two worktrees with identical paths and symbol IDs
   never see each other's evidence. No tool argument can change the workspace.
3. **Lifecycle:** stdin EOF, `kill -9` of the client, and parent death each end the server with no
   surviving process. A client restart works without owner action. Stdout stays valid MCP.
4. **Launch-time indexing:** with no index, `server/discover`, any accepted legacy `initialize`, and
   `tools/list` answer within their deadlines while indexing runs; describe reports progress; evidence tools return
   `no_published_index`, then succeed after the first revision.
5. **Revision compatibility:** an older pin on `baleyg_read_source` is honoured only when that path
   is unchanged; an older pin on every other tool and view conflicts with `currentBasis`, including
   deleted and renamed results, `not_found`, displaced ranked results, and targets changed by another
   file's declaration. Evidence is never mixed across revisions.
6. **Identity:** a copied, restored or replaced index file is quarantined until a full native
   reconciliation rotates its generation; a semantic import cannot end quarantine; old pins conflict. A
   live `git clean -fdx` while servers run is detected at the next check and servers reopen through the
   workspace record, within the documented detection boundary. Moving the checkout keeps its UUID,
   durable state and generation; copying it produces a new workspace.
7. **Safe open and discovery:** a `.baleyg` that is a symlink, contains symlinks, is hard-linked, is
   owned by another user, has permissive modes, or is tracked by Git is refused (fallback at first
   placement, `store_unavailable` afterwards); no write follows a link. Launching in the home directory
   or filesystem root without `--workspace` is refused.
8. **Semantic freshness:** after a native edit, the changed document is syntax-only; referencing,
   importing and lookup-affected documents report `possiblyStale` with the right `staleBecause`;
   bindings to a removed target report `staleTarget`, never `resolved`; a configuration change stales
   the whole source set.
9. **Concurrency:** readers keep answering while other processes publish; killing the watcher leader
   lets another process take over and catch up; an explicit index request completes while a
   long-running agent session holds leadership.
10. **No side effects:** tool calls start no indexing, producer, build, download, provider call, or
    durable write, and never read the live working tree.
11. **Bounds, cancellation, deadlines:** malformed paths, unknown fields, oversized literals, ranges,
    lines and responses fail or report exact truncation; capped results are never reported complete;
    a cancelled or timed-out read emits nothing afterwards.
12. **Placement hygiene:** `.baleyg/` contains no token, ledger or durable view/annotation data, and is
    ignored by Git and ripgrep without changes to the repository's own ignore files.
