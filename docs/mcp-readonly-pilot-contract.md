# MCP read-only contract

Status: **PROPOSED DESIGN — not implemented**. This contract defines the agent-facing MCP surface
for the [local topology](local-topology.md). It supersedes the earlier grant-based pilot contract:
owner-issued grants, limited principals, budgets, enrollment latching, grant handoff files and
HTTP tool routes are removed. Stage 1 of [#8](https://github.com/jasoncarreira/baleyg/issues/8)
ratifies the final text, including the exact MCP specification revision it binds to.

## Boundary

- `baleyg mcp` is a stdio MCP server launched by an agent client. It serves exactly one workspace,
  resolved from its working directory, for its whole lifetime. It opens no network listener.
- It reads committed snapshots of `<root>/.baleyg/index.db`. It may also become the index's writer
  for native refresh (see [local topology](local-topology.md#single-writer)); that role never
  changes what tools can do.
- Tools read only cached source and indexed evidence. No tool indexes on request, runs a semantic
  producer, builds, downloads, writes durable data, calls a provider, executes repository code,
  opens a terminal, reads the live working tree, or selects another workspace.
- The first catalog has exactly four tools: `baleyg_workspace_describe`, `baleyg_find_symbols`,
  `baleyg_inspect`, `baleyg_read_source`. Hierarchy, usages, type-hierarchy and coverage views are
  added to `baleyg_inspect` as later stages ratify their schemas.

Direct terminal agents, agents in Herdr panes, and agents reached through ACP all launch or are
configured with the same server and see the same catalog. Illustrative client configuration:

```json
{ "mcpServers": { "baleyg": { "command": "baleyg", "args": ["mcp"] } } }
```

A remote agent cannot reach a local stdio server. That needs an explicitly secured bridge, such as
the proposed Mimir provider extension, and is outside this contract.

## Process lifecycle

- Exit on stdin end-of-file. Exit when the parent process dies (Linux `PR_SET_PDEATHSIG`, macOS kqueue
  `NOTE_EXIT`). Never daemonize, never fork a long-lived child, never outlive the client.
- Stdout carries only MCP messages. Diagnostics go to stderr, with no source text or paths outside
  the workspace.
- Startup with no index: if the process becomes the writer, it runs an initial native index and
  reports `indexing` from describe until the first revision is published. Otherwise describe reports
  `no_published_index` and evidence tools fail with that code.
- A client restart simply starts a new process. There is no credential to consume or reissue.

## Evidence basis and revision pins

`evidenceBasis` is `{indexGeneration, indexRevision}`. `indexGeneration` is created with the index
and changes whenever the index is rebuilt from nothing, so equal revision numbers from different
indexes never match.

Describe takes no revision and reports the current basis. Every other tool requires
`expectedRevision` (integer > 0) and `indexGeneration`. Revision and evidence are read in one SQLite
read transaction. Pin validation follows the revision churn rule in
[local topology](local-topology.md#revision-churn):

- Same generation, pinned revision current: answer normally.
- Same generation, pinned revision older but within the retained change log, and no path the answer
  read changed since: answer from the current revision, with `evidenceBasis` set to the current
  revision and `compatibleWith` set to the pinned revision.
- Otherwise: `revision_conflict` carrying the current basis. The client re-queries. A conflict is not
  a request to reindex.

Semantic facts carry their own basis label. Facts reused across an edit are reported as possibly
stale with the revision and inputs they were derived from; they are never reported as fresh.

## Tools

All inputs are JSON objects with `schemaVersion: 1`; unknown fields are rejected. Tool annotations
may declare read-only behaviour, but they are hints, not permissions.

| Tool | Additional input | Output data |
| --- | --- | --- |
| `baleyg_workspace_describe` | None | Workspace label (not an absolute path), current basis, index state (`ready`, `indexing`, `no_published_index`), per-language extraction tier and semantic coverage, tool and schema versions, limits |
| `baleyg_find_symbols` | `indexGeneration`, `expectedRevision`, `query` (literal name/ID substring, 1–256 UTF-8 bytes), optional `limit` (default 20, 1–50) | Symbol summaries: original ID, name, kind, relative path, recorded range, certainty; no source body |
| `baleyg_inspect` | `indexGeneration`, `expectedRevision`, `symbolId` (1–8192 bytes), `view` (`declaration` or `outgoing_calls`) | Declaration metadata, or depth-one static calls with original call/target IDs and ranges, resolution labels and bounded literal `calleeText`; max 50 calls, no recursive expansion |
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
limits are rejected, not silently expanded. There are no lifetime request or byte budgets: the client
that launched the process owns its own usage policy.

## Errors

Execution failures return an MCP tool result with `isError: true` and
`{schemaVersion: 1, error: {code, message, retryable}, requestId}`. Malformed MCP requests use
protocol errors. Never expose SQL, absolute paths outside the workspace, or source in errors.

| Code | Meaning / action |
| --- | --- |
| `invalid_request`, `range_too_large` | Missing pin, unknown fields, malformed path/range, exceeded input limits; fix the request |
| `revision_conflict` | Pin is from another generation, too old, or its inputs changed; carries the current basis; re-query |
| `no_published_index` | No revision published yet; retry after indexing completes |
| `not_found` | Missing symbol or cached path at the current basis; never a live-read fallback |
| `body_too_large` | Request exceeds its byte limit |
| `too_many_requests` | Concurrency cap reached; retryable |
| `deadline_exceeded` | Bounded work cancelled; no partial evidence |
| `store_unavailable` | Index missing, unreadable or unsupported schema; describe explains; no repair from a tool call |

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
   uncertainty preserved, from a direct-terminal client and from a fixture representing an ACP-side
   bridge, with identical schemas.
2. **Workspace resolution:** launched in a nested directory, a worktree, and a non-Git root, the
   server serves exactly the enclosing workspace. Two worktrees with identical paths and symbol IDs
   never see each other's evidence. No tool argument can change the workspace.
3. **Lifecycle:** stdin EOF, `kill -9` of the client, and parent death each end the server with no
   surviving process. A client restart works without any owner action. Stdout stays valid MCP.
4. **Revision churn:** a pin whose inputs are unchanged is answered with `compatibleWith`; a pin whose
   inputs changed, a pin outside the retained window, and a pin from a rebuilt index each conflict.
   Evidence is never mixed across revisions.
5. **Writer interaction:** readers keep answering while the writer publishes deltas; killing the
   writer lets another process take over and catch up; no two processes publish concurrently.
6. **No side effects:** tool calls start no producer, build, download, provider call, or durable write,
   and never read the live working tree. Index, provider and artifact counters stay zero for tools.
7. **Bounds:** malformed paths, unknown fields, oversized literals/ranges/lines and responses fail or
   report exact truncation. Capped results are never reported complete.
8. **Cancellation and deadlines:** a cancelled or timed-out read emits nothing afterwards and starts no
   other work.
9. **Placement hygiene:** `.baleyg/` contains no token, ledger or durable view/annotation data, and is
   ignored by Git and ripgrep without changes to the repository's own ignore files.
