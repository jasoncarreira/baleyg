# Local index and agent topology

Status: **direction accepted by the owner (2026-09-23); mechanics proposed until Stage 1 ratifies
them.** Stage 1 of the semantic-index program ([#8](https://github.com/jasoncarreira/baleyg/issues/8))
turns this document into contract text. Paths, commands and fields below are proposals, not claims
about the current CLI or store. This document supersedes the one-daemon, owner-grant topology of the
earlier MCP pilot.

## Problem

Coding agents work in many checkouts at once: the main checkout, several Git worktrees, and
Feature Factory sandboxes that are created and deleted routinely. Each needs evidence about the
source it is actually editing, not about `main`. The current store binds one state directory to one
canonical root, keeps it outside the repository, has no file watcher, and publishes by replacing
the whole graph. One long-running daemon per checkout would need port assignment, discovery,
supervision and explicit teardown, and an unauthenticated loopback listener would let any local
process read any checkout's cached source.

## Decisions

1. **Every checkout has a workspace UUID** that survives moving it, so its state stays connected.
2. **One index per checkout, normally inside the checkout** at `<root>/.baleyg/index.db`, opened
   under strict safe-open rules.
3. **A shared, path-neutral fact cache** makes a new checkout cheap to index.
4. **Agents reach the index through `baleyg mcp`, a stdio MCP server** that the agent client
   launches and tears down. There is no network listener for agents.
5. **One watcher leader per workspace; publication by short optimistic transactions** that any
   Baleyg process may run.
6. **Native extraction is the only automatic work.** Semantic producers stay explicit owner actions.

## Workspace discovery and identity

**Discovery.** A process chooses its workspace root in this order:

1. An explicit `--workspace PATH`.
2. The Git top level of the working directory (`git rev-parse --show-toplevel`), which is the
   worktree root for a linked worktree.
3. The nearest ancestor of the working directory that already has a workspace UUID.
4. The working directory itself.

The user's home directory and the filesystem root are refused unless named explicitly with
`--workspace`, so launching an agent from `~` never indexes the whole home directory.

**Workspace UUID.** Each workspace root has a random UUID that names all of its per-user state:

- In a Git checkout it lives in the checkout's Git directory:
  `$(git rev-parse --git-dir)/baleyg/workspace-id`. A linked worktree has its own Git directory
  (`.git/worktrees/<name>/`), so each worktree has its own UUID. Moving the checkout, running
  `git clean -fdx`, or deleting `.baleyg/` leaves it intact. A fresh clone gets a new one.
- Outside Git it lives at `<root>/.baleyg/workspace-id`. Deleting `.baleyg/` loses it, and the
  checkout then starts over as a new workspace.
- The UUID file is created exclusively, no-follow, 0600, and read under the same safe-open rules as
  the index.

**Moves and copies.** The per-user record for each UUID stores the canonical root it was last seen
at. When a process opens a workspace:

- If the record's root is this root, nothing changes.
- If the record's root no longer exists, the checkout moved. The record is updated to the new root,
  and its saved views, notes, locator and any fallback index stay connected.
- If the record's root still exists and is a different directory, this checkout is a copy
  (`cp -r`, a restored backup). It gets a fresh UUID and starts as a new workspace.

The check and the update run under an exclusive lock on the UUID's state directory.

Move a linked Git worktree with `git worktree move`, or run `git worktree repair` after a plain `mv`.
Until Git's link is repaired, `git rev-parse --git-dir` fails, the checkout is treated as non-Git, and
it would get a new UUID; Baleyg reports the broken link instead of creating one.

## Storage layout

| State | Location | Lifetime |
| --- | --- | --- |
| Index (derived graph, cached source, change log) | `<root>/.baleyg/index.db`, or `<state>/workspaces/<uuid>/index.db` as fallback | Disposable; rebuildable |
| Workspace record and locator | `<state>/workspaces/<uuid>/workspace.json` | Small; derived except for the root history |
| Watcher-leader lock | `<state>/workspaces/<uuid>/leader.lock` | Held by the leader process |
| Durable data (saved views, annotations, future artifacts) | `<state>/workspaces/<uuid>/workspace.db` | Durable; never removed automatically |
| Secrets and ledgers (daemon token, Jev/ACP ledgers) | Per-user state, outside every workspace root | Durable; never removed automatically |
| Shared fact cache | `$(git rev-parse --git-common-dir)/baleyg/facts/`, else per-user cache | Derived; garbage-collected |

`<state>` is the per-user application-data directory (`ProjectDirs` for `dev.odin.baleyg`; today
`~/Library/Application Support/dev.odin.baleyg` on macOS). It is fixed per user and does not depend
on command-line flags. Nothing durable or secret is ever written under `.baleyg/`: it may be removed
by `git clean -fdx`, copied by backup or sync tools, or included in a `docker build .` context.

### Safe opening of `.baleyg/`

`.baleyg/` sits inside repository-controlled storage, so a checkout can contain a hostile
pre-existing `.baleyg`. Every process applies the rules the out-of-tree store already applies
(`secure_state_dir`, `secure_database_file`):

- Inspect with `lstat` and open with no-follow. Reject a symlink at `.baleyg` or any file inside it,
  and any non-regular file where a regular file is expected.
- Require the directory and files to be owned by the current user, the directory mode 0700 and
  files 0600. Refuse a pre-existing `.baleyg` that fails these checks; never chmod or chown one Baleyg
  did not create. (Today's `secure_database_file` creates a missing file and resets its mode; the
  in-tree path must not reuse that behaviour for files it did not just create.)
- Refuse any database, UUID or cache file with more than one hard link.
- Create `.baleyg/`, `.baleyg/.gitignore` (containing `*`) and `index.db` exclusively, no-follow.
- If `.baleyg` is tracked by Git, refuse to use it and report why.

The same rules apply to the fact cache directory and UUID file inside the Git directory.

### Placement and overrides

The workspace record's locator fields decide where the index lives, so every process opens the same
file:

```json
{ "schemaVersion": 1, "uuid": "...", "lastRoot": "/abs/root", "placement": "inTree",
  "indexPath": "<root>/.baleyg/index.db", "overrides": { "indexDir": null } }
```

- The first process for a workspace chooses the placement under the record's exclusive lock:
  `inTree`, unless safe-open fails or the filesystem cannot support SQLite WAL locking and shared
  memory (some network mounts and container bind mounts), in which case `fallback`.
- **Automatic fallback happens only at that first choice.** If the chosen placement later becomes
  unusable, processes report `store_unavailable` naming the cause, and the owner changes placement with
  an explicit `baleyg placement` command, which rotates the index generation.
- `--index-dir DIR` is recorded in the workspace record the first time it is used, through the same
  explicit command. A later invocation that passes a different value, or omits a recorded one, is
  refused rather than silently opening another index.
- `--state-dir DIR` keeps its meaning as the location of durable and secret state. It must be outside
  the workspace root. Like `--index-dir`, a value that conflicts with the recorded one is refused.

### Migrating existing state directories

Some existing setups pass an in-tree state directory, for example
`--state-dir .baleyg/native-smoke` in `docs/workspace-index-fix.md`, `docs/live-jev.md` and
`docs/browser-token.md`, and pass ledger directories separately (for example `--jev-budget-dir
.baleyg/jev-question-budget`). Those hold a token, ledgers and `workspace.db`, which the new rules
forbid under `.baleyg/`. On first start with the new rules, an in-tree `--state-dir` or ledger
directory is refused with a message naming `baleyg migrate-state`, which:

- Takes the source directories explicitly, including each separately configured ledger directory, and
  lists what it will move before doing anything.
- Copies each file to its per-user destination, fsyncs it and its directory, verifies content and the
  existing permission checks, then removes the original. It never overwrites an existing destination;
  a collision stops the migration with both paths reported.
- Records progress in a journal in the destination so an interrupted migration resumes idempotently,
  and a completed one is a no-op when run again.
- Discards the old `cache.db`; the index rebuilds.
- Keeps each ledger's workspace binding and cap. It never resets or refunds an allowance.

## Index identity

Evidence basis is `{indexGeneration, indexRevision}`. The generation is bound to the database file's
identity, so a random value copied along with the file is not trusted:

- The index stores `{indexGeneration, boundDevice, boundInode}` for its own file.
- On open, if the file's actual device and inode differ from the stored binding, the file is a copy,
  restore or replacement. The store is **quarantined**: evidence tools report `store_unavailable`.
  Quarantine ends only when a native job completes a full reconciliation, re-hashing every file in the
  checkout and republishing whatever differs, and then rotates the generation and rewrites the
  binding. A semantic import can never end quarantine.
- A move within one filesystem preserves device and inode, and relative paths make the index valid at
  the new root, so the generation is kept (see [moves and copies](#workspace-discovery-and-identity)).
- A process compares `fstat` of its open database with `lstat` of the locator's path at the start of
  every transaction, before a publish transaction commits, and before emitting a read response. If the
  path now names a different file, or the open file has been unlinked (link count 0, as after a live
  `git clean -fdx`), it abandons the operation and reopens through the locator.
- **Detection boundary:** these are checks at specific points. An unlink between the pre-commit check
  and the commit loses that publish (it lands in a deleted file, harmlessly) and is caught by the next
  check. An in-place restore that preserves the inode, or a swap away and back between checks, is not
  detected. Restoring an index while processes are running is unsupported; stop them first.

## Shared fact cache

Current syntax IDs embed the path (`syntax:<path>:<hash>:<start>:<kind>`), and diagnostics carry
paths too. Identical bytes at two paths must not share facts that name the wrong path.

- The cache stores a **path-neutral extraction record**: declarations, call sites, regions and
  diagnostics addressed by byte offsets and file-local keys, with no workspace path. Assembly binds
  the record to a path and derives the path-bearing IDs deterministically. A file moved by `git mv`
  keeps its cache hit.
- Key: `(language, extractorVersion, extractionContextDigest, contentHash)`. The context digest
  covers every input to extraction other than the file's bytes, such as the file extension that
  selects the grammar and any dialect setting.
- Entries are written to a temporary file, fsynced, and renamed into place. Each entry carries a
  checksum of its payload; a reader that finds a mismatch discards the entry and re-extracts.
  Concurrent writers of the same key are harmless because the content is identical.
- Indexing holds a shared lock on the cache for its duration. Garbage collection takes the exclusive
  lock, so it never deletes an entry an in-progress job is about to use.

A checkout indexes by listing path/hash pairs (`git ls-files -s` plus `git status` for dirty and
untracked files; size/mtime-guarded hashing without Git), extracting only records the cache lacks,
assembling, resolving and publishing. A new worktree of an indexed repository parses only the files
its branch changed.

## Agent access: `baleyg mcp`

- The agent client starts `baleyg mcp` as a child process. It speaks MCP over stdin/stdout only.
  Stdout carries only protocol messages; diagnostics go to stderr.
- The process discovers its workspace as above and serves exactly that workspace for its lifetime. No
  tool argument selects another workspace.
- It exits on stdin end-of-file and when its parent dies (Linux `PR_SET_PDEATHSIG`, macOS kqueue
  `NOTE_EXIT` on the parent). It never daemonizes and never outlives its client.
- **Launch-time indexing.** If no revision is published, the process asks the native scheduler to
  index at launch, on a background thread. This is part of process start, not a side effect of any
  tool call. Protocol discovery and `tools/list` answer immediately; describe reports progress;
  evidence tools return `no_published_index` until the first revision lands.
- The [MCP contract](mcp-readonly-pilot-contract.md) defines tools, bounds and errors.

## Watcher leadership and publication

Two separate mechanisms, so that no long-lived process can block explicit work.

**Watcher leadership.** An exclusive `flock` on `<state>/workspaces/<uuid>/leader.lock` decides which
process runs the file watcher. The lock lives in per-user state, not in `.baleyg/`, so deleting
`.baleyg/` cannot create a second lock file and a second leader. It is held for the leader's lifetime
and released by the kernel on exit or crash. Holding it grants no publication rights and blocks
nothing else.

**Native publication is optimistic per path.** Every published path row carries a `pathVersion` that
increases each time the row is replaced. Any Baleyg process may run a native job — the watcher
leader, reconciliation, an explicit CLI or browser index request — and each job:

1. Records, for each path it will observe, the `pathVersion` currently published.
2. Observes and extracts files outside any transaction.
3. Publishes in one short `BEGIN IMMEDIATE` transaction. A path's rows are replaced only if its
   `pathVersion` still equals the recorded one. A path that changed underneath the job is not written;
   the job re-observes it (re-reading the file) and retries, until it succeeds or a bounded retry limit
   hands the path to the next reconciliation.
4. In the same transaction, re-resolves affected bindings against the state being committed (below),
   appends to the change log, and advances the revision.

Because a job that lost a race re-reads the file, the newest content wins without relying on a later
watcher event. Resolution runs inside the committing transaction, so cross-path bindings are never
computed from a stale view.

**Semantic overlays are separate.** Semantic facts are stored as overlay rows keyed by
`(path, contentHash, artifact)`, not in the native path rows, and never change `pathVersion`. An
import admits a document's facts only if its content hash still equals the published native hash at
commit time. A native change never waits for, and is never suppressed by, a semantic import.

Explicit index requests from the CLI or browser are native jobs like any other. If another job is
running, the request is accepted or reported as queued, never rejected as busy; the browser's index
route no longer returns 409.

**Watcher sequencing.** A new leader starts watching first and buffers events, then runs a catch-up
scan, then applies buffered events. Watcher overflow, lost-event notifications, exhausted watch
limits and bulk changes such as checkout or rebase trigger a full rescan. Because some platforms lose
events silently, the leader also reconciles periodically (proposed every five minutes and after
system wake) with a size/mtime scan. `.baleyg/` and `.git/` are always excluded, so the index's own
WAL writes never trigger republishing.

A partially written file may be observed mid-save; tree-sitter recovers and marks a parse error, and
the next event or reconciliation corrects it. Source capture is not atomic, as today.

## Re-resolution

A binding can change because of any change to what it looked up, not only additions and removals:

- **Every binding records its lookup keys**, whether it resolved, found nothing, or found several
  candidates: the scope/name keys searched, the module and scope keys consulted for imports, aliases,
  re-exports and wildcards, the receiver, owner or inheritance chain examined, and any workspace-wide
  lookup relied on.
- **Each file publishes a declaration-surface digest per key** covering everything a lookup can
  observe about a declaration: its existence, name, visibility and export status, signature and
  overload arity, owner, supertypes, and re-export or wildcard membership. Bodies are excluded.
- When a delta changes the surface digest for any key, every binding that recorded that key is
  re-resolved, including bindings that were previously unique and resolved.
- A change to any input that affects resolution globally re-resolves the whole workspace.

## Semantic freshness under native refresh

The watcher never runs a producer. When native refresh publishes a change:

- The changed document's semantic overlays no longer match its content hash and are withdrawn; it
  has syntax evidence only until an artifact for its new content is imported.
- Documents that reference the changed document's symbols, that resolved names through an import,
  re-export or wildcard reaching it, or whose recorded lookup keys include a key whose surface digest
  changed, are labelled **possibly stale**, with their basis and the reason.
- If the changed document's exported surface changed, importers of it are labelled possibly stale
  transitively within the source set, because inferred types can propagate beyond one hop.
- A change to a build, configuration or dependency input marks the whole semantic basis of the
  affected source set stale.
- **Stale targets.** A semantic binding whose target node no longer exists (node IDs include the
  target file's hash, so any edit to that file removes it) is never returned as resolved. Its target
  is withdrawn and its disposition becomes `staleTarget`, with the former semantic symbol kept as
  history. Baleyg never rebinds it to a current node by name or position.
- Labels are conservative: over-labelling is acceptable, under-labelling is a defect.

Semantic basis for a fact names the producer and profile, the artifact hash, the document content
hash, and a digest of the build, configuration, dependency and source-set manifest its freshness
decisions depend on.

## Revision compatibility

With a watcher, revisions advance whenever an agent saves, and the current answer cannot reconstruct
what an answer at an older revision depended on. Compatibility is therefore limited to the one case
where it is provably safe:

| Operation | A pin to an older revision `R` is honoured when |
| --- | --- |
| Read cached source of one path | That path's content hash is unchanged in `(R, N]` |
| Every other operation | Never |

- Each revision records the paths it changed; the log is retained for a bounded window of recent
  revisions. A pin outside it, or from another generation, always conflicts.
- A conflict carries the current basis so the client can re-query.
- Only the current graph is retained. Compatibility never serves data from an older revision.
- Broader compatibility needs tombstones, predicate-aware change records, and unchanged semantic and
  freshness dependencies for every returned field; that is later work.

## Cleanup

- Removing a checkout removes its in-tree index. Processes are torn down by their clients.
- **Automatic garbage collection covers derived state only:** fact-cache entries unreferenced by any
  surviving index, fallback indexes whose workspace root no longer exists, and stale lock files. It
  runs from `baleyg gc` or opportunistically at startup, under the cache's exclusive lock, skipping
  anything currently locked.
- **Durable state is never removed automatically.** A workspace whose last root is gone may simply
  have moved and not been reopened yet. `baleyg gc --report` lists such workspaces with their last
  root. Deleting one requires an explicit `baleyg forget <uuid>`, which shows what will be deleted,
  including ledgers, and asks for confirmation.
- A machine-wide limit on concurrent index jobs, implemented as a few lock slots in the per-user cache
  directory, bounds bursts across many worktrees.

## Threat and resource boundary

- stdio has no network surface: no port, no Host/Origin handling, no DNS-rebinding exposure, and other
  OS users cannot connect.
- The boundary is the OS user. Any process running as that user can launch `baleyg mcp` or read the
  index file directly, just as it can read the source. MCP tools add bounded, typed access, not
  isolation. A hostile-agent threat model needs a separate OS account or sandbox.
- Safe-open rules stop a hostile checkout from redirecting Baleyg's writes through symlinks or links;
  they do not defend against another process running as the same user.
- Configuring `baleyg mcp` for an agent approves disclosure of the whole indexed checkout to that agent
  and its provider. There is no per-path scope and no grant.
- Per-call limits bound each request. They do not bound aggregate resource use by many MCP processes
  running as the same user; the machine-wide index-job limit bounds indexing only.
- Repository text is untrusted data, never an instruction or a permission.

## Superseded

This topology replaces owner-issued grants, limited principals, budgets, enrollment identity
latching, grant handoff files and the HTTP tool routes proposed in the earlier pilot documents. Those
documents remain as history; see [the MCP contract](mcp-readonly-pilot-contract.md) for what
survives. The [multi-project viability review](multi-project-viability.md) is still accurate about the
current code, but its one-daemon registry recommendation is not the chosen agent topology.
