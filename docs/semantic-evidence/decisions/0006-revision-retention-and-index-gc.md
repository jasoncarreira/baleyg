# Decision 0006: revision retention, automatic index deletion and unchanged requests

- **Status:** proposed, pending explicit owner approval of the 15-minute supersession grace, no reader-renewed lease, and whole-index deletion invalidating its pins; ratified when this record merges. It adds revision retention to `../../local-topology.md`, confirms that document's existing automatic GC rule, and governs #16.
- **Scope:** which published revisions stay readable, when a whole derived index may be deleted automatically, and how an explicit indexing request with no source change publishes. Pin identity (`{indexGeneration, indexRevision}`), #67's storage model and Decision 0005 are unchanged.
- **Compatibility:** none needed (pre-release).

## Background

#67 retains **every** revision of the active generation and ships the release and GC mechanism (`release_revision`, `collect_unreferenced`), but no policy. Each revision carries a full manifest, one row per document. With #16's watcher publishing on every save, retaining everything grows without bound. Storage then grows with the number of publications times the number of documents, regardless of whether anyone still reads the old revisions.

A derived index is a disposable cache: deleting it only costs a rebuild. An **old revision**, however, can't be rebuilt, because a rebuild indexes the source as it is now. Retention therefore exists only so that a reader working at a pin isn't interrupted mid-task by the next publication. It is not a history feature.

## Decision

### 1. Revision retention: head plus a short supersession grace

- **What's retained:** the **current head**, plus every revision that stopped being head **less than 15 minutes ago**. A revision's supersession time is the publication time of the revision that replaced it.
- **Release:** once a non-head revision is past the 15-minute grace, the leader releases it and collects unreferenced document versions and projections. GC always runs **outside** the publication transaction, and never removes anything a retained revision references.
- **Expired pin:** a pinned read or write at a released revision fails with a typed conflict (pin expired or released). It **never** silently moves to a newer revision. The client re-reads at head.
- **Readers write nothing.** Retention needs no lease renewal or other reader-side writes, so the T06 rule that followers never write index or request state to read still holds.
- **Saved views and notes.** Loading one at an expired pin **conflicts**, like any other pinned read. It never falls forward silently. The caller may then explicitly request head and reattach the item there through its durable anchors (#65). #97 shows when a result comes from an older revision than head.
- **The grace is a policy constant.** Changing it later is an owner decision, not a format change.

Durable per-reader pin leases were considered and rejected for now. Renewing a lease on every read would make readers write shared state, against T06. If longer-lived pins are needed later, a separate decision must define where leases are stored and why readers may write them.

### 2. Automatic deletion of whole derived indexes

- **Eligible:** a derived index directory whose recorded root is **missing or replaced**, or which has **not been opened for 30 days**. This is the existing `local-topology.md` rule, unchanged; this decision confirms it and adds the conditions below.
- **Conditions, all required:**
  - fresh checks of the exact schema, root identity and recorded age;
  - a verified **exclusive, non-blocking** use lock;
  - skip anything live, busy, with a hot journal, unknown or unreadable.
- **Never deleted automatically:** durable records, UUID markers, configured token and ledger trees, and legacy state.
- **Pins:** deleting an eligible index invalidates every pin of its generation, including revisions that would otherwise be retained. The retention rule in §1 protects revisions **within a live index**; it doesn't keep an eligible index from being deleted. Reopening the workspace creates a new generation, and old pins conflict.
- **Disk space:** the deleting process must not keep descriptors open on the deleted files, or the space isn't reclaimed. #16 must reconcile this with #67's retained SQLite check-handle cache.
- `gc --report` stays read-only. Its "eligible" never authorizes deletion by itself.

### 3. Explicit requests with no source change

- After its normal FIFO claim, an explicit CLI or browser indexing request whose capture shows no change uses the **guarded unchanged-publication path**, as unchanged leader `serve` does since #67.
  - It freshly hashes every admitted source.
  - It checks the same generation, the stored source bytes, the producer binding and executable, the extraction context, options, capture inputs and the projection links, with the CAS and `data_version` fences.
  - It publishes a new revision header and manifest, then acknowledges the request as done.
- A source change, or a changed option or input, takes #67's local or full path instead. Executable drift requires **full fresh measurement** (Decision 0005), never the local path.

## Consequences

- **Storage:** bounded by the head plus the last 15 minutes of publications, independent of how long a workspace has been in use. That keeps steady storage near a single revision.
- **Long agent sessions:** a session that holds a pin for more than 15 minutes after the next publication gets a typed conflict and must re-read at head. Agents and MCP clients treat that as normal.
- **Unused workspaces** lose their index after 30 days unopened and pay a cold rebuild the next time they're opened (about 70 s medium, about 11 min large, extrapolated).
- **No-op index requests** become fast, about 0.35 s on medium instead of a full re-measure.

## Implementation impact (#16)

- Record supersession time (for example from the next revision's publication time), and release revisions past the grace. Schedule `release_revision` and `collect_unreferenced` outside publication: after publications and periodically.
- Add a typed expired-pin conflict, and test that it never moves to a newer revision.
- Implement guarded automatic deletion per §2, using the existing 30-day `GC_AGE_SECONDS` (`src/store/topology.rs`).
- Route unchanged explicit requests through the unchanged-publication path after their FIFO claim.
- Release retained SQLite check handles for deleted or recreated files (see #16's scope).
