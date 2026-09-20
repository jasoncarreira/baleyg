# MCP read-only pilot contract

Status: **PROPOSED DESIGN — not implemented**. This contract narrows Phase 1 of the
[agent integration plan](agent-integration-plan.md). Endpoints, commands, fields, grants and
acceptance tests below are proposals, not claims about the running daemon. This document does
not authorize implementation, indexing, process launches, provider use or deployment.
The server-side linearization model is specified separately in
[MCP admission and response handoff](mcp-admission-linearization.md).

## Boundary and current facts

Use one existing local daemon, its canonical workspace, and its already published cached index.
Add exactly four tools: `baleyg_workspace_describe`, `baleyg_find_symbols`, `baleyg_inspect`,
`baleyg_read_source`. No registry or project picker is required. No text search, diagrams,
artifacts, indexing, working-tree reads, dependencies/external source libraries, arbitrary SQL,
shell, remote MCP listener or provider calls belong to this pilot. A separately tested next slice
may add a bounded literal scan of cached text; it does not assume FTS.

Observed implementation basis:

- `src/main.rs`: CLI has index/serve/status/symbols/query/export, not MCP or grant commands.
  Serve binds loopback. Workspace resolution canonicalizes the root; default state lives in an
  application-data directory keyed by the root hash. A custom state/token path is possible.
- `src/auth.rs`: one 32-byte random owner bearer encoded as 64 lowercase hex characters.
  Unix token files use exclusive creation, mode 0600 and no-follow; existing files must be regular,
  owner-owned, mode 0600 and single-linked. Non-Unix secure token storage is unsupported.
- `src/http.rs`: one `DaemonState` owns one `Store`. The guard checks Host/Origin and compares the
  bearer for all `/api` routes. That bearer can also index, write saved views/annotations and reach
  configured inference routes. There is no scoped agent principal or grant issuer today.
- `src/store.rs`: canonical workspace binding in `workspace.db`; cached evidence in `cache.db`.
  A revision allocator in `workspace.db` protects normal publication against numeric revision reuse
  after cache loss. Symbol/source single reads accept optional revision guards; symbol search returns
  the revision from its read transaction. HTTP graph query has no mandatory expected-revision guard.
  `cache()` opens a new SQLite connection by path for each read; `connect()` also performs writable
  setup/migration. There is no generation detector or enrolled read-only MCP connection today.

Existing owner API behavior need not change for the browser. **Giving an adapter the owner bearer
and hiding tools is not this design.** New server authorization and guarded store reads are required.

## Two peer entry paths, one catalog

1. An MCP-capable coding agent runs directly in a terminal, optionally inside Herdr.
2. An agent is reached through ACP, mostly Mimir, with its existing client/session UI.

Both use the same four MCP names, schemas, errors and evidence semantics. ACP is optional, not a
second catalog or a new Baleyg harness loop. Baleyg may launch an owned adapter/session connection
only at the user's request. It must not force a second Mimir ACP client alongside an editor.
Herdr is not needed for connection, identity or grants.

Embedded terminal windows/tabs inside Baleyg are an **accepted separate UI target**, not deferred
away and not part of this read-only slice. Use real PTYs for explicitly requested Baleyg-launched
direct agents. Herdr-owned terminals require a verified supported attach/stream interface, not
metadata, `pane.read`, polling or ownership takeover. ACP gets conversation/tool/approval tabs;
it is not a terminal stream, and a terminal requires a separately supplied actual PTY. Both paths
keep this same MCP catalog. Terminal launch/input requires separate authority, never this grant.

The reviewed Mimir ACP provider currently admits exactly one `mimir-hands` provider and five exact
tools (read, edit, shell, Python, scope request). A Baleyg provider is **not accepted today**. Its
later versioned server-owned profile must extend the existing local credential-aware proxy without
weakening Hands v1. Mimir's generic MCP client runs on the daemon host, not necessarily the local
workspace host. The pilot tests a synthetic local MCP client, not this future Mimir integration.

## Direct binding, not a project catalog

An owner-approved local descriptor selects exactly one loopback daemon. Proposed descriptor:

```json
{
  "schemaVersion": 1,
  "baseUrl": "http://127.0.0.1:8877",
  "daemonInstanceId": "opaque-startup-uuid",
  "storeGeneration": "opaque-generation-uuid",
  "grantFile": "/private/operator/baleyg/pilot/grant.json"
}
```

The descriptor is not authority. The server binds its identifiers to the canonical workspace root
**and canonical state directory**; neither can be supplied or changed by a tool request. The owner
confirms these resolved paths during issuance. Do not infer binding from cwd, Git, Herdr pane IDs,
matching paths on another host or an adapter-supplied label. No project enumeration endpoint exists.

