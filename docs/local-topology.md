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

1. **One index per checkout, normally inside the checkout** at `<root>/.baleyg/index.db`, opened
   under strict safe-open rules.
2. **A shared, path-neutral fact cache** makes a new checkout cheap to index.
3. **Agents reach the index through `baleyg mcp`, a stdio MCP server** that the agent client
   launches and tears down. There is no network listener for agents.
4. **One watcher leader per index; publication by short, compare-and-swap transactions** that any
   Baleyg process may run.
5. **Native extraction is the only automatic work.** Semantic producers stay explicit owner actions.

## Storage layout

| State | Location | Lifetime |
| --- | --- | --- |
| Index (derived graph, cached source, change log) | `<root>/.baleyg/index.db`, or the fallback location | Disposable; rebuildable |
| Locator record | Per-user state: `<state>/workspaces/<root-key>/locator.json` | Small; derived |
| Durable data (saved views, annotations, future artifacts) | Per-user state: `<state>/workspaces/<root-key>/workspace.db` | Durable; never removed automatically |
| Secrets and ledgers (daemon token, Jev/ACP ledgers) | Per-user state, outside every workspace root | Durable; never removed automatically |
| Shared fact cache | `$(git rev-parse --git-common-dir)/baleyg/facts/`, else per-user cache | Derived; garbage-collected |

`<state>` is the existing per-user application-data directory. `<root-key>` is the existing hash of
the canonical workspace root. Nothing durable or secret is ever written under `.baleyg/`: it may be
removed by `git clean -fdx`, copied by backup or sync tools, or included in a `docker build .`
context (`.dockerignore` does not read `.gitignore`).

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
- Refuse any database or cache file with more than one hard link.
- Create `.baleyg/`, `.baleyg/.gitignore` (containing `*`) and `index.db` exclusively, no-follow.
- If `.baleyg` is tracked by Git, refuse to use it and report why.
- A refusal falls back to the out-of-tree index location (below), never to a weaker check.

The same rules apply to the fact cache directory inside the Git common directory.

### Locator and fallback

All processes for one workspace must choose the same index. The **locator record** in per-user state
is the single source of truth:

```json
{ "schemaVersion": 1, "placement": "inTree", "indexPath": "<root>/.baleyg/index.db" }
```

- A process resolves the canonical root, computes `<root-key>`, and reads the locator under a shared
  lock on its directory. If the locator is absent, the first process takes the exclusive lock,
  chooses the placement, writes the locator atomically, and releases the lock.
- Placement is `inTree` unless safe-open fails or the filesystem cannot support SQLite WAL locking
  and shared memory (some network mounts and container bind mounts). Then placement is `fallback`,
  with the index at `<state>/workspaces/<root-key>/index.db`.
- Placement changes only through an explicit command, which rotates the index generation.
- `--state-dir DIR` keeps its meaning as the location of durable and secret state. It must be
  outside the workspace root. A new `--index-dir DIR` overrides the index location only.

### Migrating existing state directories

Some existing setups pass an in-tree state directory, for example
`--state-dir .baleyg/native-smoke` in `docs/workspace-index-fix.md`, `docs/live-jev.md` and
`docs/browser-token.md`. Those directories hold a token, ledgers and `workspace.db`, which the new
rules forbid under `.baleyg/`. On first start with the new rules:

- An in-tree `--state-dir` is refused with a message naming `baleyg migrate-state`.
- `baleyg migrate-state` moves `workspace.db`, the token and ledgers to per-user state with their
  existing permission checks, verifies them, and removes the originals. The old `cache.db` is
  discarded; the index rebuilds.
- Ledgers keep their workspace binding and caps. Migration never resets or refunds an allowance.

## Index identity

Evidence basis is `{indexGeneration, indexRevision}`. A random generation alone does not detect
copies, so the generation is bound to the database file's identity:

- The index stores `{indexGeneration, boundDevice, boundInode}` for `index.db`.
- On open, if the file's actual device and inode differ from the stored binding, the file is a copy,
  restore or replacement. The next publisher rotates the generation and rewrites the binding before
  publishing anything. Until then, readers report `store_unavailable` for evidence tools.
