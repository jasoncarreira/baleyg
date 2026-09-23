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
| Fact cache | `<cache>/facts.db` | File content | Cache; size-bounded |
| Durable data (views, notes, future artifacts) | `<data>/workspaces/<uuid>/workspace.db` | Workspace UUID | Durable |
| Tokens and Jev/ACP ledgers | Explicitly configured paths | — | Durable |

`<cache>` and `<data>` are the fixed per-user cache and data directories from `ProjectDirs` for
`dev.odin.baleyg`. There is no environment override and no `--state-dir` flag. Nothing Baleyg writes
lives inside a checkout. Token and ledger paths must be outside every workspace root. There are no
state migrations: existing saved views and notes are not carried over, and the owner moves any
existing in-tree ledger directory by hand, once, with Baleyg stopped.

**Workspace UUID.** Created exclusively (the first process to create it wins; others read it) at
`<git-dir>/baleyg/workspace-id`, where `<git-dir>` is `.git` for a main
checkout, or the directory a `.git` file points to (linked worktree, submodule, `--separate-git-dir`).
Moving a checkout keeps its UUID, so its views and notes stay connected; its index is rebuilt at the
new path, cheaply, from the fact cache. A copy of a checkout keeps the same UUID and shares its views
and notes, which is harmless. Outside Git there is no UUID: durable data is keyed by the root path,
and moving the directory disconnects it (it is reported, never deleted).

## Index as a cache

- The index records its schema version and extractor version. Any mismatch, or an integrity failure,
  means rebuild. There is no index migration. The leader rebuilds **inside the existing file** (drop
  and recreate the tables in one transaction), so readers holding it open keep a consistent snapshot
  and SQLite's WAL files are never shared between two databases. Only a file SQLite cannot open at all
  is deleted and recreated.
- `indexGeneration` is a random value created with each index file. Evidence basis is
  `{indexGeneration, indexRevision}`, so revisions from a rebuilt index never match old ones.
- Change detection is a stat scan (size, mtime, inode) against the index's file table, hashing only
  files whose stat changed, with Git's racy-timestamp rule (re-hash when mtime is not older than the
  previous scan). It uses the existing `ignore`-crate walker and its exclusions.
- The **fact cache** holds a path-neutral extraction record per
  `(language, extractorVersion, extractionContextDigest, contentHash)`, written with
  `INSERT OR IGNORE`. A missing or evicted entry is just a cache miss. Assembly binds a record to a
  path and derives the node IDs. A new worktree, or a moved checkout, parses only files the cache has
  never seen.

## Leader

- Every Baleyg process for a checkout (each `baleyg mcp`, the browser daemon, an explicit CLI job)
  tries a non-blocking `flock` on `leader.lock` (a non-blocking variant of the `initialization_lock`
  helper the Jev/ACP ledgers already use), and after locking verifies that the path still names the
  locked file. The holder is the
  leader. The kernel releases the lock when the leader exits or crashes; non-leaders retry the lock
  periodically and at their next request, and the first to succeed takes over. This is a local OS
  lock, not a distributed protocol.
- **Only the leader writes native index rows.** On taking the lock it clears the index's `reconciled`
  marker, starts the watcher, runs a catch-up stat scan (or the initial index), publishes, and sets
  `reconciled`. Evidence tools serve only while `reconciled` is set, so no process ever serves an
  index that nobody has checked against the files since the last leader left.
- **Requests from others.** An explicit index request (CLI or browser) inserts a row into the index's
  `index_requests` table; the leader sees it through `PRAGMA data_version` and runs it. Requests are
  queued, never rejected. If there is no leader, the requester takes the lock and leads for the job.
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
Saved views and notes anchor to declaration IDs, so they survive edits to the file and orphan only
when the declaration is removed or renamed.

## Re-resolution

Each binding records the simple names it looked up, including lookups that found nothing or several
candidates. When the leader publishes a changed file, it diffs the file's old and new declaration
records and collects the names whose declarations were added, removed, or changed in anything a
lookup can see (visibility, export, signature, owner, supertypes, re-exports). It re-resolves every
binding that looked up one of those names, in the same transaction. Name keys over-approximate, which
is safe. Above a threshold of changed files, it re-resolves the whole workspace.

## Semantic evidence (after the syntax-tier release)

- Semantic facts are overlay rows keyed by `(path, contentHash, artifact)`, imported by an explicit
  owner command in a short transaction. An overlay applies only while its document's content hash
  equals the current one, so an edit withdraws it without any write.
- **Freshness is computed at read time.** Each source set carries a `lastSurfaceChangeRevision`,
  advanced whenever any declaration surface or build/config/dependency input in it changes. An overlay
  is `possiblyStale` when that revision is newer than its import revision. This is conservative and
  costs nothing on edit.
- A semantic binding whose target declaration ID no longer exists is `staleTarget`, never resolved,
  and is never rebound by name or position.
- Semantic basis names the producer, profile, artifact hash, document content hash, and a digest of the
  source-set manifest.

## Revision pins

Every result reports its `evidenceBasis` and comes from one read transaction. Pins are optional: a
client may send `expectedRevision`, and a stale one always returns `revision_conflict` with the current
basis. `baleyg_read_source` may instead take `expectedContentHash`, and conflicts if the cached file
differs. There is no change log and no compatibility exception.

## Cleanup (automatic)

The leader runs garbage collection at most once a day, with a non-blocking lock, and never while
another process holds the target:

- Delete the index directory of any root path that no longer exists, or that has not been opened for
  30 days. It is a cache; a moved checkout rebuilds.
- Evict fact-cache entries least recently used beyond a size cap.
- Delete durable records that contain no views and no notes.
- Report, never delete, durable records with content whose checkout can no longer be found.
  `baleyg forget <uuid>` deletes one explicitly, after showing what it holds. Ledgers are never touched
  by garbage collection.

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