Proposed identity rules:

- `daemonInstanceId` is random at every daemon start.
- `storeGeneration` is an in-memory epoch, random at startup. It is **not a detector** of file
  changes. The identity guard below invalidates it on observed identity loss/change or before any
  supported destructive maintenance. No persistent global store UUID is needed for this narrowed
  Unix pilot; restart already invalidates every grant.
- Normal atomic, in-place publication advances `indexRevision` without changing generation.
  An owner grant pins one revision; reindexing requires owner reissuance for more evidence reads.
- An invalidation revokes all grants, rejects results that have not reached application response
  commit, rotates generation once and **latches MCP unavailable for the remaining daemon lifetime**.
  A response committed earlier may still be delivered from transport buffers. No automatic enrollment of the
  replacement cache. Recovery requires daemon restart, a valid published store and fresh owner
  approval. Existing owner APIs must not clear this latch by minting another grant.
- Matching revision numbers alone never make an old binding valid. A restart invalidates all old
  grants, descriptors and adapter caches. There are no pilot cursors.

### Concrete Unix cache-identity guard

Current `Store::cache()` opens a fresh `Connection` by pathname for each read, and its `connect()`
helper performs writable setup/migration. In particular, `secure_database_file()` uses
`.create(true)`: a deleted cache can become a new empty database on an ordinary `Store::status()`
call. That would turn identity loss into revision 0 and hide the failure. Those helpers are **not**
a safe implementation of this MCP binding, including owner binding discovery and grant issuance. Add a dedicated read-only connection service; this is required new code, not a claim
about today's daemon.

Enroll once at daemon startup after ordinary store initialization and before granting tool access. The Store publication boundary permanently accepts only one enrollment/lifecycle. A separate owner-only stable enrollment-lock inode carries an exclusive nonblocking kernel lease for the enrolled core lifetime, so separately opened Stores and processes also reject concurrent enrollment. A fresh Store may acquire that lease only after the prior enrolled core is fully dropped, as daemon-restart semantics. Boundary, raw connection, generic snapshot/admission/preparation, lifecycle callback, and finalization APIs are crate-private. PR2 exposes only bounded typed operations and its outer Tower handoff:

1. Retain a no-follow directory FD for the canonical state directory and no-follow regular-file FDs
   for `cache.db` and `workspace.db`. Record their Unix device/inode identities and validate file
   types. Retaining FDs pins inode lifetime; it does **not** make SQLite use those descriptors.
2. Open one persistent read-only SQLite connection to the bound cache through a qualified Unix VFS,
   with normal supported locking/WAL behavior, no create/migrate fallback and no symlink following.
   Serialize pilot DB reads on this connection; the two-request admission cap may include a queued
   request, whose deadline still runs. A bounded pool is optional, but every member needs enrollment.
3. Check no-follow path identities against the retained anchors before and after connection setup.
   On the **actual SQLite connection**, require `sqlite3_file_control(..., "main",
   SQLITE_FCNTL_HAS_MOVED, ...)` to return `SQLITE_OK` and `moved == 0`. This checks the VFS's opened
   main database against its pathname; it is not an API returning its OS FD. Qualify/test that VFS
   and this combined setup check. Unsupported control (`SQLITE_NOTFOUND`), errors or indeterminate
   identity fail closed. Do not substitute checks only on a separately opened FD, `/dev/fd` aliases,
   or `immutable=1` for proof about a live WAL-backed connection.
4. Before and after each read, and during crate-private `prepare_handoff`, check the retained
   directory/database path identities and the actual enrolled connection's moved status again. Run
   revision and evidence reads in one transaction on that connection. `prepare_handoff` performs the
   final SQLite revision/`SQLITE_FCNTL_HAS_MOVED` proof while retaining publication authority. Never
   call the existing reopen-by-path helper, lazily replace a connection or rebind after an error.
5. At application commit, repeat the cheap path/lock/cache/workspace identity, lifecycle latch, grant,
   revision association and absolute-deadline checks; then serialize typed finalization with
   publication and internal invalidation. No SQLite or generic callback is public at this point. Every
   supported destructive cache rebuild/restore or root/state rebind must invalidate **before**
   mutation; normal SQL publication remains revision-guarded. On observed loss/change or a failed
   identity check, apply the unavailable latch above. Define **application response commit** as the
   synchronous outer service transition that returns the immutable response to the HTTP stack. No
   evidence response or grant may commit after invalidation has linearized. A response committed
   earlier may be delivered later; HTTP/TCP buffering and peer receipt are outside this model. Do not
   use mtime/size as an epoch: ordinary writes/checkpoints change them.

