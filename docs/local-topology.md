# Local index and agent topology

Status: **PROPOSED DESIGN — not implemented**. This document supersedes the one-daemon,
owner-grant topology of the earlier MCP pilot. Stage 1 of the semantic-index program
([#8](https://github.com/jasoncarreira/baleyg/issues/8)) ratifies it as contract text. Paths,
commands and fields below are proposals, not claims about the current CLI or store.

## Problem

Coding agents work in many checkouts at once: the main checkout, several Git worktrees, and
Feature Factory sandboxes that are created and deleted routinely. Each needs evidence about the
source it is actually editing, not about `main`. The current store binds one state directory to one
canonical root, keeps it outside the repository, has no file watcher, and publishes by replacing
the whole graph. One long-running daemon per checkout would need port assignment, discovery,
supervision and explicit teardown, and an unauthenticated loopback listener would let any local
process read any checkout's cached source.

## Decisions

1. **One index per checkout, inside the checkout.** Each workspace root has its index at
   `<root>/.baleyg/index.db`.
2. **A shared per-file fact cache** makes a new checkout cheap to index.
3. **Agents reach the index through `baleyg mcp`, a stdio MCP server** that the agent client
   launches and tears down. There is no network listener for agents.
4. **Exactly one writer per index**, chosen by an exclusive lock that any Baleyg process may hold.
5. **The writer keeps syntax evidence current with a file watcher.** Native extraction is the only
   work that runs automatically; semantic producers stay explicit owner actions.

## Index placement

- The index lives at `<root>/.baleyg/index.db`. Baleyg creates `<root>/.baleyg/.gitignore`
  containing `*` when it creates the directory, so Git, ripgrep and agent searches skip it without
  any change to the repository's own ignore rules. The indexer's walker already excludes `.baleyg`.
- Everything under `.baleyg/` is **disposable**. It may be removed by `git clean -fdx`, copied by
  backup or sync tools, or included in a `docker build .` context (`.dockerignore` does not read
  `.gitignore`). Losing it costs a rebuild and nothing else.
- **Out-of-tree state is unchanged in kind:** browser/daemon tokens, Jev and ACP ledgers, and
  durable saved views and annotations stay in private per-user state outside every workspace root.
  They must never be written under `.baleyg/`.
- `index.db` records a random `indexGeneration` when it is created. Evidence basis is
  `{indexGeneration, indexRevision}`. Deleting and recreating the index produces a new generation,
  so an old revision pin can never match a rebuilt index even when the numbers coincide.
- A moved checkout carries its index with it. Store binding verifies relocation (same generation,
  matching recorded file hashes) instead of refusing a changed root.
- If the filesystem cannot support SQLite WAL locking and shared memory (some network mounts and
  container bind mounts), Baleyg falls back to an out-of-tree index for that root and reports it.
- This reverses the current default (state outside the repository; `tests/cli.rs` asserts that no
  `.baleyg` directory is created). The existing explicit `--state-dir` override keeps working.

## Shared fact cache

- Native extraction output is cached per file, keyed by `(language, extractorVersion, contentHash)`.
  Extraction is already per file; only the resolution pass is global.
- In a Git checkout the cache lives in `$(git rev-parse --git-common-dir)/baleyg/`, which every
  worktree of the repository shares and Git never tracks. Outside Git it falls back to a private
  per-user cache directory.
- Indexing a checkout: list path/hash pairs (`git ls-files -s` plus `git status` for dirty and
  untracked files; size/mtime-guarded hashing without Git), extract only hashes the cache has not
  seen, assemble, resolve, and publish. A new worktree of an indexed repository parses only the files
  its branch changed.
- Semantic artifacts are cached by their exact basis in the same place, so a checkout whose
  documents match an indexed one can reuse that evidence with correct basis labels.

## Agent access: `baleyg mcp`

- The agent client starts `baleyg mcp` as a child process. It speaks MCP over stdin/stdout only.
  Stdout carries only protocol messages; diagnostics go to stderr.
- The process resolves its workspace by walking up from its working directory to the nearest
  `.baleyg/` directory or workspace marker, and serves exactly that workspace for its lifetime. No
  tool argument selects another workspace.
- It exits on stdin end-of-file and when its parent dies (Linux `PR_SET_PDEATHSIG`, macOS kqueue
  `NOTE_EXIT` on the parent). It never daemonizes and never outlives its client. If the client is
  killed, the kernel closes the pipe and the server still exits.
- Any number of `baleyg mcp` processes, from different agent sessions, may serve one checkout. All
  of them read committed SQLite snapshots.
- The [MCP contract](mcp-readonly-pilot-contract.md) defines tools, bounds and errors.

## Single writer

- Every process that may publish to an index, including `baleyg mcp` and the existing browser
  daemon, competes for an exclusive `flock` on the index directory. The holder is the writer: it
  runs the watcher and publishes. Everyone else only reads.
- The kernel releases the lock when the holder exits or dies, so there are no stale lock files.
  When a reader acquires a released lock it becomes the writer and first performs a catch-up
  rescan, which is cheap because unchanged hashes hit the fact cache.
- Explicit index requests from the browser or CLI are routed to the writer or wait for the lock;
  two writers never publish concurrently.

## Real-time native refresh

- The writer watches the workspace with the `notify` crate (FSEvents, inotify), filtered by the
  indexer's ignore rules and symlink policy. `.baleyg/` and `.git/` are always excluded; otherwise the
  index's own WAL writes would trigger republishing in a loop.
- Events are debounced (about 100–300 ms) and coalesced per path. A path whose content hash did not
  change is ignored. Changed files are re-extracted, or served from the fact cache if their new hash
  has been seen.
- Publication is a per-path delta in one transaction: replace the rows of changed and deleted
  paths, re-resolve bindings that referenced symbols those paths added, removed or renamed, update
  derived projections, record the changed paths, and advance the revision.
- Watcher overflow, lost-event notifications, exhausted watch limits, and bulk changes such as a
  checkout or rebase fall back to a full rescan.
- A partially written file may be captured mid-save; tree-sitter recovers and marks it with a parse
  error, and the next event corrects it. Source capture is not atomic, as today.
- Semantic evidence is never refreshed by the watcher. A changed file keeps syntax-only evidence.
  Files whose semantic facts reference its symbols are labelled stale with their basis. A change to a
  build or configuration input invalidates the whole semantic basis of the affected source set.
  Producers run only through the owner workflow.

## Revision churn

With a watcher, revisions advance whenever an agent saves. Rejecting every stale pin would make
most follow-up calls fail. Proposed rule, for Stage 1 to ratify:

- Each revision records the paths it changed. Baleyg retains that log for a bounded window of recent
  revisions.
- An evidence call with `expectedRevision = R` is answered from the current revision `N`. The answer
  records every path it read. If no path in the answer changed in `(R, N]`, the call succeeds with
  basis `N` and reports that it is compatible with `R`. Otherwise it fails with `revision_conflict`,
  carrying `N`, so the client can re-query.
- A pin older than the retained window, or from another `indexGeneration`, always conflicts.
- Only the current graph is retained. The rule never serves data from an older revision; it only
  decides whether current data is a valid answer to an older question.

## Cleanup

- Removing a checkout removes its index. Processes are torn down by their clients.
- The shared fact cache and any out-of-tree per-workspace state are garbage-collected by
  mark-and-sweep against surviving workspaces, run by `baleyg gc` or opportunistically at startup,
  skipping anything currently locked.
- A machine-wide limit on concurrent index jobs, implemented as a few lock slots in the shared cache
  directory, bounds bursts across many worktrees without a coordinating daemon.

## Threat boundary

- stdio has no network surface: no port, no Host/Origin handling, no DNS-rebinding exposure, and
  other OS users cannot connect. That is stronger than an unauthenticated loopback listener.
- The boundary is the OS user. Any process running as that user can launch `baleyg mcp` or read
  `.baleyg/index.db` directly, just as it can read the source. MCP tools add bounded, typed access,
  not isolation. A hostile-agent threat model needs a separate OS account or sandbox.
- Configuring `baleyg mcp` for an agent approves disclosure of the whole indexed checkout to that
  agent and whatever provider it uses. There is no per-path scope and no grant.
- Repository text is untrusted data, never an instruction or a permission.

## Superseded

This topology replaces owner-issued grants, limited principals, budgets, enrollment identity
latching, grant handoff files and the HTTP tool routes proposed in the earlier pilot documents. Those
documents remain as history; see [the MCP contract](mcp-readonly-pilot-contract.md) for what
survives. The [multi-project viability review](multi-project-viability.md) is still accurate about
the current code, but its one-daemon registry recommendation is not the chosen agent topology.
