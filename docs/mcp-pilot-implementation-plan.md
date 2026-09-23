# MCP read-only pilot — implementation plan

> **Superseded (2026-09-23).** This plan implemented the grant-based pilot (enrollment, owner-issued
> grants, limited principals, budgets, HTTP tool routes). Its PRs were closed unmerged. The accepted
> design is the [local topology](local-topology.md) and [stdio MCP contract](mcp-readonly-pilot-contract.md),
> sequenced by [#8](https://github.com/jasoncarreira/baleyg/issues/8). Kept as history; do not implement from it.

Status: **PLAN — no implementation authorized beyond slice 0**. This sequences the work specified
by the [read-only pilot contract](mcp-readonly-pilot-contract.md), which remains the normative
specification and acceptance gate. Where this plan and that contract disagree, the contract wins.
Nothing here authorizes indexing, provider calls, changes to a running daemon, credential access,
or execution of an inspected repository.

Code references are navigation aids for the tree inspected while writing this plan, not stable API
identifiers. Each was checked against the working tree rather than assumed.

## Scope

Phase 1 of the [agent integration plan](agent-integration-plan.md): four read-only MCP tools against
one already-running, already-indexed daemon, behind owner-issued scoped grants. Snapshot text search,
diagram artifacts, the Mimir provider profile, Herdr discovery, the project registry and embedded
terminals are separate later contracts and appear in no slice below.

## Module layout

New code lives under `src/mcp/`, keeping the contract's separations visible in the tree:

| Module | Owns |
| --- | --- |
| `enrollment` | Retained FDs, device/inode anchors, the enrolled read-only connection, identity checks, generation epoch, unavailability latch, read serialization and interruption |
| `grants` | In-memory grant table, capabilities, TTL, budget reservation and settlement, revocation |
| `http` | Owner control routes, the four tool routes, DTO projection, error envelope |
| `stdio` | The `baleyg mcp` adapter: framing, `tools/list` filtering, cancellation, EOF teardown |
| `handoff` | Descriptor and grant-file creation, validation and consumption |

Keeping enrollment separate from `store::Store` is deliberate. The contract requires a connection
`Store` does not provide, and the two must not become confusable at a call site.

## Why pilot reads cannot call existing `Store` entry points

Verified in the working tree:

| Entry point | Connection | Revision guard |
| --- | --- | --- |
| `symbols_at` (`src/store.rs:1266`) | `self.cache()?` — reopens by path | none; returns revision only |
| `entity_at` (`src/store.rs:1286`) | `self.cache()?` — reopens by path | `Option<u64>`, opt-in |
| `symbol_at` (`src/store.rs:1301`) | via `entity_at` | `Option<u64>`, opt-in |
| `source_at` (`src/store.rs:1312`) | via `entity_at` | `Option<u64>`, opt-in |
| `query_view` (`src/store.rs:1454`) | `self.cache()?` — reopens by path | none |

`Store::cache()` (`src/store.rs:837`) opens a fresh connection per call through `connect()`
(`src/store.rs:95`), which performs writable setup and migration; `secure_database_file()`
(`src/store.rs:70`) uses `.create(true)`, so a deleted cache is silently recreated empty. Each
property independently defeats the binding the contract requires.

Pilot reads therefore reuse these **SQL patterns**, executed on the enrolled connection with
mandatory revision equality. To stop the copies drifting, hoist the query text into shared `const`
strings consumed by both the `Store` method and the pilot reader; the connection and guards differ,
the SQL should not.

## Slices

### Slice 0 — storage and cancellation spike (throwaway)

Answer the questions the contract marks "qualify this" before any product code exists. Disposable
fixture databases only.

- Read-only open against a WAL database, warm and cold (no `-shm`), including a case where sidecar
  creation is refused. `immutable=1` is out of scope by contract.
- `SQLITE_FCNTL_HAS_MOVED` on the actual connection: behavior for replace, delete, rename, and an
  inode-preserving overwrite. Confirm the reported detection boundary rather than assume it.
- Interruption: what an interrupted read returns, whether the transaction needs explicit rollback,
  and **whether the connection remains usable afterwards** (see open questions).
- Qualify on both CI platforms — `.github/workflows/rust.yml:9` already runs
  `[ubuntu-latest, macos-latest]`, so no new infrastructure is needed. If both support the file
  control, cover the `SQLITE_NOTFOUND` path by injection instead.

Available without new dependencies: rusqlite 0.40.2 re-exports `libsqlite3_sys as ffi`
(`lib.rs:65`) and exposes `unsafe fn handle()` (`lib.rs:952`); the bundled bindings define
`SQLITE_FCNTL_HAS_MOVED = 20` and `sqlite3_file_control` (libsqlite3-sys 0.38.2, SQLite 3.53.2).
`Connection::get_interrupt_handle()` (`lib.rs:1027`) returns a `Send + Sync` `InterruptHandle`
(`lib.rs:1291`), so a watchdog may interrupt from another thread.

**Output:** a findings note and a go/no-go on whether the contract is implementable as written.
**Not in this slice:** merged product code, any HTTP route, any grant.

### Slice 1 — enrollment and identity service

**Deliverable:** `src/mcp/enrollment.rs`. Retained no-follow directory and file FDs, recorded
device/inode identities, one persistent read-only connection, identity checks before and after
connection setup and around every read, the in-memory generation epoch, and the lifetime
unavailability latch. Also owns read serialization, bounded queueing, the interrupt handle and the
post-interrupt recovery policy decided in slice 0.

**Gates:** acceptance test 5 (identity) and the read-only WAL qualification in test 7.
**Not in this slice:** HTTP, grants, tools.

### Slice 2 — grant model, budgets and owner control routes

**Deliverable:** digest-only in-memory grant table, monotonic TTL, capability set, `sourceApproved`,
server ceilings, budget reservation and atomic settlement, expiry/revocation checks and
final-response admission. Routes `GET /api/mcp-pilot/binding`, `POST /api/mcp-pilot/grants`,
`DELETE /api/mcp-pilot/grants/{grantId}`.

Budget accounting belongs here, not with the evidence tools: describe is a limited-grant tool route
and the contract's caps include error and metadata envelopes, so the first tool to ship already
consumes budget. All revision reads use the enrolled connection — never `Store::status()`, which
would turn a destroyed cache into a friendly `no_published_index` instead of an unavailability latch.

**Gates:** acceptance tests 3 (bootstrap authority) and 8 (expiry/revoke).

### Slice 3 — principal split and default-deny dispatch

**Deliverable:** the guard at `src/http.rs:424` grows distinct owner and limited-grant principals
with default-deny dispatch. A limited grant is rejected on every pre-existing route in the table at
`src/http.rs:488` — status, source, query, index, jobs, views, annotations, question and provider
routes — and the owner bearer is rejected on tool routes.

This is the security boundary, so it is its own slice with a test that sweeps the entire route table
rather than a sampled subset. Preserve the existing single-`Host`, single-`Origin`, single-
`Authorization` and constant-time comparison behavior exactly.

**Gates:** acceptance test 4 (server authorization).

### Slice 4 — `baleyg_workspace_describe` end to end

**Deliverable:** the first real vertical over HTTP, exercised with a plain client: enrolled
connection, real grant, real principal check, budget consumption and final-response admission, one
tool. Proves the three preceding layers compose.

No stub path uses the owner bearer. The contract rules that out explicitly — "giving an adapter the
owner bearer and hiding tools is not this design" — so there is no shortcut worth taking here.

**Gates:** part of test 1; describe behavior in test 6.
**Not in this slice:** MCP framing.

### Slice 5 — the three evidence tools

**Deliverable:** `baleyg_find_symbols`, `baleyg_inspect`, `baleyg_read_source` as SQL patterns on
the enrolled connection with mandatory `expectedRevision` equality against both the admitted
revision and the transaction-pinned current revision.

The substantive work is the explicit projection. Native DTOs must not be serialized through:
`CallSite.callee_text` (`src/model.rs:70`) and `ControlRegion.label` (`src/model.rs:84`) are literal
source substrings, and control-region objects, callback bodies and unlisted native fields are
excluded from the pilot projection entirely. Add the 1024-byte `calleeText` cap with its per-call
and envelope truncation flags, plus concurrency cap and operation deadline.

Native bounds already constrain inputs: `ViewQuery::validate` (`src/model.rs:184`) enforces seed
1..8192 bytes, depth ≤ 5, `maxNodes` 1..150 and `maxCalls` 1..500, so the contract's 50-call ceiling
sits inside them. Outgoing calls are outgoing only (`src/store.rs:1473`, `WHERE caller=?1`); there is
no incoming hierarchy to expose.

**Gates:** acceptance tests 1, 6 and 7.

### Slice 6 — stdio MCP adapter

**Deliverable:** `baleyg mcp --binding-file`, added beside the existing `index`/`serve`/`status`/
`symbols`/`query`/`export` subcommands (`src/main.rs:32`). Protocol-only stdout with diagnostics on
stderr, `tools/list` filtered to granted capabilities, structured errors returned as `isError`,
cancellation that suppresses late responses, EOF teardown, and the consumed-grant
`owner_reissue_required` exit.

**Gates:** acceptance tests 2 (peer catalog) and 10 (lifecycle).

### Slice 7 — owner helper and handoff hygiene

**Deliverable:** `baleyg mcp-grant issue`. Operator-controlled 0700 directory with symlink-free
resolution, refusal when a configured path lies inside an agent-readable root, exclusive no-follow
0600 grant file validated through the opened FD, unlink after consumption, exact loopback base URL
with no redirects or proxy routing, and no token in argv, environment, logs or diagnostics.

**Gates:** acceptance test 9 (transport and secret hygiene); the "adapter holds no owner bearer"
case in test 3.

### Slice 8 — adversarial sweep

**Deliverable:** all ten acceptance groups run as a suite rather than as leftovers — fake clock, a
second fixture daemon with colliding symbol IDs and revision numbers, an injected inconsistent grant
policy proving the backend checks source approval independently of issuance, concurrent calls racing
the lifetime and concurrency ceilings, and a swap injected between path checking and SQLite open.

Ship only after these pass.

## Sequencing

Slice 0 blocks slice 1; slice 1 blocks everything after it. Slices 1 and 2 can proceed alongside
slice 3, which touches a different file and shares only a principal type. Slices 6 and 7 pair
naturally — the adapter and the thing that feeds it.

Expect surprises in slice 0 (unknown until run), slice 3 (the existing guard is one linear
`if`/`else` chain, and splitting principals must not weaken its header or timing behavior), and
slice 6 (per-client launcher and restart behavior varies more than a specification can anticipate).

## Open questions for slice 0

1. **Post-interrupt connection reusability.** The contract forbids lazily replacing a connection or
   rebinding after an error. If an interrupted read leaves the enrolled connection unusable, the
   only compliant response is the unavailability latch — meaning a single cancelled request would
   disable the MCP surface for the daemon's lifetime. That is not acceptable behavior, and the
   contract does not currently resolve it. The spike decides whether recovery is ordinary error
   handling or the contract needs an amendment.
2. **Cold-start sidecar permissions.** Whether a read-only main connection can establish supported
   WAL coordination when `-shm` is absent, and what the closed failure looks like when it cannot.
3. **File-control availability.** Whether `HAS_MOVED` is supported by the bundled VFS on both CI
   platforms, and what an unsupported or erroring result must map to.

## Authorization boundary

The first implementation authorization covers **slice 0 only**: disposable fixture databases, no
inspected repositories, no provider calls, no changes to a running daemon, no credential access. Its
findings determine whether the contract is ready to implement. Slices 1 onward require their own
authorization after that.