**Read-only WAL qualification:** read-only main-database access still uses SQLite's WAL/shared-memory
coordination. Depending on existing sidecars and VFS behavior, a cold open may need permission to
create/initialize `-shm` in the private state directory. Qualify both a warmed store and a cold store
without `-shm`, including permission failures. SQLite-managed shared-memory coordination is not
permission to create a missing main database, migrate schema, or switch to a writable main connection.
Do not assume a read-only main connection means zero auxiliary filesystem writes, nor that every WAL
read always requires writable sidecars. Fail closed if the supported WAL setup cannot be established;
never use `immutable=1` to bypass it.

**Detection boundary:** these are sampled identity checks, not filesystem tamper attestation.
Persistent replacements/deletions observed at these boundaries are rejected, even if the new DB
contains the same numeric revision. An unobserved swap-away-and-back (ABA), an unmanaged same-inode
restore/content edit, or destructive external WAL/SHM manipulation is **not reliably detected**.
No content-fingerprint check is promised by this pilot. Such live storage operations are unsupported;
stop the daemon before external restore/replacement and restart/reissue afterward. Internal locking
cannot serialize arbitrary external filesystem writers, malicious or accidental. The acceptance
suite must test the actual checks, not claim detection of every possible storage mutation.

**Browser scope / open product decision:** the latch protects the MCP pilot, including its owner
binding/issuance routes. Existing browser/owner store reads still use their current reopen-by-path
behavior and may continue against a replacement or recreate a deleted cache. Do not describe this
as a daemon-wide store-integrity guard. Decide separately whether the browser should display a
"store identity changed, restart required" warning, or share the fail-closed behavior. Neither UI
notification nor owner-route blocking is implemented or a settled addition to this pilot. Browser
activity must never clear the MCP latch, even if it recreates or republishes the database.

`evidenceBasis` is `{daemonInstanceId, storeGeneration, indexRevision}`. The pilot uses cached
per-file hashes with exact ranges for citations. A future registry can map stable project IDs to
these bindings without becoming a Phase 1 prerequisite.

## Owner-only grant bootstrap

### Proposed HTTP control and data routes

Keep the existing loopback/Host/Origin checks. Reject duplicate/malformed authorization headers.
Authenticate into **distinct owner and limited-grant principal types**, with default-deny dispatch.
A grant token must never fall through to existing owner handlers.

| Method and route | Required credential | Result |
| --- | --- | --- |
| `GET /api/mcp-pilot/binding` | Owner bearer only | Canonical workspace/state paths, binding, current revision; no project list |
| `POST /api/mcp-pilot/grants` | Owner bearer only | Create a pinned, short-lived grant; token returned once to trusted owner helper |
| `DELETE /api/mcp-pilot/grants/{grantId}` | Owner bearer only | Idempotent revoke, 204 even if already revoked/unknown |
| `POST /api/mcp-pilot/tools/baleyg_workspace_describe` | Limited grant | Authorized binding metadata only |
| `POST /api/mcp-pilot/tools/baleyg_find_symbols` | Limited grant with matching capability | Guarded symbol lookup |
| `POST /api/mcp-pilot/tools/baleyg_inspect` | Limited grant with matching capability and `sourceApproved: true` | Guarded declaration/outgoing calls, including bounded source fragments |
| `POST /api/mcp-pilot/tools/baleyg_read_source` | Limited grant with matching capability and `sourceApproved: true` | Guarded cached range |

Data routes accept only limited grants, making accidental owner-token use by an adapter fail closed.
All other routes/methods reject a limited principal, including existing source/query/status routes,
index/jobs, views/annotations, question/provider routes and grant management. This is a server rule,
not only the MCP `tools/list` catalog. The adapter has no generic HTTP proxy tool.

All schemas are version 1, JSON objects with unknown fields rejected. Grant creation input:

```json
{
  "schemaVersion": 1,
  "binding": {"daemonInstanceId": "...", "storeGeneration": "..."},
  "expectedRevision": 12,
  "capabilities": ["baleyg_workspace_describe", "baleyg_find_symbols", "baleyg_inspect", "baleyg_read_source"],
  "ttlSeconds": 900,
  "limits": {"maxRequests": 200, "maxTotalResponseBytes": 2097152, "maxResponseBytes": 65536},
  "clientLabel": "terminal-pilot",
  "disclosure": {"recipient": "approved local client", "sourceApproved": true}
}
```

