# Local index and agent topology

Status: **direction accepted by the owner (2026-09-23); the mechanics in this document are proposed
until Stage 1 ratifies them.** Stage 1 of the semantic-index program ([#8](https://github.com/jasoncarreira/baleyg/issues/8))
turns this document into contract text. Paths, commands and fields below are proposals, not claims
about the current CLI or store. This document supersedes the one-daemon, owner-grant topology of the
earlier MCP pilot.

## Problem

Coding agents work in many checkouts at once: the main checkout, several Git worktrees, and
Feature Factory sandboxes that are created and deleted routinely. Each needs evidence about the
source it is actually editing, not about `main`. Indexing a large codebase is hard enough, so the
topology around it must stay simple.

## Design in one paragraph

Each checkout has an index that is a **pure cache**, stored outside the checkout and keyed by the
checkout's path. Durable data (saved views, notes) is keyed by a **workspace UUID** kept in the
checkout's Git directory, so it follows the checkout when it moves. Agents reach the index through
**`baleyg mcp`**, a stdio MCP server the agent client launches in the checkout. One process per
checkout is the **leader**, chosen by an OS file lock: it alone watches the files and writes native
index rows. Node IDs are **stable across edits**, so an edit changes only what it actually changes.
Anything derived is rebuilt rather than repaired, and cleanup is automatic.

## Workspace discovery

A process chooses its workspace root in this order: an explicit `--workspace PATH`; the Git top level
of the working directory, found in-process by walking up to a `.git` directory or file; otherwise the
working directory. The home directory and filesystem root are refused unless named explicitly.
Baleyg runs no `git` subprocess for discovery or indexing.

## Storage

| State | Location | Keyed by | Lifetime |
| --- | --- | --- | --- |
| Index (graph, cached source) | `<cache>/indexes/<root-key>/index.db` | Canonical root path | Cache; rebuilt freely |
| Leader lock | `<cache>/indexes/<root-key>/leader.lock` | Canonical root path | Removed with its index |
| Index use lock | `<cache>/indexes/<root-key>.lock`, beside the index directory | Canonical root path | Removed last, by the deleter holding it |
| Explicit index requests | `<cache>/indexes/<root-key>/requests.db` | Canonical root path | Survives index rebuilds |
| Fact cache | `<cache>/facts.db` | File content | Cache; size-bounded |
| Durable data (views, notes, future artifacts) | `<data>/workspaces/<record-id>/workspace.db` | Workspace UUID, or `path-<root-key>` outside Git | Durable; created on first write |
| Durable use lock | `<data>/workspaces/<record-id>.lock`, beside the record directory | Record id | Removed last, by `forget` holding it |
| Tokens and Jev/ACP ledgers | Explicitly configured paths | — | Durable |

`<cache>` and `<data>` are the fixed per-user cache and data directories from `ProjectDirs` for
`dev.odin.baleyg`. There is no environment override and no `--state-dir` flag. Nothing Baleyg writes
lives inside a checkout. Token and ledger paths must be outside every workspace root. There are no
state migrations: existing saved views and notes are not carried over, and the owner moves any
existing in-tree ledger directory by hand, once, with Baleyg stopped.

**Workspace UUID.** Stored at `<git-dir>/baleyg/workspace-id`, where `<git-dir>` is `.git` for a main
checkout, or the directory a `.git` file points to (linked worktree, submodule, `--separate-git-dir`).
Baleyg parses the `gitdir:` line itself, requires the target to be an existing directory owned by the
current user, opens the UUID file with no-follow, and accepts only a canonical lowercase UUID; anything
else is refused, never used as a path. The file is created exclusively, so concurrent first openers
agree on one UUID. Moving a checkout keeps its UUID, so its views and notes stay connected; its index
is rebuilt at the new path, cheaply, from the fact cache. A copy keeps the same UUID and shares its
views and notes, which is harmless. Outside Git there is no UUID: the durable record id is
`path-<root-key>`, and moving the directory disconnects it (it is reported, never deleted).

A durable record directory and `workspace.db` are created only when the first view or note is written,
so checkouts that never save anything, such as most Feature Factory sandboxes, leave nothing durable
behind. Every process that opens a durable record, to read or write, holds a shared `flock` on the
record's use lock for as long as it has `workspace.db` open; `baleyg forget` needs it exclusively.

## Index as a cache

- The index records its schema version and extractor version. Any mismatch, or an integrity failure,
  means rebuild. There is no index migration. The leader rebuilds **inside the existing file** (drop
  and recreate the index tables in one transaction), so readers holding it open keep a consistent
  snapshot. Explicit requests live in `requests.db`, which a rebuild does not touch.
- **Use locks.** Use locks live *beside* the directory they protect, never inside it, so deleting the
  directory cannot remove or recreate the lock mid-operation. A deleter holds the lock exclusively for
  the whole operation, removes the directory, unlinks the lock file last, then releases. Every locker
  verifies after locking that the path still names the file it locked and retries if not, so a process
  racing a deletion starts over against fresh state.
- **Index use lock.** Every process that has the index open holds a shared `flock` on its use lock.
  Deleting
  or recreating `index.db` (a file SQLite can no longer open or rebuild, or garbage collection)
  requires the exclusive lock, taken non-blocking; a reader that gets `SQLITE_CORRUPT` closes its
  connection, releases its shared lock, and retries after the leader has recreated the file. So no two
  database incarnations ever share one pathname while anyone has the old one open.
- `indexGeneration` is a random value set when the index is created and replaced on every rebuild.
  Evidence basis is `{indexGeneration, indexRevision}`, so revisions from a rebuilt index never match
  old ones.
- Change detection is a stat scan (size, mtime, ctime, inode) against the index's file table,
  hashing only files whose stat changed, with Git's racy-timestamp rule (re-hash when mtime or ctime
  is not older than the previous scan). ctime changes on every write and cannot be set by tools that
  preserve mtime, which closes the same-size, preserved-mtime case. It uses the existing `ignore`-crate walker and its exclusions.
- The **fact cache** holds a path-neutral extraction record per
  `(language, extractorVersion, extractionContextDigest, contentHash)`, written with
  `INSERT OR IGNORE`. A missing or evicted entry is just a cache miss. Assembly binds a record to a
  path and derives the node IDs. A new worktree, or a moved checkout, parses only files the cache has
  never seen.

## Leader

- Every Baleyg process for a checkout (each `baleyg mcp`, the browser daemon, an explicit CLI job)
  tries a non-blocking `flock` on `leader.lock` (a non-blocking variant of the `initialization_lock`
  helper the Jev/ACP ledgers already use), and after locking verifies that the path still names the
  locked file. The holder is the leader. The kernel releases the lock when the leader exits or
  crashes; non-leaders retry the lock periodically and at their next request, and the first to succeed
  takes over. This is a local OS lock, not a distributed protocol.
- **Leader incarnation.** Immediately after locking, the leader writes a fresh random incarnation id
  into `leader.lock`. The index's `reconciled` marker records the incarnation that reconciled it.
  Evidence is served only while the lock is held and the marker's incarnation equals the one in
  `leader.lock`, so once a successor has written its id, a crashed leader's marker no longer matches.
  **Accepted window:** between a successor taking the lock and writing its id (a few system calls), a
  reader may still serve the previous leader's last reconciled revision. That evidence is labelled
  with its basis and lags the files by no more than ordinary watcher latency does; it is the same
  guarantee readers have at all times, not an exception to it.
- **Root identity.** The leader records the root directory's device and inode when it starts, and
  re-checks them before every reconcile and publish; readers re-check before serving. If the path no
  longer names that directory (the checkout moved, or something else now occupies the path), the
  process stops serving and leading for it; it never scans or publishes a different directory.
- **One ordered queue.** The leader runs all native work, meaning watcher batches, reconciles and
  explicit requests, one job at a time from a single queue, so a slow job can never commit an older
  observation over a newer one. A long job checks the watcher's pending events before committing and
  re-reads any path that changed while it ran.
- **Requests from others.** An explicit index request (CLI or browser) is appended to `requests.db`;
  the leader claims it, runs it, and marks it done; a request claimed by a leader that then died is
  reclaimed by the next one. Requests are queued, never rejected. If there is no leader, the requester
  takes the lock and leads for the job.
- **Watcher.** The leader watches the checkout with the `notify` crate, debounces and coalesces events,
  re-extracts changed files (or takes them from the fact cache), and publishes one short transaction
  per batch. Watcher overflow, lost-event notifications, watch-limit exhaustion, bulk changes such as
  a checkout or rebase, and system wake trigger a full stat reconcile; a slow periodic reconcile
  catches silently lost events.
- Readers use SQLite WAL snapshots and never block the leader. Each publish advances `indexRevision`.

## Stable node IDs

A declaration's ID is its path plus a declaration key: the chain of enclosing declarations, its kind,
its name, and an overload signature or ordinal among same-named siblings. Its range and the file's
content hash are separate fields. A body-only edit changes no IDs. A call site is identified by its
caller's ID and its ordinal within the caller, and a control region likewise; these are occurrence IDs
within one revision. SCIP symbols are separate semantic bindings, never node IDs, for every language.
Saved views and notes anchor to a declaration ID plus a hash of the declaration's header text (name,
signature, modifiers). If an ID now names a declaration whose header hash differs, which can happen
when an earlier same-named sibling is inserted or removed, the anchor orphans rather than silently
moving to another declaration. When same-named siblings have identical headers, the hash cannot tell
them apart, so an anchor also records how many identical-header siblings existed; if that count
changes, the anchor orphans. Anchors survive body edits and orphan when the declaration is removed,
renamed, its header changes, or its identical-header sibling group changes.