- A rename within one filesystem preserves device and inode, so a moved checkout keeps its
  generation. The store then verifies the new root by its recorded file hashes instead of refusing
  it.
- At the start of every read and write transaction, a process compares `fstat` of its open database
  against `lstat` of the locator's path. If the path now names a different file, or the open file
  has been unlinked (link count 0, as after a live `git clean -fdx`), the process closes its handle
  and reopens through the locator. Pins from the old generation then conflict. A writer never
  publishes into an unlinked file.
- Limits: an in-place restore that preserves the inode, or a swap away and back between checks, is
  not detected. External restore while processes are running is unsupported; stop them first.

This is deliberately lighter than the earlier pilot's enrollment guard. Each `baleyg mcp` process
lives only as long as one agent session, so a missed replacement has a bounded lifetime.

## Shared fact cache

Current syntax IDs embed the path (`syntax:<path>:<hash>:<start>:<kind>`), and diagnostics carry
paths too. Identical bytes at two paths must not share facts that name the wrong path.

- The cache stores a **path-neutral extraction record**: declarations, call sites, regions and
  diagnostics addressed by byte offsets and file-local keys, with no workspace path. Assembly binds
  the record to a path and derives the existing path-bearing IDs deterministically. A file moved by
  `git mv` keeps its cache hit.
- Key: `(language, extractorVersion, extractionContextDigest, contentHash)`. The context digest
  covers every input to extraction other than file bytes, such as the file extension that selects
  the grammar and any dialect setting. Anything that could change the record belongs in the key.
- Entries are written to a temporary file, fsynced, and renamed into place. Each entry carries a
  checksum of its payload; a reader that finds a mismatch discards the entry and re-extracts.
  Concurrent writers of the same key are harmless because the content is identical.
- Indexing holds a shared lock on the cache for its duration. Garbage collection takes the exclusive
  lock, so it never deletes an entry an in-progress job is about to use.
- Semantic artifacts are cached by exact basis in the same place.

A checkout indexes by listing path/hash pairs (`git ls-files -s` plus `git status` for dirty and
untracked files; size/mtime-guarded hashing without Git), extracting only records the cache lacks,
assembling, resolving and publishing. A new worktree of an indexed repository parses only the files
its branch changed.

## Agent access: `baleyg mcp`

- The agent client starts `baleyg mcp` as a child process. It speaks MCP over stdin/stdout only.
  Stdout carries only protocol messages; diagnostics go to stderr.
- The process resolves its workspace by walking up from its working directory to the nearest
  workspace root and serves exactly that workspace for its lifetime. No tool argument selects another
  workspace.
- It exits on stdin end-of-file and when its parent dies (Linux `PR_SET_PDEATHSIG`, macOS kqueue
  `NOTE_EXIT` on the parent). It never daemonizes and never outlives its client.
- **Launch-time indexing.** If no revision is published, the process schedules a native index at
  launch on a background thread. This is part of process start, not a side effect of any tool call.
  MCP initialization and `tools/list` answer immediately; describe reports progress; evidence tools
  return `no_published_index` until the first revision lands.
- The [MCP contract](mcp-readonly-pilot-contract.md) defines tools, bounds and errors.

## Watcher leadership and publication

Two separate mechanisms, so that no long-lived process can block explicit work:

**Watcher leadership.** An exclusive `flock` on `.baleyg/leader.lock` (or the fallback directory's)
decides which process runs the file watcher. It is held for the leader's lifetime and released by the
kernel on exit or crash. Holding it grants no publication rights and blocks nothing else.

**Publication.** Any Baleyg process may publish: the watcher leader, an explicit CLI or browser
index request, or a semantic artifact import. Each job:

1. Opens a short SQLite write transaction to take a **job epoch** from a monotonic counter in the
   index, then commits.
2. Observes files and extracts outside any transaction. Each observed path records the epoch of the
   job that observed it.
3. Publishes in one short `BEGIN IMMEDIATE` transaction that replaces a path's rows only if its job
   epoch is greater than the epoch of the currently published observation of that path. Rows that
   would be overwritten by an older observation are skipped. Artifact imports additionally require
   each document's content hash to match the published one, and reject late results.
4. Re-resolves affected bindings (below), appends to the change log and advances the revision in the
   same transaction.

Because every change produces a later watcher event, the most recent observation of each path wins
without any job waiting for another. SQLite serializes the short publish transactions.

**Watcher sequencing.** A new leader starts watching first and buffers events, then runs a catch-up
scan, then applies buffered events. Watcher overflow, lost-event notifications, exhausted watch
limits and bulk changes such as checkout or rebase trigger a full rescan. Because some platforms lose
events silently, the leader also reconciles periodically (proposed every five minutes and after
system wake) with a size/mtime scan. `.baleyg/` and `.git/` are always excluded, so the index's own
WAL writes never trigger republishing.

A partially written file may be observed mid-save; tree-sitter recovers and marks a parse error, and
the next event corrects it. Source capture is not atomic, as today.

## Re-resolution

Adding a declaration can resolve or change an unchanged call that never referenced it. Resolution
therefore records what it **looked up**, not only what it found:

- For each binding, the publish step records its **resolution dependencies**: the scope/name keys it
  searched, including searches that found nothing or found several candidates, and any workspace-wide
  lookup it relied on.
- When a delta adds, removes or renames declarations, it computes the affected scope/name keys and
  re-resolves every binding that depends on any of them, whether resolved, unresolved or ambiguous.
- Import, alias and wildcard resolution record the module and scope keys they consulted.
- A change to any input that affects resolution globally re-resolves the whole workspace.

## Semantic freshness under native refresh

The watcher never runs a producer. When native refresh publishes a change:

- The changed document's semantic facts are withdrawn; it has syntax evidence only until an artifact
  for its new content is imported.
- Documents that reference the changed document's symbols, that resolved names through an import or
  wildcard reaching it, or whose unresolved and ambiguous references could now resolve to a
  declaration it added, are labelled **possibly stale**, with their basis and the reason.
- If the changed document's exported declarations changed, importers of it are labelled possibly stale
  transitively within the source set, because inferred types can propagate beyond one hop.
- A change to a build or configuration input marks the whole semantic basis of the affected source set
  stale.
- Labels are conservative: over-labelling is acceptable, under-labelling is a defect.

## Revision compatibility

With a watcher, revisions advance whenever an agent saves. The current answer cannot reconstruct
what an answer at an older revision would have depended on, so compatibility is decided per
operation, conservatively:

| Operation | A pin to an older revision `R` is honoured when |
| --- | --- |
| Read cached source of one path | That path's content hash is unchanged in `(R, N]` |
| Inspect one declaration | The declaration exists and its path is unchanged in `(R, N]` |
| Inspect outgoing calls | Never; targets depend on workspace-wide resolution |
| Find symbols, and any ranked or global query | Never |
| Any `not_found` or other negative result | Never |

- Each revision records the paths it changed. The log is retained for a bounded window of recent
  revisions; a pin outside it, or from another generation, always conflicts.
- A conflict carries the current basis so the client can re-query.
- Only the current graph is retained. Compatibility never serves data from an older revision; it only
  accepts current data as a valid answer to an older pin.
- Broader compatibility needs tombstones and predicate-aware change records; that is later work.

## Cleanup

- Removing a checkout removes its in-tree index. Processes are torn down by their clients.
- **Automatic garbage collection covers derived state only:** fact-cache entries unreferenced by any
  surviving index, fallback indexes and locator records whose workspace root no longer exists, and
  stale lock and lease files. It runs from `baleyg gc` or opportunistically at startup, under the
  cache's exclusive lock, skipping anything currently locked.
- **Durable state is never removed automatically.** `baleyg gc --report` lists durable state whose
  workspace root is gone. Deleting it requires an explicit `baleyg forget <root-key>`, which shows what
  will be deleted, including ledgers, and asks for confirmation.
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