Issuance validates a nonzero published revision inside the bound store. This **already indexed pilot**
is deliberately narrower than the future ability to describe unindexed workspaces or author purely
conceptual drafts before indexing. Here, describe needs a grant issued against a published index;
owner-only binding discovery handles unindexed bootstrap. Describe has no request revision guard,
but that does not waive the pilot's issuance prerequisite. The owner may choose a
subset of the four capabilities; describe is required. **Both `baleyg_inspect` (all views) and
`baleyg_read_source` require `sourceApproved: true`**, enforced at issuance and independently on
backend calls. Reject issuance with 403 `forbidden` if either capability is requested without that
approval; do not silently drop the capability or issue a weaker-looking grant. There is no per-call
approval override. With `sourceApproved: false`, only describe/find may be granted. This is not
zero disclosure: names, IDs, paths and other structural metadata are source-derived information
that the owner still approves. The additional flag authorizes source expressions/snippets/bodies.

Native `CallSite.callee_text` and `ControlRegion.label` are literal source substrings, not harmless
structural metadata. Inspect therefore needs source approval even without a read_source capability.
Use the explicit projection below, never a passthrough serialization of native DTOs.
Approval covers the **whole indexed workspace**, not selective paths. No subdirectory scope is
promised in this pilot; projects requiring path-level exclusions cannot use a whole-workspace grant.
The owner is warned that declarations and call metadata can also disclose sensitive code facts.
Recipient/client labels are audit declarations, not cryptographic attestations of the downstream model.

Defaults above are also server ceilings; the owner may reduce them. TTL is 1–900 seconds, with a
monotonic in-memory expiry deadline. There is no renewal, refresh token, delegated minting or
agent-callable permission request. A new grant requires the authenticated owner. Server ceilings
also bound concurrency and each query independently (below).

Response 201 is `{schemaVersion, grantId, token, expiresAt, binding, admittedRevision,
capabilities, effectiveLimits}`. Token is a new opaque 256-bit random secret (distinct `bgp_` token
format); it carries no editable claims. Keep only its cryptographic digest and grant policy in an
in-memory daemon table. Do not persist grants across restarts. `grantId` is non-secret and cannot
be exchanged for the token. No token retrieval endpoint exists. A grant does not become active until
its complete 201 response reaches application response commit. If the connection fails after that
commit but before client receipt, an ambiguous issuance retry may leave one unused grant until expiry;
never recover it by printing secrets.

### Trusted handoff, separate from the agent

A proposed owner-operated helper (for example `baleyg mcp-grant issue`, **not an existing command**)
reads the current owner token privately, fetches the binding, asks the user to confirm resolved paths,
recipient/source disclosure and limits, then calls issuance. It must run outside the agent-controlled
session. It writes the descriptor and short-lived grant file, or passes the grant to a child adapter.
It never hands the owner bearer, its path or its open FD to the adapter. The helper exits after
handoff or remains only as an explicitly owned lifecycle supervisor, not an agent loop.

Default interoperable handoff for ordinary MCP launchers:

- Use an operator-controlled 0700 directory **outside every agent-readable/workspace root**.
  Validate operator/system-trusted parent ownership/modes and symlink-free resolution. Fail if a configured owner/grant
  path is inside a permitted agent tree; the existing CLI's custom paths are not proof of safety.
- Create the grant file exclusively, no-follow, 0600, current owner, regular and single-linked.
  JSON contains `{schemaVersion, grantId, token, expiresAt, binding}` only. Validate again via
  the opened FD, with a strict small size cap. Never follow a replaced path or trust repository config.
- Only the descriptor path, not token bytes, appears in MCP command args. No token in environment,
  URLs, stdout, stderr, config JSON shared with the model, diagnostics, crash dumps or request logs.
  The adapter loads the grant once and unlinks its grant file after validation. Startup retries
  after consumption require owner reissue. This is intentionally incompatible with transparent
  credential reuse on automatic stdio-server restart. Clean up abandoned files on expiry via the
  owner helper; apply the restart behavior below rather than recreating a consumed file.
- Allow only an exact loopback base URL; no redirects, proxy-environment routing, endpoint discovery
  from repository files or fallback to another daemon. Verify the server binding in describe before
  evidence use. A binding mismatch stops the adapter.

An inherited FD is a stronger lifecycle alternative when a trusted launcher controls process spawn:
pass an anonymous pipe or already-open/unlinked grant file on a designated FD, not stdin/stdout
(which carry MCP). Read once, close it, and never pass it to grandchildren. The launcher must arrange
inheritance/close-on-exec deliberately. Generic MCP command/args configurations do not guarantee
arbitrary FD forwarding; require a native wrapper/launcher and test it per client. FD number in argv
is not the secret. File and FD variants are alternative native handoffs, not imaginary MCP features.
The file route is the pilot baseline on Unix; non-Unix secure storage needs a separate design.