## Re-resolution

Each binding records the simple names it looked up, including lookups that found nothing or several
candidates, **and the names of the scopes it traversed**: enclosing classes and modules, supertypes,
imported modules, and re-export sources. When the leader publishes a changed file, it diffs the
file's old and new declaration records and collects every name whose declaration was added, removed,
or changed in anything a lookup can see (visibility, export, signature, owner, supertypes, imports,
re-exports). It re-resolves every binding that recorded one of those names, as a looked-up name or as
a traversed scope, in the same transaction. Name keys over-approximate, which is safe. Above a
threshold of changed files, it re-resolves the whole workspace.

## Semantic evidence (after the syntax-tier release)

- Semantic facts are overlay rows keyed by `(path, contentHash, artifact)`, imported by an explicit
  owner command in a short transaction. An overlay applies only while its document's content hash
  equals the current one, so an edit withdraws it without any write.
- **Freshness is computed at read time.** At import, an artifact is *basis-current* only if its
  manifest (every document's content hash plus the build, configuration and dependency inputs)
  matches the current source set exactly; otherwise all of its overlays are `possiblyStale` from the
  start. Each source set carries a `lastSurfaceChangeRevision`, advanced whenever a declaration
  surface or a build/config/dependency input in it, **or in any source set it depends on**, changes.
  A basis-current overlay becomes `possiblyStale` once that revision is newer than its import
  revision. This is conservative and costs nothing on edit.
- A semantic binding whose target declaration ID no longer exists is `staleTarget`, never resolved,
  and is never rebound by name or position.
- Semantic basis names the producer, profile, artifact hash, document content hash, and a digest of the
  source-set manifest.

## Revision pins

Every result reports its `evidenceBasis` and comes from one read transaction. Pins are optional: a
client may send a complete `expectedBasis` (`indexGeneration` and `indexRevision` together, never a
revision alone), and a stale one always returns `revision_conflict` with the current basis. `baleyg_read_source` may instead take `expectedContentHash`, and conflicts if the cached file
differs. There is no change log and no compatibility exception.

## Cleanup (automatic)

The leader runs garbage collection at most once a day:

- Delete the index directory of any root path that no longer exists, or that has not been opened for
  30 days, but only after taking its index use lock exclusively, non-blocking; if anyone has it open,
  skip it.
- Evict fact-cache entries least recently used beyond a size cap.
- Durable records are never deleted automatically. Empty ones never exist (records are created on
  first write). Records whose checkout can no longer be found are reported by `baleyg gc --report`;
  `baleyg forget <record-id>` deletes one explicitly, after showing what it holds, while holding the
  record's use lock exclusively. Ledgers are never touched.

## Threat and resource boundary

- stdio has no network surface: no port, no Host/Origin handling, no DNS-rebinding exposure, and other
  OS users cannot connect.
- The boundary is the OS user. Any same-user process can launch `baleyg mcp` or read the index
  directly, just as it can read the source. MCP tools add bounded, typed access, not isolation. A
  hostile-agent threat model needs a separate OS account or sandbox.
- Configuring `baleyg mcp` for an agent approves disclosure of the whole indexed checkout to that agent
  and its provider. There is no per-path scope and no grant.
- Per-call limits bound each request, not aggregate use across many MCP processes of one user.
- Repository text is untrusted data, never an instruction or permission.

## Superseded

This topology replaces owner-issued grants, limited principals, budgets, enrollment identity
latching, grant handoff files and the HTTP tool routes of the earlier pilot documents, and the
in-checkout index, placement, quarantine and per-path versioning of earlier drafts of this document.
The [multi-project viability review](multi-project-viability.md) is still accurate about the current
code, but its one-daemon registry recommendation is not the chosen agent topology.