Only the owner-authenticated helper/UI revokes grants. The adapter can close and erase its copy,
but cannot issue, renew or revoke via a privileged endpoint. A trusted owner supervisor requests
revocation on session disconnect/cancellation/replacement. If absent or crashed, the fixed TTL and
budgets bound exposure; do not promise instantaneous revocation on every EOF.

## Minimal tool schemas

MCP tool annotations may declare read-only/non-destructive behavior but are hints, not permissions.
`tools/list` contains only granted tools from the four-name catalog. No dotted/slashed wire names.
Tools contain no owner token, grant token, local state path, recipient override or runtime-control field.
The adapter supplies bearer authentication separately to the four allowlisted HTTP paths.

Every call has `schemaVersion: 1` and `binding: {daemonInstanceId, storeGeneration}`. The adapter
checks these against its descriptor; the daemon independently checks them against the grant. All
three evidence reads require integer `expectedRevision > 0`; missing/null values are errors.
The server checks equality with both `admittedRevision` and the transactional current revision.
Describe is the only exception and accepts no `expectedRevision`.

| Tool | Additional input | Output data |
| --- | --- | --- |
| `baleyg_workspace_describe` | None | Non-secret workspace label, binding, current/admitted revision, `evidenceReadable`, tool/schema versions, effective limits, expiry; no absolute owner paths, file list or source |
| `baleyg_find_symbols` | `expectedRevision`, `query` (literal name/ID substring, 1–256 UTF-8 bytes), optional `limit` (default 20, 1–50) | Symbol summaries: original ID, name, kind, relative path, recorded range, certainty; no source body |
| `baleyg_inspect` | `expectedRevision`, `symbolId` (1–8192 bytes), `view` (`declaration` or `outgoing_calls`) | Source-approved only, for either view. Declaration metadata, or depth-one static calls with original call/target IDs/ranges, resolution labels and bounded literal `calleeText`; max 50 calls, no recursive expansion |
| `baleyg_read_source` | `expectedRevision`, `path`, `startLine`, `endLine` | Cached text only, file content hash, exact returned line/byte range; maximum 200 lines and 16 KiB text |

Source paths are validated relative indexed paths: no absolute path, backslash, colon, NUL, empty,
`.` or `..` segment; max 4096 UTF-8 bytes. Lines are 1-based inclusive integers, end >= start and
within the cached file. Byte offsets returned are 0-based UTF-8 half-open offsets. Read only cached
rows: never resolve/open a source path on disk, browse libraries or fall back to a live file.
Hash identifies the cached file; keep stored declaration ranges in their documented native units.

An unresolved callsite still records measured call syntax. Do not promote a candidate or unresolved
target to a compiler-resolved or runtime binding. Outgoing calls are existing static evidence, not
runtime observations or incoming-call hierarchy.
Filter out raw source bodies/extra native DTO fields not in this contract before serialization.
For inspect's outgoing calls, `calleeText` is capped at 1024 UTF-8 bytes per call, clipped only at a
code-point boundary with per-call `calleeTextTruncated: true` and envelope `truncated: true` plus
`truncationReason: "source_fragment_limit"`. Preserve original call IDs/ranges. The 64 KiB complete
response cap still wins. Control-region objects/labels, callback bodies and arbitrary native fields
are excluded from this pilot projection; adding them later needs an explicit schema/bounds review.
A declaration-only request still requires source approval; selecting another view cannot weaken
its capability policy.

Success envelope:

```json
{
  "schemaVersion": 1,
  "requestId": "server-assigned-id",
  "evidenceBasis": {"daemonInstanceId": "...", "storeGeneration": "...", "indexRevision": 12},
  "data": {},
  "warnings": [],
  "truncated": false,
  "truncationReason": null
}
```

Describe reports the current basis even if the grant's admitted revision is now stale, sets
`evidenceReadable: false`, and exposes no evidence. Generation mismatch still fails authentication
binding checks. After observed cache loss, MCP remains unavailable until daemon restart, a valid
published index and fresh owner issuance; publication alone cannot clear the lifetime latch.
A valid revision-0 store without identity loss is a separate unindexed bootstrap case, handled by
owner binding discovery and `no_published_index`, not by an agent tool.

Symbol lookup uses the existing SQLite `lower`/`instr` literal semantics, not regex or FTS.
Symbol ordering is deterministic (exact name, name prefix, other substring; then name/ID).
Calls are ordered by recorded path/range/ID. Fetch one extra record to detect row truncation.
No pagination/cursors or result-reference service in this slice. A truncated lookup instructs the
client to narrow its query; a source read can explicitly request the next range at the same basis.
Never report a clipped list as complete. Clip source only at a complete line/UTF-8 boundary and
report the actual range; a single line exceeding the text budget returns `range_too_large`.

Backend hard ceilings: 16 KiB tool request, 64 KiB complete response, 2 concurrent reads per grant,
5-second operation deadline through application response commit, and the grant's shared 200-request/2 MiB response lifetime budget. Socket drain and peer receipt are outside that deadline.
Caps include error/metadata envelopes; source content also has its separate 16 KiB ceiling.
Reserve request/output budgets before work and settle actual bytes atomically; concurrent requests
cannot exceed them. A depleted budget never produces an empty successful result. Retain a small
fixed error allowance for budget-denied responses. Reject over-cap requested limits, do not silently
expand them; native lower limits still win. Cancellation does not refund the admitted request count. The operation deadline is also an absolute publication-authority lease deadline: queued or canceled preparation, a running sample, retained carrier clones, and a lifecycle-ordered failure cannot extend it. Expiry releases only the small lease and never drops arbitrary response/evidence state; a late handoff cannot emit it.

Read revision and evidence in one SQLite read transaction. Separate status -> data -> status checks
are not sufficient to claim atomic evidence. If native helpers are combined, they need a shared
snapshot/guarded service, not independent unguarded calls. `prepare_handoff` repeats SQLite revision
and `SQLITE_FCNTL_HAS_MOVED` while retaining the publication lease. Application commit then repeats
only cheap identity, lifecycle latch, deadline, grant expiry/revocation and admitted-revision
association checks before its infallible typed finalizer. A publication linearized before that commit
causes conflict; a publication linearized afterward does not relabel or recall a response that
truthfully carries the older `evidenceBasis`. Serialize this final decision with invalidation. A
response already committed to the HTTP stack cannot be recalled.

## Errors and lifecycle

Proposed HTTP error envelope for owner control and tool routes:
`{schemaVersion:1, error:{code, message, retryable}, requestId}`.
MCP returns `isError: true` with that structured error for execution/auth failures; malformed MCP
requests use protocol errors. Never expose SQL, token fragments, absolute state paths or source in errors.

| HTTP | Code | Meaning/action |
| --- | --- | --- |
| 400 | `invalid_request`, `range_too_large` | Missing revision, unknown fields, malformed path/range or exceeded input limits; fix request |
| 401 | `unauthorized` | Missing/invalid/expired/revoked grant; owner must reissue; do not distinguish secret validity |
| 403 | `forbidden` | Wrong principal/route/capability/disclosure permission; no owner fallback |
| 409 | `binding_mismatch`, `revision_conflict` | Wrong incarnation/generation or stale evidence; drop cache, owner rebind/reissue as needed |
| 409 | `no_published_index` | Owner `POST /api/mcp-pilot/grants` against a valid bound store with revision 0; `retryable: false`, no grant/token created; owner must explicitly publish an index before trying again |
| 404 | `not_found` | Missing symbol/cached path at the authorized basis; never live-read fallback |
| 413 | `body_too_large` | Tool request exceeds byte limit |
| 429 | `budget_exhausted`, `too_many_requests` | Lifetime budget exhausted (new owner approval) or temporary concurrency cap |
| 504 | `deadline_exceeded` | Cancel bounded work, no partial evidence response |
| 503 | `store_unavailable` | Identity failure or latched invalidation; latched cases have `retryable: false`, no in-process recovery even after paths are repaired; restart and owner reissue required, never automatic index/repair |

For issuance, authenticate the owner and validate the request and store binding first.
**`POST /api/mcp-pilot/grants` must obtain the published revision from a transaction on the enrolled
read-only connection, with the identity guard applied before/after and at grant admission. Never
call `Store::status()`, `Store::cache()` or `connect()` on this path.** Apply the same rule to
`GET /api/mcp-pilot/binding`, describe and final revision checks; these are not exceptions merely
because they return metadata or use owner authentication. In-memory revocation must remain possible
while storage is unavailable and does not need to open a database.
For an otherwise valid request with positive `expectedRevision`, check for no published index before
comparing that expected revision: return `no_published_index`, not `revision_conflict`. A missing,
replaced or unreadable bound database is `store_unavailable`, not an empty valid store. Do not create
a cache, index, repair files or reserve a grant as a side effect of issuance. Owner-only binding
discovery may report revision 0 so the helper can explain the prerequisite before issuance.

Invalidated generations may return 401 first if the grant was removed; callers must treat both
401 and binding conflict as terminal for that connection. Errors do not enumerate other bindings.
Retry only explicitly retryable transient errors within the original grant/deadline; no background
reauthorization or provider activity. Revision conflict is not a request to reindex.

On MCP cancellation, abort/interrupt the read where supported and reject its response if application
response commit has not happened. A bounded blocking DB read may finish internally, but its result is
discarded. HTTP disconnect cancels response delivery where the transport observes it. Cancellation
cannot recall a response already committed or roll back a completed disclosure.
On stdio EOF, close HTTP work and erase credentials/cached evidence; owner supervisor revokes if
available. On daemon restart, adapter restart, auth failure, logout or session replacement, clear
caches and require owner handoff instead of silently reacquiring an owner token.

### Consumed handoff and client auto-restart

A crash after consuming the grant file leaves no restart credential. On a missing/consumed handoff,
the adapter must not contact the daemon, search for another credential, prompt the model to repair
access, or retry internally. Emit one sanitized stderr diagnostic `owner_reissue_required` and exit
nonzero; this is a local startup condition, not a daemon HTTP response. Keep stdout MCP-only. FD
handoffs have the same one-shot restart limitation unless the trusted owner explicitly issues anew.

MCP clients differ in restart policy. Configure retries disabled or capped and an actionable owner
notice in each supported client/launcher. The adapter cannot force an arbitrary third-party client
to stop respawning it. If the client has an uncontrollable restart loop, the operator must disable
that server entry until a new owner handoff is ready; do not advertise unattended recovery for it.
Reissue is an out-of-band owner action, not a permission that the agent can obtain by retrying.

No shared evidence cache is needed for the pilot. If added, key it by grant ID, binding, revision,
tool and complete normalized arguments; recheck authorization before every hit. Never share cached
responses between principals or revive old grants. The daemon logs only grant/request IDs, tool,
counts/bytes, timing and error category with bounded retention; not queries, source, headers or secrets.

## Threat limits

The backend protects against a limited-token holder invoking unapproved daemon capabilities, reading
another store, exceeding budgets or reusing stale evidence. MCP allowlists, read-only annotations,
client labels and claimed session IDs do not enforce those boundaries by themselves.

The grant remains a **bearer**: theft permits replay within its exact scope until expiry/revocation.
No proof-of-possession or authenticated downstream-provider identity is claimed. Bind to one instance,
keep TTL short, never redirect, redact logs and revoke on exposure. Loopback plus current Host/Origin
checks is not encrypted mutual authentication against malicious local processes. An attacker who
can replace the loopback service or inspect the trusted helper's memory is outside this pilot's
boundary; a stronger transport/process-isolation design is separate work.

Unix 0700/0600 and hidden paths do not isolate processes running as the same OS user. An agent with
unrestricted shell/Python/Hands access may read the owner token/cache directly, inspect processes or
modify state. Moving secrets outside agent-readable roots helps only when the runtime actually
enforces those roots. Use a distinct OS account/sandbox for a hostile-agent threat model. The grant
is least privilege for the tool channel, not a claim that it reduces the runtime's broader authority.

Owner approval authorizes disclosure to a chosen client; MCP cannot stop an arbitrary client from
forwarding responses. Mimir-specific destination/taint policy remains enforced by Mimir after its
profile extension. Repository text is untrusted evidence, never a grant or an instruction. Existing
Jev/ACP allowances are untouched; pilot reads cannot call or reset them.

## Acceptance tests

These are **required future tests**, not executed checks. Use fixtures and a synthetic MCP client;
no model/provider calls, builds of inspected projects, repository commands or automatic indexing.
A prepared disposable store may be supplied by the test harness.

1. **Happy path:** owner approves one existing binding, privately hands off a 900-second grant,
   then describe -> find -> inspect -> cached source returns matching basis/hashes and uncertainty.
   Source edits on disk do not change cached reads. No Herdr, Mimir or registry is installed.
2. **Peer catalog:** a direct terminal client and a fixture representing the ACP-side bridge see
   identical granted MCP schemas. This does not claim current Mimir admits Baleyg. No second ACP
   connection, harness loop, agent process or provider is started by tool discovery.
3. **Bootstrap authority:** unauthenticated and limited-token issue/revoke attempts fail. Owner
   issuance with wrong canonical binding/revision or unapproved source fails. Adapter has no owner
   bearer/FD/env entry. Lost token cannot be fetched by grant ID or renewed by an agent.
   A valid unindexed fixture reports revision 0 to owner binding discovery; a well-formed issuance
   with positive expected revision returns 409 `no_published_index`, `retryable: false`, and creates
   neither grant nor token. No file repair/index starts; missing database is instead unavailable.
   Delete the enrolled cache before issuance and before binding discovery: each returns unavailable,
   latches MCP and leaves cache.db absent, never 409 `no_published_index` or a recreated empty file.
   Verify these routes and final revision checks do not invoke the ordinary writable-open/status
   helpers. If a separate browser read recreates the file, the old MCP binding still fails and its
   latch remains set; no grant can be issued against that replacement in this daemon lifetime.
   Issuing inspect (either view) or read_source with `sourceApproved: false` returns 403; describe/find
   only succeeds with the owner's structural-metadata disclosure approval.
4. **Server authorization:** bypass the adapter and use a limited bearer directly against every
   existing route/method, grant endpoint, unknown tool and omitted capability. All are denied except
   the four authorized service reads. Owner bearer on service routes fails. An MCP annotation change
   cannot change these outcomes. A describe/find-only grant cannot invoke either inspect view or
   read_source, nor override approval in tool arguments. Also test an inconsistent injected grant
   policy to prove backend source-approval checks independently of issuance. With inspect approved
   but read_source omitted, bounded literal callee fragments are allowed; full source remains denied.
   Condition labels and callback bodies never leak through native DTO serialization. Index, provider,
   artifact and runtime-control counters remain zero.
5. **Identity:** a second fixture daemon with equal symbol IDs and numeric revisions rejects the
   first descriptor/grant. Restart invalidates old grants. With the first daemon still running,
   persistently replace/delete its cache with a same-revision fixture DB; identity checks must reject
   it, revoke all grants, suppress pending results and latch MCP unavailable until restart. Repeat
   for workspace.db and the state directory, including during issuance, describe and final response
   admission. Inject a swap between setup path checking and SQLite open to prove the actual opened
   handle is checked; reject wrong/reopened pool members and unsupported/erroring HAS_MOVED controls.
   A supported destructive restore invalidates before mutation; owner reissuance cannot clear the
   latch. Normal transactional publication/checkpoint keeps generation while revision rules apply.
   Do not assert that the guard detects unobserved ABA, inode-preserving external restores or sidecar
   manipulation; these are unsupported live mutations. No fallback/re-enrollment/registry lookup occurs.
6. **Revision race:** omit/null/alter expected revision on each evidence read; reject all. Publish a
   new fixture revision between admission/read/application response commit and require conflict,
   never mixed evidence.
   Describe alone reports current/admitted revision without source; only owner reissue restores reads.
7. **Bounds:** malformed paths, external source IDs, unknown fields, deep/recursive inspect requests,
   overlong literals/ranges, oversized lines and excessive response bytes fail or report exact
   truncation. Deterministic capped symbol/call results are never reported complete. Concurrent calls
   cannot evade lifetime bytes/requests or concurrency ceilings; no SQL/shell/regex input exists.
   Qualify read-only enrollment/reads against both warmed WAL fixtures and a cold fixture with no
   `-shm`, plus insufficient sidecar permissions. Verify supported SQLite coordination or a closed
   failure, never main-database creation/migration, writable-main fallback or `immutable=1`.
8. **Expiry/revoke:** use a fake clock for 900-second expiry and wall-clock changes. Owner revocation
   denies new reads and cached hits, rejects pending results that have not reached application response
   commit, and is idempotent. Replayed stolen
   grant succeeds only within its authorized scope before revocation/expiry, documenting bearer risk.
9. **Transport/secret hygiene:** hostile Origin/Host, duplicate auth, redirects and proxy environment
   cannot send the grant elsewhere. Insecure/symlink/hard-linked/oversized grant files fail. Paths
   inside agent-readable roots fail setup. No token appears in argv/env, MCP responses, protocol stdout,
   logs or errors. Test file cleanup and separately test an inherited FD launcher if supported.
10. **Lifecycle:** cancel during a slow read, stdio EOF, daemon restart, logout and session replacement
    suppress late replies, clear caches and trigger owner revocation when supervised. When unsupervised,
    verify expiry instead of claiming immediate revocation. Cancel/timeout never starts a provider,
    retries with owner credentials or auto-indexes. Stdout stays valid MCP throughout.
    Crash after file consumption, then simulate a client auto-restarting the adapter: every restart
    fails locally with `owner_reissue_required`, zero daemon requests and no credential recreation.
    Verify supported launcher retry caps/owner notice; simulate an uncontrollable client and document
    operator-disable recovery instead of claiming the adapter can stop that client's loop. Recovery
    succeeds only after a new explicit owner handoff. Repeat the exhausted-FD case if supported.

Ship the four-tool pilot only after these gates pass. Text scan, diagram/artifact delivery, Mimir
profile admission, Herdr discovery and multi-project registry each require their own later slice.
